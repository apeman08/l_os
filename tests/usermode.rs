//! Ring-3 isolation tests. Each test runs real user-mode machine code inside
//! the kernel (booted in QEMU) and checks how it ended.
#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(l_os::test_runner)]
#![reexport_test_harness_main = "test_main"]

use bootloader::{BootInfo, entry_point};
use core::panic::PanicInfo;
use l_core::{
    caps::{Cap, Caps},
    syscall::{ERR_BAD_POINTER, ERR_NO_CAP, Outcome},
};
use l_os::usermode::{self, POLICY};

entry_point!(main);

fn main(boot_info: &'static BootInfo) -> ! {
    l_os::init();
    l_os::init_memory(boot_info);
    test_main();
    l_os::hlt_loop();
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    l_os::test_panic_handler(info)
}

const BUDGET: u64 = 18; // ~1 s
const NET: Caps = Caps::NONE.with(Cap::Net);
const CONSOLE: Caps = Caps::NONE.with(Cap::Console);

fn run(p: &[u8], caps: Caps) -> Outcome {
    usermode::run(p, caps, BUDGET).expect("run failed")
}

#[test_case]
fn hello_runs_in_ring3_and_syscalls_work() {
    // 18 bytes written to the console, program exits with that count.
    assert_eq!(run(usermode::prog_hello(), CONSOLE), Outcome::Exited(18));
}

#[test_case]
fn no_capability_means_no_console() {
    assert_eq!(run(usermode::prog_write_nocap(), Caps::NONE), Outcome::Exited((-ERR_NO_CAP) as u16));
}

#[test_case]
fn user_cannot_read_kernel_memory() {
    assert_eq!(run(usermode::prog_read_kernel(), Caps::NONE), Outcome::PageFault);
}

#[test_case]
fn user_cannot_write_kernel_memory() {
    assert_eq!(run(usermode::prog_write_kernel(), Caps::NONE), Outcome::PageFault);
}

#[test_case]
fn privileged_instruction_is_rejected() {
    assert_eq!(run(usermode::prog_privileged(), Caps::NONE), Outcome::GeneralProtection);
}

#[test_case]
fn invalid_opcode_and_div0_kill_only_the_process() {
    assert_eq!(run(usermode::prog_ud2(), Caps::NONE), Outcome::InvalidOpcode);
    assert_eq!(run(usermode::prog_div0(), Caps::NONE), Outcome::DivideError);
}

#[test_case]
fn runaway_process_is_killed_by_time_budget() {
    let start = l_os::time::ticks();
    let out = usermode::run(usermode::prog_spin(), Caps::NONE, 5).unwrap();
    assert_eq!(out, Outcome::Timeout);
    assert!(l_os::time::ticks() - start >= 5);
}

#[test_case]
fn syscall_rejects_pointers_into_kernel_memory() {
    // Pointer into supervisor-only heap: passes the range check, fails the page check.
    assert_eq!(run(usermode::prog_badptr_heap(), CONSOLE), Outcome::Exited((-ERR_BAD_POINTER) as u16));
    // Kernel-half pointer: fails the range check.
    assert_eq!(run(usermode::prog_badptr_kernel_half(), CONSOLE), Outcome::Exited((-ERR_BAD_POINTER) as u16));
}

#[test_case]
fn kernel_survives_and_keeps_working_after_kills() {
    for _ in 0..3 {
        assert_eq!(run(usermode::prog_read_kernel(), Caps::NONE), Outcome::PageFault);
    }
    assert_eq!(run(usermode::prog_hello(), CONSOLE), Outcome::Exited(18));
    // Timer still ticks => interrupts and PIC are healthy after abandoned handlers.
    let t = l_os::time::ticks();
    while l_os::time::ticks() == t {
        core::hint::spin_loop();
    }
}

#[test_case]
fn net_check_default_deny_then_allow_revoke_expiry() {
    POLICY.lock().revoke_all();
    // No Cap::Net: refused before the policy is even consulted.
    assert_eq!(run(usermode::prog_net_443(), Caps::NONE), Outcome::Exited((-ERR_NO_CAP) as u16));
    // Cap::Net but no grant: denied (DENIED_NO_GRANT = -10).
    assert_eq!(run(usermode::prog_net_443(), NET), Outcome::Exited(10));

    POLICY.lock().grant("example.com", 443, None).unwrap();
    assert_eq!(run(usermode::prog_net_443(), NET), Outcome::Exited(0)); // allowed
    assert_eq!(run(usermode::prog_net_80(), NET), Outcome::Exited(10)); // other port still denied

    assert!(POLICY.lock().revoke("example.com", 443));
    assert_eq!(run(usermode::prog_net_443(), NET), Outcome::Exited(10)); // revoked

    POLICY.lock().grant("example.com", 8443, Some(0)).unwrap(); // already expired
    assert_eq!(run(usermode::prog_net_8443(), NET), Outcome::Exited(11)); // DENIED_EXPIRED
    POLICY.lock().revoke_all();
}

#[test_case]
fn empty_and_oversize_programs_are_rejected() {
    let big = [0x90u8; 5000];
    assert_eq!(usermode::run(&big, Caps::NONE, BUDGET), Err(usermode::RunError::TooLarge));
    assert_eq!(usermode::run(&[], Caps::NONE, BUDGET), Err(usermode::RunError::TooLarge));
}
