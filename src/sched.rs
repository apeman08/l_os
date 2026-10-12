//! Process table, kernel-stack context switching and a round-robin scheduler.
//!
//! Model (single CPU):
//! * Every process owns a kernel stack and an address space. Switching means
//!   saving callee-saved registers on the current kernel stack, loading the
//!   target's stack pointer and CR3, and restoring.
//! * The "boot context" (whoever called `usermode::run_all`) waits until no
//!   process is left; it is never preempted.
//! * All scheduler state is touched with interrupts disabled (syscall/ISR
//!   gates clear IF; boot code uses `without_interrupts`). No lock guard is
//!   ever held across a context switch.
//! * Preemption happens only when the timer interrupts *user* code; kernel
//!   code is never preempted.

use crate::{gdt, memory};
use alloc::{boxed::Box, vec, vec::Vec};
use core::arch::global_asm;
use core::sync::atomic::{AtomicU64, Ordering};
use l_core::{handles::HandleTable, syscall::Outcome};
use spin::Mutex;
use x86_64::{
    registers::control::{Cr3, Cr3Flags},
    structures::paging::PhysFrame,
};

pub const MAX_PROCS: usize = 8;
const KERNEL_STACK_SIZE: usize = 32 * 1024;

global_asm!(
    r#"
    .section .text
    // void switch_context(u64 *save_rsp /*rdi*/, u64 new_rsp /*rsi*/)
    // Saves callee-saved registers on the current stack, stores rsp in
    // *save_rsp, then continues on the stack at new_rsp.
    .global switch_context
switch_context:
    push rbx
    push rbp
    push r12
    push r13
    push r14
    push r15
    mov qword ptr [rdi], rsp
    mov rsp, rsi
    pop r15
    pop r14
    pop r13
    pop r12
    pop rbp
    pop rbx
    ret

    // First run of a process: switch_context "returns" here with rsp pointing
    // at a prepared iretq frame (rip, cs, rflags, rsp, ss).
    .global user_trampoline
user_trampoline:
    iretq
"#
);

unsafe extern "C" {
    fn switch_context(save_rsp: *mut u64, new_rsp: u64);
    fn user_trampoline();
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Ready,
    Running,
    /// Waiting for a message on this endpoint.
    BlockedRecv(u16),
    Zombie,
}

pub struct Process {
    pub id: u32,
    handles: HandleTable,
    state: State,
    pub(crate) l4: PhysFrame,
    kstack: Box<[u8]>,
    saved_rsp: u64,
    budget_ticks: u64,
    used_ticks: u64,
    outcome: Option<Outcome>,
    finished_tick: u64,
}

impl Process {
    /// Create a process that will start executing in ring 3 at `entry` with
    /// stack pointer `user_sp` when first scheduled.
    pub fn new(id: u32, handles: HandleTable, budget_ticks: u64, l4: PhysFrame, entry: u64, user_sp: u64) -> Box<Process> {
        let mut kstack = vec![0u8; KERNEL_STACK_SIZE].into_boxed_slice();
        let (user_cs, user_ss) = gdt::user_selectors();
        let top = (kstack.as_mut_ptr() as u64 + KERNEL_STACK_SIZE as u64) & !0xf;

        // Layout (high -> low): iretq frame, return address, six saved regs.
        let words: [u64; 12] = [
            0, 0, 0, 0, 0, 0,                // r15 r14 r13 r12 rbp rbx (popped first)
            user_trampoline as *const () as u64, // `ret` target
            entry,                           // iretq: rip
            user_cs.0 as u64,                // cs
            0x202,                           // rflags: IF=1, IOPL=0
            user_sp,                         // rsp
            user_ss.0 as u64,                // ss
        ];
        let saved_rsp = top - (words.len() as u64) * 8;
        // SAFETY: [saved_rsp, top) lies inside `kstack`, which we own.
        unsafe {
            let base = saved_rsp as *mut u64;
            for (i, w) in words.iter().enumerate() {
                base.add(i).write(*w);
            }
        }
        Box::new(Process {
            id,
            handles,
            state: State::Ready,
            l4,
            kstack,
            saved_rsp,
            budget_ticks,
            used_ticks: 0,
            outcome: None,
            finished_tick: 0,
        })
    }

