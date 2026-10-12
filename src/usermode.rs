//! Ring 3 processes: building address spaces, loading programs, and the
//! `run`/`run_all` entry points. Scheduling lives in `sched.rs`.
//!
//! Current limits (see docs/THREAT_MODEL.md): programs are raw machine code
//! copied into one page (no ELF loader), at most `sched::MAX_PROCS` processes,
//! no blocking IPC. Isolation rests on separate page tables (kernel pages
//! supervisor-only), ring 3 privilege checks, and syscall argument validation;
//! it is exercised by `tests/usermode.rs` and `tests/processes.rs`.

use crate::memory::{self, Memory};
use crate::sched::{self, Process};
use alloc::{boxed::Box, vec::Vec};
use core::arch::global_asm;
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use l_core::{
    handles::{HandleTable, Object, Rights},
    policy::NetPolicy,
    syscall::Outcome,
};
use spin::Mutex;
use x86_64::{
    structures::paging::{FrameAllocator, Mapper, Page, PageTableFlags, Size4KiB},
    VirtAddr,
};

pub const USER_CODE_ADDR: u64 = 0x_7000_0000_0000;
pub const USER_STACK_TOP: u64 = 0x_7000_0010_0000;
const USER_STACK_PAGES: u64 = 2;

/// The network policy consulted by `SYS_NET_CHECK`. Do not hold this lock
/// while calling [`run`].
pub static POLICY: Mutex<NetPolicy> = Mutex::new(NetPolicy::new());

static RUNNING: AtomicBool = AtomicBool::new(false);
static NEXT_PID: AtomicU32 = AtomicU32::new(1);

global_asm!(
    r#"
    .section .text
    // int 0x80 entry. rax = number, rdi/rsi/rdx/r10/r8 = args a0..a4. Result in
    // rax; every other register is preserved.
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
    mov r9, r8              // a4
    mov r8, r10             // a3
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
"#
);

unsafe extern "C" {
    pub fn syscall_entry();
}

#[derive(Debug, PartialEq, Eq)]
pub enum RunError {
    /// Processes are already running (re-entrancy).
    Busy,
    TooLarge,
    TooMany,
    /// A handle in the spec is invalid (too many, or names a missing endpoint).
    BadHandles,
    /// Memory manager missing or out of frames.
    NoMemory,
}

/// What to start: code, the handles the process is born with (slot order =
/// handle values 0, 1, 2, ... in a fresh table), and its CPU budget. A process
/// has no authority beyond these handles.
pub struct Spec<'a> {
    pub program: &'a [u8],
    pub handles: &'a [(Object, Rights)],
    pub budget_ticks: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcessResult {
    pub outcome: Outcome,
    pub finished_tick: u64,
}

pub fn user_process_active() -> bool {
    sched::process_active()
}

/// Terminate the running user process from kernel context (fault or exit).
/// Never returns to the interrupted user code.
pub fn terminate_current(outcome: Outcome) -> ! {
    sched::exit_current(outcome)
}

/// Build the address space of one process. On any failure everything already
/// allocated for it is released again.
fn build_process(mem: &mut Memory, spec: &Spec) -> Result<Box<Process>, RunError> {
    let mut table = HandleTable::new();
    for &(object, rights) in spec.handles {
        if let Object::Endpoint(id) = object {
            if !crate::ipc::exists(id) {
                return Err(RunError::BadHandles);
            }
        }
        table.insert(object, rights).map_err(|_| RunError::BadHandles)?;
    }
    let l4 = memory::create_address_space(mem).map_err(|_| RunError::NoMemory)?;
    match populate(mem, l4, spec) {
        Ok(()) => {
            let id = NEXT_PID.fetch_add(1, Ordering::Relaxed);
            Ok(Process::new(id, table, spec.budget_ticks, l4, USER_CODE_ADDR, USER_STACK_TOP))
        }
        Err(e) => {
            memory::destroy_address_space(mem, l4);
            Err(e)
        }
    }
}

