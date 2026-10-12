//! PCI enumeration finds the emulated NIC without touching the network.
#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(l_os::test_runner)]
#![reexport_test_harness_main = "test_main"]

use core::panic::PanicInfo;
use l_os::{pci, serial_println};

bootloader_api::entry_point!(main, config = &l_os::BOOTLOADER_CONFIG);

fn main(_boot_info: &'static mut bootloader_api::BootInfo) -> ! {
    l_os::init();
    pci::enumerate();
    test_main();
    l_os::hlt_loop();
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    l_os::test_panic_handler(info)
}

#[test_case]
fn finds_host_bridge_and_virtio_net() {
    for d in pci::devices() {
        serial_println!("  {:02x}:{:02x}.{} {:04x}:{:04x} {} irq {} bar0 {:#x}", d.bus, d.slot, d.func, d.vendor, d.device, pci::class_name(&d), d.irq_line, d.bars[0]);
    }
    assert!(pci::devices().any(|d| d.class == 0x06 && d.subclass == 0x00), "host bridge");
    let nic = pci::find(0x1af4, 0x1000).expect("legacy virtio-net (QEMU -device virtio-net-pci,disable-modern=on)");
    assert_eq!((nic.class, nic.subclass), (0x02, 0x00));
    assert_eq!(nic.bars[0] & 1, 1, "BAR0 is an I/O-space BAR (legacy virtio)");
}