    pub fn outcome(&self) -> Option<Outcome> {
        self.outcome
    }

    pub fn finished_tick(&self) -> u64 {
        self.finished_tick
    }

    fn kernel_stack_top(&self) -> u64 {
        (self.kstack.as_ptr() as u64 + KERNEL_STACK_SIZE as u64) & !0xf
    }
}

struct Scheduler {
    procs: Vec<Box<Process>>,
    current: Option<usize>,
    boot_waiting: bool,
}

#[derive(Clone, Copy)]
enum Target {
    Boot,
    Proc { rsp: u64, l4: PhysFrame, kstack_top: u64 },
}

impl Scheduler {
    /// First Ready process after index `after` in round-robin order (wraps,
    /// and may return `after` itself if it is the only Ready one).
    fn select_next(&self, after: usize) -> Option<usize> {
        let n = self.procs.len();
        (1..=n).map(|k| (after + k) % n).find(|&i| self.procs[i].state == State::Ready)
    }

    /// Nothing can run any more. Processes still blocked will never be woken
    /// (the only wake-ups come from other processes), so end them.
    fn kill_blocked(&mut self) {
        let now = crate::time::ticks();
        for p in self.procs.iter_mut().filter(|p| matches!(p.state, State::BlockedRecv(_))) {
            p.state = State::Zombie;
            p.outcome = Some(Outcome::Deadlock);
            p.finished_tick = now;
        }
    }

    /// Choose what runs after the current process stopped being runnable.
    fn next_after_stop(&mut self, cur: usize) -> Target {
        match self.select_next(cur) {
            Some(next) => {
                self.procs[next].state = State::Running;
                self.current = Some(next);
                self.target(next)
            }
            None => {
                self.kill_blocked();
                assert!(self.boot_waiting, "last process stopped but nobody is waiting");
                self.current = None;
                Target::Boot
            }
        }
    }

    fn target(&self, idx: usize) -> Target {
        let p = &self.procs[idx];
        Target::Proc { rsp: p.saved_rsp, l4: p.l4, kstack_top: p.kernel_stack_top() }
    }
}

static SCHED: Mutex<Scheduler> =
    Mutex::new(Scheduler { procs: Vec::new(), current: None, boot_waiting: false });

/// Saved stack pointer of the boot context while it waits.
static BOOT_RSP: AtomicU64 = AtomicU64::new(0);

/// Switch to `target`, saving the current stack pointer to `save`.
///
/// # Safety
/// Interrupts must be disabled, no scheduler lock may be held, `save` must
/// stay valid until the context is resumed, and `target` must describe a
/// context whose stack/tables are alive.
unsafe fn perform_switch(save: *mut u64, target: Target) {
    let new_rsp = match target {
        Target::Boot => {
            // SAFETY (Cr3): the kernel's own tables map all kernel code/stacks.
            unsafe { Cr3::write(memory::kernel_l4(), Cr3Flags::empty()) };
            BOOT_RSP.load(Ordering::SeqCst)
        }
        Target::Proc { rsp, l4, kstack_top } => {
            gdt::set_kernel_stack(kstack_top);
            // SAFETY (Cr3): process tables share every kernel mapping, so the
            // code and the stack we are executing on stay mapped.
            unsafe { Cr3::write(l4, Cr3Flags::empty()) };
            rsp
        }
    };
    // SAFETY: forwarded contract; the new stack holds a context saved by
    // switch_context or prepared by Process::new.
    unsafe { switch_context(save, new_rsp) };
}

/// Install processes and run them until none is left. Called by the boot
/// context with interrupts disabled; returns when all are zombies.
pub fn run_until_idle(procs: Vec<Box<Process>>) -> Vec<Box<Process>> {
    let target = {
        let mut s = SCHED.lock();
        assert!(s.procs.is_empty() && !s.boot_waiting, "scheduler already active");
        s.procs = procs;
        if s.procs.is_empty() {
            return Vec::new();
        }
        s.boot_waiting = true;
        let n = s.procs.len();
        let first = s.select_next(n - 1).expect("new processes are Ready");
        s.procs[first].state = State::Running;
        s.current = Some(first);
        s.target(first)
    };
    // SAFETY: interrupts are disabled by the caller; BOOT_RSP is a static;
    // the target process was just created.
    unsafe { perform_switch(BOOT_RSP.as_ptr(), target) };
    // Resumed: every process has exited.
    let mut s = SCHED.lock();
    s.boot_waiting = false;
    s.current = None;
    core::mem::take(&mut s.procs)
}

