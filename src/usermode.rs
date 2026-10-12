//! Ring 3 execution: one user process at a time, entered with `iretq`, left
//! via `int 0x80` (exit), a fault, or a timer-enforced time budget.
//!
//! Current limits (see docs/THREAT_MODEL.md): a single process, no scheduler,
//! frames are never freed (each run leaks a few frames), programs are raw
//! machine code copied into one page. Isolation rests on page tables
//! (supervisor-only kernel pages), ring 3 privilege checks, and syscall
//! argument validation; it has had only the tests in `tests/usermode.rs`.

use crate::memory::{self, Memory};
use core::arch::global_asm;
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use l_core::{caps::Caps, policy::NetPolicy, syscall::Outcome};
use spin::Mutex;
use x86_64::{
    VirtAddr,
    structures::paging::{FrameAllocator, Mapper, Page, PageTableFlags, Size4KiB},
};

pub const USER_CODE_ADDR: u64 = 0x_7000_0000_0000;
pub const USER_STACK_TOP: u64 = 0x_7000_0010_0000;
const USER_STACK_PAGES: u64 = 2;

/// The network policy consulted by `SYS_NET_CHECK`. Do not hold this lock
/// while calling [`run`].
pub static POLICY: Mutex<NetPolicy> = Mutex::new(NetPolicy::new());

static RUNNING: AtomicBool = AtomicBool::new(false);
static IN_USER: AtomicBool = AtomicBool::new(false);
static DEADLINE_TICK: AtomicU64 = AtomicU64::new(0);
static CURRENT_CAPS: AtomicU32 = AtomicU32::new(0);

global_asm!(
    r#"
    .section .text
    // u64 enter_user_mode(rip=rdi, rsp=rsi, cs=rdx, ss=rcx)
    // Saves callee-saved registers and the kernel stack pointer, then drops to
    // ring 3 with IF=1 and IOPL=0. Returns (via return_to_kernel) the Outcome.
    .global enter_user_mode
enter_user_mode:
    push rbx
    push rbp
    push r12
    push r13
    push r14
    push r15
    mov qword ptr [rip + L_KERNEL_RSP], rsp
    push rcx
    push rsi
    push 0x202
    push rdx
    push rdi
    iretq

    // never returns to its caller: unwinds to the caller of enter_user_mode.
    .global return_to_kernel
return_to_kernel:
    mov rsp, qword ptr [rip + L_KERNEL_RSP]
    mov rax, rdi
    pop r15
    pop r14
    pop r13
    pop r12
    pop rbp
    pop rbx
    ret

    // int 0x80 entry. rax = number, rdi/rsi/rdx = args. Result in rax; every
    // other register is preserved.
    .global syscall_entry
syscall_entry:
    push rcx
    push rdx
    push rsi
    push rdi
    push r8
    push r9
    push r10
    push r11
    sub rsp, 8              // 16-byte alignment for the call
    cld
    mov rcx, rdx            // a2
    mov rdx, rsi            // a1
    mov rsi, rdi            // a0
    mov rdi, rax            // number
    call syscall_dispatch
    add rsp, 8
    pop r11
    pop r10
    pop r9
    pop r8
    pop rdi
    pop rsi
    pop rdx
    pop rcx
    iretq

    .section .bss
    .align 8
L_KERNEL_RSP:
    .quad 0
"#
);

unsafe extern "C" {
    fn enter_user_mode(rip: u64, rsp: u64, cs: u64, ss: u64) -> u64;
    fn return_to_kernel(raw_outcome: u64) -> !;
    pub fn syscall_entry();
}

#[derive(Debug, PartialEq, Eq)]
pub enum RunError {
    /// Another user process is already running.
    Busy,
    TooLarge,
    /// Memory manager missing or out of frames.
    NoMemory,
}

pub fn current_caps() -> Caps {
    Caps::from_bits(CURRENT_CAPS.load(Ordering::Relaxed))
}

pub fn user_process_active() -> bool {
    IN_USER.load(Ordering::Relaxed)
}

/// Terminate the running user process from kernel context (fault or exit).
/// Never returns to the interrupted user code.
pub fn terminate_current(outcome: Outcome) -> ! {
    if !IN_USER.load(Ordering::Relaxed) {
        panic!("ring-3 event ({:?}) with no user process running", outcome);
    }
    // SAFETY: IN_USER is only set between `enter_user_mode` and its return, so
    // L_KERNEL_RSP holds the stack pointer saved by that call and the frame it
    // points to is still live.
    unsafe { return_to_kernel(outcome.to_raw()) }
}

/// Called by the timer ISR after EOI. Kills the process if it overran.
pub fn enforce_deadline(interrupted_from_user: bool) {
    if interrupted_from_user
        && IN_USER.load(Ordering::Relaxed)
        && crate::time::ticks() >= DEADLINE_TICK.load(Ordering::Relaxed)
    {
        terminate_current(Outcome::Timeout);
    }
}

