//! Multi-process tests: separate address spaces, preemption, reclamation.
#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(l_os::test_runner)]
#![reexport_test_harness_main = "test_main"]

extern crate alloc;

use alloc::vec::Vec;
use bootloader_api::{BootInfo, entry_point};
use core::panic::PanicInfo;
use l_core::{
    handles::{Object, Rights},
    syscall::{Outcome, USER_SPACE_END},
};
use l_os::usermode::{self, Program, Spec, USER_CODE_ADDR, USER_STACK_PAGES, USER_STACK_TOP};

entry_point!(main, config = &l_os::BOOTLOADER_CONFIG);

fn main(boot_info: &'static mut BootInfo) -> ! {
    l_os::init();
    l_os::init_memory(boot_info);
    test_main();
    l_os::hlt_loop();
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    l_os::test_panic_handler(info)
}

type Handles = &'static [(Object, Rights)];
const NONE: Handles = &[];
const CONSOLE: Handles = &[(Object::Console, Rights::WRITE)];

/// About 5.5 s of CPU time: plenty for the short test programs.
const BUDGET: u64 = l_os::time::ticks_from_ms(5500);

fn spec<'a>(program: &'a [u8], handles: &'a [(Object, Rights)], budget_ticks: u64) -> Spec<'a> {
    Spec { program: Program::Raw(program), handles, budget_ticks, args: &[] }
}

#[test_case]
fn two_processes_do_not_share_memory() {
    // Both write a different value to the SAME virtual address, interleave via
    // yield, and read it back. Shared memory would make one see the other's value.
    let r = usermode::run_all(&[
        spec(usermode::prog_stack_11(), NONE, BUDGET),
        spec(usermode::prog_stack_22(), NONE, BUDGET),
    ])
    .unwrap();
    assert_eq!(r[0].outcome, Outcome::Exited(11));
    assert_eq!(r[1].outcome, Outcome::Exited(22));
}

#[test_case]
fn many_processes_interleave_and_stay_isolated() {
    let progs = [
        usermode::prog_stack_11(), usermode::prog_stack_22(), usermode::prog_stack_11(), usermode::prog_stack_22(),
        usermode::prog_stack_11(), usermode::prog_stack_22(), usermode::prog_stack_11(), usermode::prog_stack_22(),
    ];
    let specs: Vec<Spec> = progs.iter().map(|p| spec(p, NONE, BUDGET)).collect();
    let r = usermode::run_all(&specs).unwrap();
    for (i, res) in r.iter().enumerate() {
        assert_eq!(res.outcome, Outcome::Exited(if i % 2 == 0 { 11 } else { 22 }));
    }
}

#[test_case]
fn too_many_processes_rejected() {
    let p = usermode::prog_stack_11();
    let specs: Vec<Spec> = (0..9).map(|_| spec(p, NONE, l_os::time::ticks_from_ms(550))).collect();
    assert_eq!(usermode::run_all(&specs).err(), Some(usermode::RunError::TooMany));
}

#[test_case]
fn spinning_process_is_preempted_so_others_finish() {
    // The spinner never yields. If scheduling were cooperative, hello could
    // not finish before the spinner's budget (about 1 s) runs out.
    let r = usermode::run_all(&[
        spec(usermode::prog_spin(), NONE, l_os::time::ticks_from_seconds(1)),
        spec(usermode::prog_hello(), CONSOLE, BUDGET),
    ])
    .unwrap();
    assert_eq!(r[0].outcome, Outcome::Timeout);
    assert_eq!(r[1].outcome, Outcome::Exited(18));
    assert!(r[1].finished_tick < r[0].finished_tick, "hello must finish while the spinner is still running");
}

#[test_case]
fn a_crashing_process_does_not_disturb_others() {
    let r = usermode::run_all(&[
        spec(usermode::prog_read_kernel(), NONE, BUDGET),
        spec(usermode::prog_stack_11(), NONE, BUDGET),
        spec(usermode::prog_privileged(), NONE, BUDGET),
        spec(usermode::prog_stack_22(), NONE, BUDGET),
    ])
    .unwrap();
    assert_eq!(r[0].outcome, Outcome::PageFault);
    assert_eq!(r[1].outcome, Outcome::Exited(11));
    assert_eq!(r[2].outcome, Outcome::GeneralProtection);
    assert_eq!(r[3].outcome, Outcome::Exited(22));
}

