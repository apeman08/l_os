//! The kernel finds the PM1 control block and the S5 sleep type in QEMU's tables.
#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(l_os::test_runner)]
#![reexport_test_harness_main = "test_main"]

use core::panic::PanicInfo;
use l_os::{acpi, serial_println};
use x86_64::instructions::port::Port;

bootloader_api::entry_point!(main, config = &l_os::BOOTLOADER_CONFIG);

fn main(boot_info: &'static mut bootloader_api::BootInfo) -> ! {
    l_os::init();
    // init_memory remembers rsdp_addr and runs acpi::init.
    l_os::init_memory(boot_info);
    test_main();
    l_os::hlt_loop();
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    l_os::test_panic_handler(info)
}

#[test_case]
fn finds_s5_in_qemu_tables() {
    let plan = acpi::power_off_plan().expect("S5 poweroff plan from the ACPI tables");
    serial_println!(
        "  pm1a_cnt {:#x} pm1b_cnt {:#x} value_a {:#x} value_b {:#x}",
        plan.pm1a_cnt, plan.pm1b_cnt, plan.value_a, plan.value_b
    );
    assert_ne!(plan.pm1a_cnt, 0);
    assert_ne!(plan.value_a & (1 << 13), 0, "SLP_EN");
}

#[test_case]
fn sci_enabled_after_init() {
    let plan = acpi::power_off_plan().expect("plan");
    // SAFETY: reading the PM1a control register port reported by the FADT.
    let v = unsafe { Port::<u16>::new(plan.pm1a_cnt).read() };
    serial_println!("  PM1a_CNT = {:#x}", v);
    assert_eq!(v & 1, 1, "SCI_EN");
}