fn map_page(mem: &mut Memory, addr: u64, flags: PageTableFlags) -> Result<Page, RunError> {
    let page = Page::<Size4KiB>::containing_address(VirtAddr::new(addr));
    let frame = mem.frames.allocate_frame().ok_or(RunError::NoMemory)?;
    let parent = PageTableFlags::PRESENT | PageTableFlags::WRITABLE | PageTableFlags::USER_ACCESSIBLE;
    // SAFETY: the user virtual range is reserved for the single user process
    // and unmapped; the frame is freshly allocated and exclusively ours.
    unsafe { mem.mapper.map_to_with_table_flags(page, frame, flags, parent, &mut mem.frames) }
        .map_err(|_| RunError::NoMemory)?
        .flush();
    Ok(page)
}

/// Run `program` (raw x86_64 machine code, entered at its first byte) in ring
/// 3 with exactly `caps`, for at most `budget_ticks` timer ticks.
pub fn run(program: &[u8], caps: Caps, budget_ticks: u64) -> Result<Outcome, RunError> {
    use x86_64::instructions::interrupts;

    if program.is_empty() || program.len() > 4096 {
        return Err(RunError::TooLarge);
    }
    if RUNNING.swap(true, Ordering::SeqCst) {
        return Err(RunError::Busy);
    }
    let result = run_inner(program, caps, budget_ticks);
    RUNNING.store(false, Ordering::SeqCst);
    // The int-gate/fault paths that ended the process left IF cleared.
    interrupts::enable();
    result
}

fn run_inner(program: &[u8], caps: Caps, budget_ticks: u64) -> Result<Outcome, RunError> {
    let stack_base = USER_STACK_TOP - USER_STACK_PAGES * 4096;
    let (code_page, stack_pages);
    {
        let mut guard = memory::MEMORY.lock();
        let mem = guard.as_mut().ok_or(RunError::NoMemory)?;

        let rw_user = PageTableFlags::PRESENT | PageTableFlags::WRITABLE | PageTableFlags::USER_ACCESSIBLE;
        code_page = map_page(mem, USER_CODE_ADDR, rw_user)?;
        let s0 = map_page(mem, stack_base, rw_user | PageTableFlags::NO_EXECUTE)?;
        let s1 = map_page(mem, stack_base + 4096, rw_user | PageTableFlags::NO_EXECUTE)?;
        stack_pages = [s0, s1];

        // SAFETY: the pages were just mapped writable and are exclusively ours;
        // zeroing first ensures no stale data from earlier use of the frame.
        unsafe {
            core::ptr::write_bytes(USER_CODE_ADDR as *mut u8, 0, 4096);
            core::ptr::copy_nonoverlapping(program.as_ptr(), USER_CODE_ADDR as *mut u8, program.len());
            core::ptr::write_bytes(stack_base as *mut u8, 0, (USER_STACK_PAGES * 4096) as usize);
        }
        // Code becomes read-only + executable (W^X).
        let rx_user = PageTableFlags::PRESENT | PageTableFlags::USER_ACCESSIBLE;
        // SAFETY: the page is mapped; only its flags change.
        unsafe { mem.mapper.update_flags(code_page, rx_user) }
            .map_err(|_| RunError::NoMemory)?
            .flush();
    }

    let (user_cs, user_ss) = crate::gdt::user_selectors();
    CURRENT_CAPS.store(caps.bits(), Ordering::Relaxed);
    DEADLINE_TICK.store(crate::time::ticks().saturating_add(budget_ticks), Ordering::Relaxed);
    IN_USER.store(true, Ordering::SeqCst);
    // SAFETY: the code page is mapped user-executable, the stack top is the end
    // of the mapped user stack, and the selectors are the ring-3 descriptors in
    // the loaded GDT. The TSS holds a valid ring-0 stack for the way back.
    let raw = unsafe {
        enter_user_mode(USER_CODE_ADDR, USER_STACK_TOP, user_cs.0 as u64, user_ss.0 as u64)
    };
    IN_USER.store(false, Ordering::SeqCst);
    CURRENT_CAPS.store(0, Ordering::Relaxed);

    // Tear down: wipe and unmap. (Frames are not returned to an allocator yet.)
    let mut guard = memory::MEMORY.lock();
    if let Some(mem) = guard.as_mut() {
        // SAFETY: the pages are still mapped writable (stack) / need a flag
        // change (code); the process no longer runs.
        unsafe {
            let rw = PageTableFlags::PRESENT | PageTableFlags::WRITABLE;
            if let Ok(f) = mem.mapper.update_flags(code_page, rw) {
                f.flush();
            }
            core::ptr::write_bytes(USER_CODE_ADDR as *mut u8, 0, 4096);
            core::ptr::write_bytes(stack_base as *mut u8, 0, (USER_STACK_PAGES * 4096) as usize);
        }
        for page in [code_page, stack_pages[0], stack_pages[1]] {
            if let Ok((_, flush)) = mem.mapper.unmap(page) {
                flush.flush();
            }
        }
    }
    Ok(Outcome::from_raw(raw))
}