fn populate(mem: &mut Memory, l4: x86_64::structures::paging::PhysFrame, spec: &Spec) -> Result<(), RunError> {
    // SAFETY: `l4` was just created by create_address_space and is not active.
    let mut mapper = unsafe { memory::mapper_for(l4) };
    let parent = PageTableFlags::PRESENT | PageTableFlags::WRITABLE | PageTableFlags::USER_ACCESSIBLE;
    let off = memory::phys_offset();

    let mut map = |addr: u64, flags: PageTableFlags, init: Option<&[u8]>| -> Result<(), RunError> {
        let page = Page::<Size4KiB>::containing_address(VirtAddr::new(addr));
        let frame = mem.frames.allocate_frame().ok_or(RunError::NoMemory)?; // zeroed
        if let Some(bytes) = init {
            // SAFETY: the frame is exclusively ours and mapped at the physical
            // offset; bytes.len() <= 4096 was checked by the caller. This goes
            // through the kernel's mapping, never through user addresses.
            unsafe {
                core::ptr::copy_nonoverlapping(
                    bytes.as_ptr(),
                    (off + frame.start_address().as_u64()).as_mut_ptr::<u8>(),
                    bytes.len(),
                );
            }
        }
        // SAFETY: the target virtual page is unmapped in this fresh address
        // space and the frame is not mapped anywhere else.
        unsafe { mapper.map_to_with_table_flags(page, frame, flags, parent, &mut mem.frames) }
            .map_err(|_| RunError::NoMemory)?
            .ignore(); // not the active address space: nothing to flush
        Ok(())
    };

    // Code: readable + executable, never writable (W^X).
    map(USER_CODE_ADDR, PageTableFlags::PRESENT | PageTableFlags::USER_ACCESSIBLE, Some(spec.program))?;
    // Stack: writable, never executable.
    let stack_base = USER_STACK_TOP - USER_STACK_PAGES * 4096;
    let rw = PageTableFlags::PRESENT | PageTableFlags::WRITABLE | PageTableFlags::USER_ACCESSIBLE
        | PageTableFlags::NO_EXECUTE;
    for i in 0..USER_STACK_PAGES {
        map(stack_base + i * 4096, rw, None)?;
    }
    Ok(())
}

/// Run several programs concurrently (preemptive round-robin) until all have
/// ended, each in its own address space with exactly its `caps` and a budget
/// of `budget_ticks` CPU ticks. Results are in the order of `specs`.
pub fn run_all(specs: &[Spec]) -> Result<Vec<ProcessResult>, RunError> {
    use x86_64::instructions::interrupts;

    if specs.len() > sched::MAX_PROCS {
        return Err(RunError::TooMany);
    }
    if specs.iter().any(|s| s.program.is_empty() || s.program.len() > 4096) {
        return Err(RunError::TooLarge);
    }
    if RUNNING.swap(true, Ordering::SeqCst) {
        return Err(RunError::Busy);
    }
    let result = run_all_inner(specs);
    crate::ipc::reset(); // endpoints do not outlive the group that used them
    RUNNING.store(false, Ordering::SeqCst);
    interrupts::enable();
    result
}

fn run_all_inner(specs: &[Spec]) -> Result<Vec<ProcessResult>, RunError> {
    use x86_64::instructions::interrupts;

    let mut procs: Vec<Box<Process>> = Vec::with_capacity(specs.len());
    {
        let mut guard = memory::MEMORY.lock();
        let mem = guard.as_mut().ok_or(RunError::NoMemory)?;
        for spec in specs {
            match build_process(mem, spec) {
                Ok(p) => procs.push(p),
                Err(e) => {
                    for p in procs {
                        memory::destroy_address_space(mem, p.l4);
                    }
                    return Err(e);
                }
            }
        }
    }

    // The scheduler switches stacks and CR3, so interrupts stay off until the
    // last process is gone (processes themselves run with IF=1 via iretq).
    let finished = interrupts::without_interrupts(|| sched::run_until_idle(procs));

    let mut results = Vec::with_capacity(finished.len());
    let mut guard = memory::MEMORY.lock();
    for p in finished {
        results.push(ProcessResult {
            // A process that never recorded an outcome is treated as killed.
            outcome: p.outcome().unwrap_or(Outcome::GeneralProtection),
            finished_tick: p.finished_tick(),
        });
        if let Some(mem) = guard.as_mut() {
            memory::destroy_address_space(mem, p.l4);
        }
        // `p` (and its kernel stack) is dropped here.
    }
    Ok(results)
}