#[test_case]
fn two_spinners_both_hit_their_budgets() {
    let r = usermode::run_all(&[
        spec(usermode::prog_spin(), NONE, l_os::time::ticks_from_ms(330)),
        spec(usermode::prog_spin(), NONE, l_os::time::ticks_from_ms(495)),
    ])
    .unwrap();
    assert_eq!(r[0].outcome, Outcome::Timeout);
    assert_eq!(r[1].outcome, Outcome::Timeout);
}

#[test_case]
fn no_frames_leak_across_runs() {
    // Warm up once so lazily created kernel structures don't count.
    usermode::run(usermode::prog_hello(), CONSOLE, BUDGET).unwrap();
    let before = l_os::memory::frames_in_use();
    for _ in 0..60 {
        usermode::run(usermode::prog_read_kernel(), NONE, BUDGET).unwrap();
        let r = usermode::run_all(&[
            spec(usermode::prog_stack_11(), NONE, BUDGET),
            spec(usermode::prog_stack_22(), NONE, BUDGET),
        ])
        .unwrap();
        assert_eq!(r[0].outcome, Outcome::Exited(11));
    }
    assert_eq!(l_os::memory::frames_in_use(), before, "frames leaked");
}

#[test_case]
fn user_stack_overflow_kills_only_the_process() {
    let r = usermode::run_all(&[
        spec(usermode::prog_stack_overflow(), NONE, BUDGET),
        spec(usermode::prog_stack_11(), NONE, BUDGET),
    ])
    .unwrap();
    assert_eq!(r[0].outcome, Outcome::PageFault);
    assert_eq!(r[1].outcome, Outcome::Exited(11));
}

/// Build the table-driven probe: generic code + (ptr, len) table.
fn syscall_probe(entries: &[(u64, u64)]) -> Vec<u8> {
    let mut v = Vec::from(usermode::prog_syscall_table());
    v.extend_from_slice(&(entries.len() as u64).to_le_bytes());
    for (ptr, len) in entries {
        v.extend_from_slice(&ptr.to_le_bytes());
        v.extend_from_slice(&len.to_le_bytes());
    }
    v
}

#[test_case]
fn write_syscall_rejects_every_bad_pointer() {
    let code_end = USER_CODE_ADDR + 4096;
    let bad: [(u64, u64); 16] = [
        (0, 8),                                  // null
        (1, 8),                                  // unmapped low address
        (USER_CODE_ADDR - 8, 16),                // starts in unmapped page below code
        (code_end - 8, 16),                      // runs off the end of the code page
        (code_end, 8),                           // first byte after code page
        (USER_STACK_TOP - 8, 16),                // runs off the end of the stack
        (USER_STACK_TOP, 8),                     // just above the stack
        (USER_STACK_TOP - USER_STACK_PAGES * 4096 - 8, 16), // starts in the guard page below the stack
        (0x4444_4444_0000, 8),                   // kernel heap (supervisor-only, mapped)
        (0xffff_8000_0000_0000, 8),              // kernel half
        (USER_SPACE_END - 4, 8),                 // straddles the user/kernel split
        (USER_SPACE_END, 8),                     // first non-user address
        (u64::MAX, 8),                           // end overflows
        (u64::MAX - 3, 16),                      // overflow
        (USER_CODE_ADDR, u64::MAX),              // absurd length
        (USER_CODE_ADDR, 0),                     // empty
    ];
    let prog = syscall_probe(&bad);
    assert_eq!(usermode::run(&prog, CONSOLE, BUDGET).unwrap(), Outcome::Exited(0), "some bad pointer was accepted");
}

#[test_case]
fn write_syscall_accepts_valid_pointers() {
    // Control for the test above: the probe itself works and good ranges pass.
    let good: [(u64, u64); 3] = [
        (USER_STACK_TOP - 8, 8),            // last 8 bytes of the stack
        (USER_STACK_TOP - 4096 - 4, 8),     // spans the boundary of the two stack pages
        (USER_CODE_ADDR, 4),                // start of code
    ];
    let prog = syscall_probe(&good);
    assert_eq!(usermode::run(&prog, CONSOLE, BUDGET).unwrap(), Outcome::Exited(3));
}
