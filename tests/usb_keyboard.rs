//! The USB boot keyboard on the interrupt-IN path (spec §7.1 step 6, §7.2,
//! M2 Task 22): after enumeration and the storage seal, `usb::start_keyboard`
//! queues 8 Normal TRBs (8-byte report buffers in the device's DMA page) on
//! the keyboard's interrupt-IN endpoint, and the timer ISR's
//! `xhci::poll_tick` handles their completions. The runner attaches
//! `qemu-xhci` + `usb-storage` + `usb-kbd` to every `usb_*` test.
//!
//! No keys are pressed here (a test kernel has no QMP); typing through the
//! USB keyboard is checked end to end by `scripts/qemu_usb_smoke.py`.
#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(l_os::test_runner)]
#![reexport_test_harness_main = "test_main"]

use core::panic::PanicInfo;
use l_core::usb_desc::DeviceKind;
use l_os::{serial_println, usb, xhci};

bootloader_api::entry_point!(main, config = &l_os::BOOTLOADER_CONFIG);

fn main(boot_info: &'static mut bootloader_api::BootInfo) -> ! {
    l_os::init();
    l_os::init_memory(boot_info);
    // The production boot order (src/main.rs): enumerate, identify and seal
    // the stick, then start the keyboard.
    let _ = usb::enumerate();
    let _ = usb::identify_boot_stick();
    usb::storage_seal();
    let started = usb::start_keyboard();
    serial_println!("start_keyboard: {:?}", started);
    test_main();
    l_os::hlt_loop();
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    l_os::test_panic_handler(info)
}

/// Slot and endpoint (DCI) of the enumerated boot keyboard.
fn keyboard() -> (u8, u8) {
    let s = usb::enumerate();
    let d = s
        .devices()
        .iter()
        .find(|d| matches!(d.kind, DeviceKind::BootKeyboard { .. }))
        .expect("usb-kbd enumerated (runner attaches it to usb_* tests)");
    let DeviceKind::BootKeyboard { ep_in, .. } = d.kind else { unreachable!() };
    (d.slot, l_core::xhci_ctx::dci(ep_in.address))
}

#[test_case]
fn keyboard_endpoint_running() {
    let (slot, dci) = keyboard();
    let state = xhci::endpoint_state(slot, dci);
    let queued = xhci::keyboard_in_flight();
    serial_println!("\n  slot {} dci {} state {:?} in flight {:?}", slot, dci, state, queued);
    assert_eq!(state, Some(1), "endpoint context state Running");
    assert_eq!(queued, Some(xhci::KEYBOARD_TRBS));
    assert_eq!(xhci::KEYBOARD_TRBS, 8);
    assert!(usb::keyboard_present());
}

/// Ledger Task 21: the seal blocks bulk transfers only; the keyboard's
/// interrupt transfers run after it, without a single bulk TD.
#[test_case]
fn keyboard_runs_after_seal_without_bulk() {
    assert!(xhci::bulk_sealed());
    let tds = xhci::bulk_transfers();
    let reads = usb::stick_reads();
    // Idempotent: a second start queues nothing more.
    assert_eq!(usb::start_keyboard(), Ok(()));
    assert_eq!(xhci::keyboard_in_flight(), Some(xhci::KEYBOARD_TRBS));
    // Let the timer ISR poll for a while (no key is pressed: no reports).
    let t0 = l_os::time::ticks();
    while l_os::time::ticks() < t0 + 20 {
        x86_64::instructions::hlt();
    }
    assert_eq!(xhci::keyboard_in_flight(), Some(xhci::KEYBOARD_TRBS));
    assert_eq!((xhci::bulk_transfers(), usb::stick_reads()), (tds, reads));
    assert_eq!(usb::keyboard_reports(), 0, "nobody typed");
}

/// `poll_tick` from thread context with interrupts off: returns at once,
/// never blocks, handles at most `POLL_EVENTS_MAX` events.
#[test_case]
fn poll_tick_is_bounded_and_harmless() {
    assert_eq!(xhci::POLL_EVENTS_MAX, 16);
    x86_64::instructions::interrupts::without_interrupts(|| {
        for _ in 0..100 {
            assert!(xhci::poll_tick() <= xhci::POLL_EVENTS_MAX);
        }
    });
    assert_eq!(xhci::keyboard_in_flight(), Some(xhci::KEYBOARD_TRBS));
}