/// Run one program; see [`run_all`].
pub fn run(program: &[u8], handles: &[(Object, Rights)], budget_ticks: u64) -> Result<Outcome, RunError> {
    let mut r = run_all(&[Spec { program, handles, budget_ticks }])?;
    Ok(r.remove(0).outcome)
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
    prog_ud2, prog_div0, prog_stack_11, prog_stack_22, prog_stack_overflow, prog_syscall_table,
    prog_use_after_close, prog_forged_handle, prog_send_console_write, prog_send_console_nothing,
    prog_send_console_amplify, prog_recv_and_write, prog_recv_only, prog_send_plain, prog_recv_wrong_right,
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
    xor edi, edi
    lea rsi, [rip + 2f]
    mov edx, 18
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
    xor edi, edi
    lea rsi, [rip + 2f]
    mov edx, 2
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
    xor edi, edi
    movabs rsi, 0x444444440000
    mov edx, 8
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
    xor edi, edi
    movabs rsi, 0xffff800000000000
    mov edx, 8
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
    xor edi, edi
    lea rsi, [rip + 2f]
    mov edx, 11
    mov r10d, \port
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

    // Isolation probe: store VALUE in the stack page, yield 8 times (so the
    // other process runs and writes ITS value to the same virtual address),
    // then exit with whatever is in memory now. Equals VALUE iff isolated.
    .macro STACKPROG name, val
    PROG \name
USER_\name\()_START:
    mov dword ptr [rsp - 8], \val
    mov ecx, 8
1:  mov eax, 3
    int 0x80
    dec ecx
    jnz 1b
    mov edi, dword ptr [rsp - 8]
    mov eax, 0
    int 0x80
USER_\name\()_END:
    .endm
    STACKPROG prog_stack_11, 11
    STACKPROG prog_stack_22, 22

    // Runs off the bottom of its stack -> #PF on an unmapped page.
    PROG prog_stack_overflow
USER_prog_stack_overflow_START:
1:  push rax
    jmp 1b
USER_prog_stack_overflow_END:

    // Table-driven write() probe. The kernel appends, right after this code,
    // a table: u64 count, then count * (u64 ptr, u64 len). Exit code = number
    // of calls the kernel ACCEPTED (non-negative result).
    PROG prog_syscall_table
USER_prog_syscall_table_START:
    lea rbx, [rip + USER_prog_syscall_table_END]
    mov r12, qword ptr [rbx]
    add rbx, 8
    xor r13d, r13d
1:  test r12, r12
    jz 2f
    mov eax, 1
    xor edi, edi
    mov rsi, qword ptr [rbx]
    mov rdx, qword ptr [rbx + 8]
    int 0x80
    test rax, rax
    js 3f
    inc r13
3:  add rbx, 16
    dec r12
    jmp 1b
2:  mov rdi, r13
    mov eax, 0
    int 0x80
USER_prog_syscall_table_END:

    // close(0), then write(0, ...): the closed handle must be dead. exit(-result)
    PROG prog_use_after_close
USER_prog_use_after_close_START:
    mov eax, 4
    xor edi, edi
    int 0x80
    mov eax, 1
    xor edi, edi
    lea rsi, [rip + 2f]
    mov edx, 2
    int 0x80
    neg rax
    mov rdi, rax
    mov eax, 0
    int 0x80
2:  .ascii "hi"
USER_prog_use_after_close_END:

    // write() through a handle value the process was never given (slot 1, gen 5)
    PROG prog_forged_handle
USER_prog_forged_handle_START:
    mov eax, 1
    mov edi, 0x501
    lea rsi, [rip + 2f]
    mov edx, 2
    int 0x80
    neg rax
    mov rdi, rax
    mov eax, 0
    int 0x80
2:  .ascii "hi"
USER_prog_forged_handle_END:

    // Parent: send(ep=handle 1, "hi!\n", xfer=handle 0, mask=\mask); exit(-result)
    .macro SENDPROG name, mask
    PROG \name
USER_\name\()_START:
    mov eax, 5
    mov edi, 1
    lea rsi, [rip + 2f]
    mov edx, 4
    xor r10d, r10d
    mov r8d, \mask
    int 0x80
    neg rax
    mov rdi, rax
    mov eax, 0
    int 0x80
2:  .ascii "hi!\n"
USER_\name\()_END:
    .endm
    SENDPROG prog_send_console_write, 1       // mask = WRITE
    SENDPROG prog_send_console_nothing, 4     // mask = SEND only: console right is stripped
    SENDPROG prog_send_console_amplify, 31    // mask = everything: must not add rights

    // Child: recv(ep=handle 0) -> packed result; write(received handle, buf, len);
    // exit(result of write)
    PROG prog_recv_and_write
USER_prog_recv_and_write_START:
    mov eax, 6
    xor edi, edi
    lea rsi, [rsp - 128]
    mov edx, 64
    int 0x80
    mov rbx, rax
    shr rbx, 32
    movzx edx, ax
    mov rdi, rbx
    lea rsi, [rsp - 128]
    mov eax, 1
    int 0x80
    mov rdi, rax
    mov eax, 0
    int 0x80
USER_prog_recv_and_write_END:

    // Child that only waits for a message: exit(len) (blocks forever if none)
    PROG prog_recv_only
USER_prog_recv_only_START:
    mov eax, 6
    xor edi, edi
    lea rsi, [rsp - 128]
    mov edx, 64
    int 0x80
    movzx edi, ax
    mov eax, 0
    int 0x80
USER_prog_recv_only_END:

    // send("hi!\n") without any transfer: exit(-result)
    PROG prog_send_plain
USER_prog_send_plain_START:
    mov eax, 5
    xor edi, edi
    lea rsi, [rip + 2f]
    mov edx, 4
    mov r10, -1
    xor r8d, r8d
    int 0x80
    neg rax
    mov rdi, rax
    mov eax, 0
    int 0x80
2:  .ascii "hi!\n"
USER_prog_send_plain_END:

    // recv() on handle 0 (used with a SEND-only endpoint handle): exit(-result)
    PROG prog_recv_wrong_right
USER_prog_recv_wrong_right_START:
    mov eax, 6
    xor edi, edi
    lea rsi, [rsp - 128]
    mov edx, 64
    int 0x80
    neg rax
    mov rdi, rax
    mov eax, 0
    int 0x80
USER_prog_recv_wrong_right_END:
"#
);