// ------------------------------------------------------------ test programs
//
// Hand-assembled ring-3 programs used by the shell demo and the tests.
// Calling convention: see l_core::syscall. Exit code = value passed to exit.

macro_rules! user_programs {
    ($($name:ident),* $(,)?) => {
        $(
            pub fn $name() -> &'static [u8] {
                unsafe extern "C" {
                    #[link_name = concat!("USER_", stringify!($name), "_START")]
                    static START: u8;
                    #[link_name = concat!("USER_", stringify!($name), "_END")]
                    static END: u8;
                }
                // SAFETY: START/END delimit a byte blob emitted by global_asm
                // below into .rodata; it is immutable, so a shared slice is sound.
                unsafe {
                    let start = &raw const START;
                    let end = &raw const END;
                    core::slice::from_raw_parts(start, end.offset_from(start) as usize)
                }
            }
        )*
    };
}

user_programs!(
    prog_hello, prog_write_nocap, prog_read_kernel, prog_write_kernel, prog_privileged, prog_spin,
    prog_badptr_heap, prog_badptr_kernel_half, prog_net_443, prog_net_80, prog_net_8443,
    prog_ud2, prog_div0,
);

global_asm!(
    r#"
    .section .rodata
    .macro PROG name
    .global USER_\name\()_START
    .global USER_\name\()_END
    .endm

    // exit(rdi) helper: mov eax,0 ; int 0x80
    // 1. hello: write 18 bytes, exit(bytes written)
    PROG prog_hello
USER_prog_hello_START:
    mov eax, 1
    lea rdi, [rip + 2f]
    mov esi, 18
    int 0x80
    mov rdi, rax
    mov eax, 0
    int 0x80
2:  .ascii "hello from ring 3\n"
USER_prog_hello_END:

    // 2. write without Cap::Console: exit(-result)
    PROG prog_write_nocap
USER_prog_write_nocap_START:
    mov eax, 1
    lea rdi, [rip + 2f]
    mov esi, 2
    int 0x80
    neg rax
    mov rdi, rax
    mov eax, 0
    int 0x80
2:  .ascii "hi"
USER_prog_write_nocap_END:

    // 3. read kernel heap (supervisor-only page) -> #PF
    PROG prog_read_kernel
USER_prog_read_kernel_START:
    movabs rax, 0x444444440000
    mov rbx, qword ptr [rax]
    mov eax, 0
    xor edi, edi
    int 0x80
USER_prog_read_kernel_END:

    // 4. write kernel heap -> #PF
    PROG prog_write_kernel
USER_prog_write_kernel_START:
    movabs rax, 0x444444440000
    mov qword ptr [rax], 1
    mov eax, 0
    xor edi, edi
    int 0x80
USER_prog_write_kernel_END:

    // 5. privileged instruction -> #GP
    PROG prog_privileged
USER_prog_privileged_START:
    hlt
    mov eax, 0
    xor edi, edi
    int 0x80
USER_prog_privileged_END:

    // 6. infinite loop -> killed by the timer budget
    PROG prog_spin
USER_prog_spin_START:
1:  jmp 1b
USER_prog_spin_END:

    // 7. write() with a pointer into supervisor-only kernel heap; exit(-result)
    PROG prog_badptr_heap
USER_prog_badptr_heap_START:
    mov eax, 1
    movabs rdi, 0x444444440000
    mov esi, 8
    int 0x80
    neg rax
    mov rdi, rax
    mov eax, 0
    int 0x80
USER_prog_badptr_heap_END:

    // 8. write() with a kernel-half pointer; exit(-result)
    PROG prog_badptr_kernel_half
USER_prog_badptr_kernel_half_START:
    mov eax, 1
    movabs rdi, 0xffff800000000000
    mov esi, 8
    int 0x80
    neg rax
    mov rdi, rax
    mov eax, 0
    int 0x80
USER_prog_badptr_kernel_half_END:

    // 9-11. net_check("example.com", port); exit(-result)
    .macro NETPROG name, port
    PROG \name
USER_\name\()_START:
    mov eax, 2
    lea rdi, [rip + 2f]
    mov esi, 11
    mov edx, \port
    int 0x80
    neg rax
    mov rdi, rax
    mov eax, 0
    int 0x80
2:  .ascii "example.com"
USER_\name\()_END:
    .endm
    NETPROG prog_net_443, 443
    NETPROG prog_net_80, 80
    NETPROG prog_net_8443, 8443

    // 12. ud2 -> #UD
    PROG prog_ud2
USER_prog_ud2_START:
    ud2
USER_prog_ud2_END:

    // 13. division by zero -> #DE
    PROG prog_div0
USER_prog_div0_START:
    xor edx, edx
    mov eax, 1
    xor ecx, ecx
    div ecx
USER_prog_div0_END:
"#
);
