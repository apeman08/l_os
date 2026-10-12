//! Process table, kernel-stack context switching and a round-robin scheduler.
//!
//! Model (single CPU):
//! * Every process owns a kernel stack and an address space. Switching means
//!   saving callee-saved registers on the current kernel stack, loading the
//!   target's stack pointer and CR3, and restoring.
//! * The "boot context" (whoever called `usermode::run_all`) waits until no
//!   process can run any more; it is never preempted.
//! * All scheduler state is touched with interrupts disabled (syscall/ISR
//!   gates clear IF; boot code uses `without_interrupts`). No lock guard is
//!   ever held across a context switch.
//! * Preemption happens only when the timer interrupts *user* code; kernel
//!   code is never preempted.
//! * When every process is blocked but some wait for the keyboard, the CPU
//!   idles in `hlt` (on the stack of the process that just blocked) until an
//!   interrupt makes something runnable.

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
const KERNEL_STACK_SIZE: usize = 64 * 1024;

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
    /// Waiting for the child with this pid to end.
    BlockedWait(u32),
    /// Waiting for a keyboard scancode (woken by the keyboard interrupt).
    BlockedKey,
    Zombie,
}

pub struct Process {
    pub id: u32,
    parent: Option<u32>,
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
    pub fn new(
        id: u32,
        parent: Option<u32>,
        handles: HandleTable,
        budget_ticks: u64,
        l4: PhysFrame,
        entry: u64,
        user_sp: u64,
    ) -> Box<Process> {
        let mut kstack = vec![0u8; KERNEL_STACK_SIZE].into_boxed_slice();
        let (user_cs, user_ss) = gdt::user_selectors();
        let top = (kstack.as_mut_ptr() as u64 + KERNEL_STACK_SIZE as u64) & !0xf;

        // Layout (high -> low): iretq frame, return address, six saved regs.
        let words: [u64; 12] = [
            0, 0, 0, 0, 0, 0,                    // r15 r14 r13 r12 rbp rbx (popped first)
            user_trampoline as *const () as u64, // `ret` target
            entry,                               // iretq: rip
            user_cs.0 as u64,                    // cs
            0x202,                               // rflags: IF=1, IOPL=0
            user_sp,                             // rsp
            user_ss.0 as u64,                    // ss
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
            parent,
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
    slots: [Option<Box<Process>>; MAX_PROCS],
    current: Option<usize>,
    boot_waiting: bool,
}

#[derive(Clone, Copy)]
enum Target {
    Boot,
    Proc { rsp: u64, l4: PhysFrame, kstack_top: u64 },
}

enum Stop {
    Switch(Target),
    /// Nothing runnable, but a keyboard interrupt may change that.
    Idle,
}

impl Scheduler {
    fn proc(&self, i: usize) -> &Process {
        self.slots[i].as_deref().expect("scheduler index names a live slot")
    }

    fn proc_mut(&mut self, i: usize) -> &mut Process {
        self.slots[i].as_deref_mut().expect("scheduler index names a live slot")
    }

    /// First Ready process after slot `after` in round-robin order (wraps, and
    /// may return `after` itself if it is the only Ready one).
    fn select_next(&self, after: usize) -> Option<usize> {
        (1..=MAX_PROCS)
            .map(|k| (after + k) % MAX_PROCS)
            .find(|&i| matches!(&self.slots[i], Some(p) if p.state == State::Ready))
    }

    fn any_blocked_on_key(&self) -> bool {
        self.slots.iter().flatten().any(|p| p.state == State::BlockedKey)
    }

    /// Wake parents waiting for a process that has become a zombie.
    fn wake_waiters(&mut self) {
        let zombies: Vec<u32> = self.slots.iter().flatten().filter(|p| p.state == State::Zombie).map(|p| p.id).collect();
        for p in self.slots.iter_mut().flatten() {
            if let State::BlockedWait(pid) = p.state {
                if zombies.contains(&pid) {
                    p.state = State::Ready;
                }
            }
        }
    }

    /// End processes that can never be woken. Receivers go first: ending them
    /// may unblock a parent waiting for them, so waiters are only ended when no
    /// receiver was left. Returns whether anything was ended.
    fn kill_unwakeable(&mut self) -> bool {
        let now = crate::time::ticks();
        let mut killed = false;
        for pass in 0..2 {
            for p in self.slots.iter_mut().flatten() {
                let hit = match (pass, p.state) {
                    (0, State::BlockedRecv(_)) => true,
                    (1, State::BlockedWait(_)) => true,
                    _ => false,
                };
                if hit {
                    p.state = State::Zombie;
                    p.outcome = Some(Outcome::Deadlock);
                    p.finished_tick = now;
                    killed = true;
                }
            }
            if killed {
                break;
            }
        }
        killed
    }

    /// Choose what runs after the current process stopped being runnable.
    fn stop_current(&mut self, cur: usize) -> Stop {
        loop {
            if let Some(next) = self.select_next(cur) {
                self.proc_mut(next).state = State::Running;
                self.current = Some(next);
                return Stop::Switch(self.target(next));
            }
            if self.any_blocked_on_key() {
                return Stop::Idle;
            }
            if self.kill_unwakeable() {
                self.wake_waiters();
                continue; // ending a receiver may have unblocked its parent
            }
            assert!(self.boot_waiting, "nothing can run and nobody is waiting");
            self.current = None;
            return Stop::Switch(Target::Boot);
        }
    }

    fn target(&self, idx: usize) -> Target {
        let p = self.proc(idx);
        Target::Proc { rsp: p.saved_rsp, l4: p.l4, kstack_top: p.kernel_stack_top() }
    }

    fn save_slot(&mut self, idx: usize) -> *mut u64 {
        &raw mut self.proc_mut(idx).saved_rsp
    }
}

static SCHED: Mutex<Scheduler> =
    Mutex::new(Scheduler { slots: [const { None }; MAX_PROCS], current: None, boot_waiting: false });

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

/// With interrupts enabled, halt until some process becomes Ready. Returns the
/// target to switch to, or `None` if the waiting process itself is the one
/// that became runnable (it simply continues).
fn idle_until_runnable(cur: usize) -> Option<Target> {
    use x86_64::instructions::interrupts;
    loop {
        interrupts::enable_and_hlt();
        interrupts::disable();
        let mut s = SCHED.lock();
        if let Some(n) = s.select_next(cur) {
            s.proc_mut(n).state = State::Running;
            s.current = Some(n);
            return if n == cur { None } else { Some(s.target(n)) };
        }
    }
}

/// Install processes and run them until none can run any more. Called by the
/// boot context with interrupts disabled. Returns every process still in the
/// table (zombies), in slot order.
pub fn run_until_idle(procs: Vec<Box<Process>>) -> Vec<Box<Process>> {
    let target = {
        let mut s = SCHED.lock();
        assert!(!s.boot_waiting && s.slots.iter().all(Option::is_none), "scheduler already active");
        assert!(procs.len() <= MAX_PROCS);
        if procs.is_empty() {
            return Vec::new();
        }
        for (i, p) in procs.into_iter().enumerate() {
            s.slots[i] = Some(p);
        }
        s.boot_waiting = true;
        let first = s.select_next(MAX_PROCS - 1).expect("new processes are Ready");
        s.proc_mut(first).state = State::Running;
        s.current = Some(first);
        s.target(first)
    };
    // SAFETY: interrupts are disabled by the caller; BOOT_RSP is a static;
    // the target process was just created.
    unsafe { perform_switch(BOOT_RSP.as_ptr(), target) };
    // Resumed: nothing can run any more.
    let mut s = SCHED.lock();
    s.boot_waiting = false;
    s.current = None;
    s.slots.iter_mut().filter_map(Option::take).collect()
}

/// Run the common "current process stops" path: pick what is next and switch.
fn stop_and_switch(cur: usize, save: *mut u64, stop: Stop) {
    let target = match stop {
        Stop::Switch(t) => Some(t),
        Stop::Idle => idle_until_runnable(cur),
    };
    if let Some(t) = target {
        // SAFETY: syscall/ISR gate (IF=0), no lock guard alive, `save` points
        // into a boxed Process that stays in its slot until reaped.
        unsafe { perform_switch(save, t) };
    }
}

/// Give up the CPU voluntarily (syscall) or because the slice ended (timer).
pub fn yield_current() {
    let (cur, save, stop) = {
        let mut s = SCHED.lock();
        let Some(cur) = s.current else { return };
        s.proc_mut(cur).state = State::Ready;
        let next = s.select_next(cur).unwrap_or(cur);
        s.proc_mut(next).state = State::Running;
        if next == cur {
            return;
        }
        s.current = Some(next);
        let t = s.target(next);
        (cur, s.save_slot(cur), Stop::Switch(t))
    };
    stop_and_switch(cur, save, stop);
}

/// Block the current process in `state`; returns once it was woken and is
/// running again. Callers re-check their condition in a loop.
fn block_current(state: State) {
    let (cur, save, stop) = {
        let mut s = SCHED.lock();
        let Some(cur) = s.current else { return };
        s.proc_mut(cur).state = state;
        let stop = s.stop_current(cur);
        (cur, s.save_slot(cur), stop)
    };
    stop_and_switch(cur, save, stop);
}

/// Wait for a message on `endpoint` (see `block_current`).
pub fn block_on_recv(endpoint: u16) {
    block_current(State::BlockedRecv(endpoint));
}

/// Wait for a keyboard scancode.
pub fn block_on_key() {
    block_current(State::BlockedKey);
}

/// Wait for child `pid` to end.
pub fn block_on_child(pid: u32) {
    block_current(State::BlockedWait(pid));
}

/// Make every process waiting on `endpoint` runnable again.
pub fn wake_endpoint(endpoint: u16) {
    for p in SCHED.lock().slots.iter_mut().flatten() {
        if p.state == State::BlockedRecv(endpoint) {
            p.state = State::Ready;
        }
    }
}

/// Keyboard interrupt hook: wake everything waiting for a key.
pub fn wake_keyboard() {
    for p in SCHED.lock().slots.iter_mut().flatten() {
        if p.state == State::BlockedKey {
            p.state = State::Ready;
        }
    }
}

/// Run `f` on the running process's handle table.
pub fn with_handles<R>(f: impl FnOnce(&mut HandleTable) -> R) -> Option<R> {
    let mut s = SCHED.lock();
    let cur = s.current?;
    Some(f(&mut s.proc_mut(cur).handles))
}

pub fn current_pid() -> Option<u32> {
    let s = SCHED.lock();
    s.current.map(|c| s.proc(c).id)
}

/// CPU ticks the running process may still use.
pub fn current_remaining_budget() -> u64 {
    let s = SCHED.lock();
    s.current.map(|c| s.proc(c).budget_ticks.saturating_sub(s.proc(c).used_ticks)).unwrap_or(0)
}

/// Put a freshly built process into a free slot.
pub fn add_process(p: Box<Process>) -> Result<(), Box<Process>> {
    let mut s = SCHED.lock();
    match s.slots.iter().position(Option::is_none) {
        Some(i) => {
            s.slots[i] = Some(p);
            Ok(())
        }
        None => Err(p),
    }
}

pub enum Reap {
    /// No such process, or it is not a child of the caller.
    NotChild,
    StillRunning,
    /// The child ended; its slot is free again. The caller frees its memory.
    Done(Box<Process>),
}

pub fn reap_child(parent_pid: u32, child_pid: u32) -> Reap {
    let mut s = SCHED.lock();
    let Some(i) = s.slots.iter().position(|p| matches!(p, Some(p) if p.id == child_pid)) else {
        return Reap::NotChild;
    };
    let p = s.proc(i);
    if p.parent != Some(parent_pid) {
        return Reap::NotChild;
    }
    if p.state != State::Zombie {
        return Reap::StillRunning;
    }
    Reap::Done(s.slots[i].take().expect("slot checked above"))
}

/// End the current process and never return to it.
pub fn exit_current(outcome: Outcome) -> ! {
    let (cur, save, stop) = {
        let mut s = SCHED.lock();
        let cur = match s.current {
            Some(c) => c,
            None => panic!("ring-3 event ({:?}) with no user process running", outcome),
        };
        {
            let p = s.proc_mut(cur);
            p.state = State::Zombie;
            p.outcome = Some(outcome);
            p.finished_tick = crate::time::ticks();
        }
        s.wake_waiters();
        let stop = s.stop_current(cur);
        (cur, s.save_slot(cur), stop)
    };
    stop_and_switch(cur, save, stop);
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
        let p = s.proc_mut(cur);
        p.used_ticks += 1;
        if p.used_ticks >= p.budget_ticks {
            Action::Kill
        } else if s.slots.iter().enumerate().any(|(i, q)| i != cur && matches!(q, Some(q) if q.state == State::Ready)) {
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