/// Give up the CPU voluntarily (syscall) or because the slice ended (timer).
pub fn yield_current() {
    let (save, target) = {
        let mut s = SCHED.lock();
        let Some(cur) = s.current else { return };
        s.procs[cur].state = State::Ready;
        let next = s.select_next(cur).unwrap_or(cur);
        s.procs[next].state = State::Running;
        if next == cur {
            return;
        }
        s.current = Some(next);
        (&raw mut s.procs[cur].saved_rsp, s.target(next))
    };
    // SAFETY: we are in a syscall/ISR gate (IF=0), the lock guard is dropped,
    // `save` points into a boxed Process that stays in the table until reaped.
    unsafe { perform_switch(save, target) };
}

/// Block the current process until a message may be available on `endpoint`,
/// run something else meanwhile, and return once woken. Callers re-check.
pub fn block_on_recv(endpoint: u16) {
    let (save, target) = {
        let mut s = SCHED.lock();
        let Some(cur) = s.current else { return };
        s.procs[cur].state = State::BlockedRecv(endpoint);
        let target = s.next_after_stop(cur);
        (&raw mut s.procs[cur].saved_rsp, target)
    };
    // SAFETY: as in yield_current. If nothing else could run, the process was
    // turned into a zombie by kill_blocked and is never resumed.
    unsafe { perform_switch(save, target) };
}

/// Make every process waiting on `endpoint` runnable again.
pub fn wake_endpoint(endpoint: u16) {
    for p in SCHED.lock().procs.iter_mut() {
        if p.state == State::BlockedRecv(endpoint) {
            p.state = State::Ready;
        }
    }
}

/// Run `f` on the running process's handle table.
pub fn with_handles<R>(f: impl FnOnce(&mut HandleTable) -> R) -> Option<R> {
    let mut s = SCHED.lock();
    let cur = s.current?;
    Some(f(&mut s.procs[cur].handles))
}

/// End the current process and never return to it.
pub fn exit_current(outcome: Outcome) -> ! {
    let (save, target) = {
        let mut s = SCHED.lock();
        let cur = match s.current {
            Some(c) => c,
            None => panic!("ring-3 event ({:?}) with no user process running", outcome),
        };
        {
            let p = &mut s.procs[cur];
            p.state = State::Zombie;
            p.outcome = Some(outcome);
            p.finished_tick = crate::time::ticks();
        }
        let target = s.next_after_stop(cur);
        (&raw mut s.procs[cur].saved_rsp, target)
    };
    // SAFETY: as in yield_current; the zombie's stack is never resumed, and it
    // is freed only after the boot context regains control.
    unsafe { perform_switch(save, target) };
    unreachable!("a zombie process was resumed");
}

/// Timer hook (after EOI, no locks held). Charges the running process one
/// tick, kills it past its budget, and round-robins to another Ready process.
pub fn on_tick(interrupted_user_code: bool) {
    if !interrupted_user_code {
        return;
    }
    enum Action {
        Nothing,
        Kill,
        Yield,
    }
    let action = {
        let mut s = SCHED.lock();
        let Some(cur) = s.current else { return };
        let p = &mut s.procs[cur];
        p.used_ticks += 1;
        if p.used_ticks >= p.budget_ticks {
            Action::Kill
        } else if s.procs.iter().enumerate().any(|(i, q)| i != cur && q.state == State::Ready) {
            Action::Yield
        } else {
            Action::Nothing
        }
    };
    match action {
        Action::Kill => exit_current(Outcome::Timeout),
        Action::Yield => yield_current(),
        Action::Nothing => {}
    }
}

pub fn process_active() -> bool {
    SCHED.lock().current.is_some()
}

pub fn scheduler_busy() -> bool {
    let s = SCHED.lock();
    s.boot_waiting || !s.procs.is_empty()
}
