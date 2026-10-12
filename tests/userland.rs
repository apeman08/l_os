//! Rust user programs (ELF): loading, W^X, spawn/wait limits, keyboard input.
#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(l_os::test_runner)]
#![reexport_test_harness_main = "test_main"]

extern crate alloc;

use alloc::vec::Vec;
use bootloader::{BootInfo, entry_point};
use core::panic::PanicInfo;
use l_core::{
    handles::{Object, Rights},
    syscall::{ERR_BAD_HANDLE, ERR_NO_RIGHTS, Outcome},
};
use l_os::usermode::{self, Program, RunError, Spec, user_elf};

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

const CONSOLE: (Object, Rights) = (Object::Console, Rights::WRITE);

fn run_elf(name: &str, handles: &[(Object, Rights)], budget: u64) -> Outcome {
    let image = user_elf(name).expect("image exists");
    let spec = Spec { program: Program::Elf(image), handles, budget_ticks: budget, args: &[] };
    usermode::run_all(&[spec]).expect("run_all").remove(0).outcome
}

#[test_case]
fn rust_program_runs_from_an_elf_image() {
    assert_eq!(run_elf("hello_rs", &[CONSOLE], 100), Outcome::Exited(0));
}

#[test_case]
fn rust_program_without_a_console_handle_simply_cannot_print() {
    // hello_rs ignores the write error and exits normally; nothing is shown
    // and nothing crashes: authority is exactly the handle table.
    assert_eq!(run_elf("hello_rs", &[], 100), Outcome::Exited(0));
}

#[test_case]
fn loaded_image_is_w_xor_x() {
    assert_eq!(run_elf("fault_write_code", &[], 100), Outcome::PageFault, "code must not be writable");
    assert_eq!(run_elf("fault_write_rodata", &[], 100), Outcome::PageFault, "rodata must not be writable");
    assert_eq!(run_elf("fault_exec_stack", &[], 100), Outcome::PageFault, "stack must not be executable");
}

#[test_case]
fn corrupt_images_are_rejected_without_side_effects() {
    let good = user_elf("hello_rs").unwrap();
    let before = l_os::memory::frames_in_use();

    let mut bad_magic = Vec::from(good);
    bad_magic[0] = 0;
    let mut writable_code = Vec::from(good);
    // Make the first PT_LOAD (R+E) also writable: p_flags is at phoff(64)+4.
    writable_code[64 + 4] |= 2;
    let mut kernel_vaddr = Vec::from(good);
    kernel_vaddr[64 + 16..64 + 24].copy_from_slice(&0xffff_8000_0000_0000u64.to_le_bytes());
    let mut huge_memsz = Vec::from(good);
    huge_memsz[64 + 40..64 + 48].copy_from_slice(&u64::MAX.to_le_bytes());
    let truncated = Vec::from(&good[..100]);

    for (name, image) in [
        ("bad magic", bad_magic),
        ("writable+executable", writable_code),
        ("kernel vaddr", kernel_vaddr),
        ("huge memsz", huge_memsz),
        ("truncated", truncated),
    ] {
        let spec = Spec { program: Program::Elf(&image), handles: &[], budget_ticks: 10, args: &[] };
        assert_eq!(usermode::run_all(&[spec]).err(), Some(RunError::BadImage), "{name}");
    }
    assert_eq!(l_os::memory::frames_in_use(), before, "rejected images must not leak frames");
}

#[test_case]
fn spawn_and_wait_obey_their_limits() {
    // See user/src/bin/spawn_probe.rs for the list of checks; it exits with
    // the number of failures. It prints "check failed: ..." for each.
    let handles = [
        (Object::Console, Rights::WRITE.union(Rights::GRANT)),
        (Object::Spawner(0b0111), Rights::USE),
        (Object::Console, Rights::WRITE),
    ];
    assert_eq!(run_elf("spawn_probe", &handles, 60), Outcome::Exited(0));
}

#[test_case]
fn spawning_leaves_no_frames_behind() {
    let handles = [
        (Object::Console, Rights::WRITE.union(Rights::GRANT)),
        (Object::Spawner(0b0111), Rights::USE),
        (Object::Console, Rights::WRITE),
    ];
    run_elf("spawn_probe", &handles, 60); // warm up
    let before = l_os::memory::frames_in_use();
    for _ in 0..3 {
        assert_eq!(run_elf("spawn_probe", &handles, 60), Outcome::Exited(0));
    }
    assert_eq!(l_os::memory::frames_in_use(), before);
}

#[test_case]
fn keyboard_handle_delivers_queued_scancodes() {
    while l_os::input::SCANCODES.pop().is_some() {}
    for sc in [0x10, 0x20, 0x30] {
        assert!(l_os::input::SCANCODES.push(sc));
    }
    let r = run_elf("key_probe", &[(Object::Keyboard, Rights::READ)], 100);
    assert_eq!(r, Outcome::Exited(0x10 + 0x20 + 0x30));
    assert!(l_os::input::SCANCODES.pop().is_none(), "all three were consumed");
}

#[test_case]
fn keyboard_needs_the_right_object_and_right() {
    // Without any handle: three bad-handle errors.
    assert_eq!(run_elf("key_probe", &[], 100), Outcome::Exited((3 * ERR_BAD_HANDLE) as u16));
    // A console in slot 0 is not a keyboard.
    assert_eq!(run_elf("key_probe", &[CONSOLE], 100), Outcome::Exited((3 * ERR_NO_RIGHTS) as u16));
    // A keyboard handle without READ.
    assert_eq!(
        run_elf("key_probe", &[(Object::Keyboard, Rights::NONE)], 100),
        Outcome::Exited((3 * ERR_NO_RIGHTS) as u16)
    );
}
