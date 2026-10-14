//! Wipe triggers of the USB path (spec §7.1 step 8, §7.2, §8.1; M2 Task 23):
//! boot-stick removal (`Trigger::StickRemoved`) and controller loss
//! (`Trigger::ControllerLost`), checked against the real `qemu-xhci` of the
//! runner (`usb_*` tests get the image as a read-only stick plus a USB
//! keyboard). Built only with feature `test-hooks`: `wipe::trigger` counts
//! ([`wipe::test_hook_count`]) instead of wiping the test kernel.
//!
//! The test cases share one boot and the harness runs them sorted by name:
//! `arming_required_before_any_trigger` sorts first and checks the state
//! before [`usb::arm_stick_watch`] (it fails if it ever runs later); every
//! other case arms itself (idempotent) and checks the hook count as a delta
//! with the watch's latch open (`controller_gone_triggers_wipe_hook` reopens
//! it through the test-hooks-only `xhci::reopen_watch_latch`). The real unplug (QMP `device_del` of the stick
//! wiping a production build) is `scripts/qemu_wipe_test.py --trigger
//! unplug` (Task 24).
#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(l_os::test_runner)]
#![reexport_test_harness_main = "test_main"]

use core::panic::PanicInfo;
use l_core::usb_desc::DeviceKind;
use l_core::wipe::Trigger;
use l_core::xhci_ctx::{self as ctx, PortChange, PortSc};
use l_os::{serial_println, usb, wipe, xhci};

bootloader_api::entry_point!(main, config = &l_os::BOOTLOADER_CONFIG);

fn main(boot_info: &'static mut bootloader_api::BootInfo) -> ! {
    l_os::init();
    l_os::init_memory(boot_info);
    // The production boot order (src/main.rs) up to, not including, the
    // arming: enumerate, identify and seal the stick, start the keyboard.
    let _ = usb::enumerate();
    let stick = usb::identify_boot_stick();
    usb::storage_seal();
    let kbd = usb::start_keyboard();
    serial_println!("stick: {:?}, start_keyboard: {:?}", stick, kbd);
    test_main();
    l_os::hlt_loop();
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    l_os::test_panic_handler(info)
}

const CCS: u32 = 1;
const CSC: u32 = 1 << 17;

fn stick_port() -> u8 {
    usb::stick_port().expect("boot stick identified (runner attaches the image as usb-storage)")
}

fn keyboard_port() -> u8 {
    let s = usb::enumerate();
    s.devices()
        .iter()
        .find(|d| matches!(d.kind, DeviceKind::BootKeyboard { .. }))
        .map(|d| d.port)
        .expect("usb-kbd enumerated")
}

/// Arm (idempotent) and check the watch could fire: returns the hook count.
fn armed() -> u64 {
    assert_eq!(usb::arm_stick_watch(), Ok(stick_port()));
    assert!(!xhci::watch_fired(), "latch open");
    wipe::test_hook_count()
}

/// Let the timer ISR run `poll_tick` for `n` ticks.
fn ticks(n: u64) {
    let t0 = l_os::time::ticks();
    while l_os::time::ticks() < t0 + n {
        x86_64::instructions::hlt();
    }
}

/// `poll_tick` from thread context with interrupts off, `n` times.
fn poll(n: usize) {
    x86_64::instructions::interrupts::without_interrupts(|| {
        for _ in 0..n {
            xhci::poll_tick();
        }
    });
}

/// Brief, verbatim: the live PORTSC of the stick's port (CCS=1) classifies
/// as `Nothing`; the same value with CCS=0, CSC=1 (what QMP `device_del`
/// produced in the spike, 0x000202a0) as `Disconnected`. The watch verdict
/// that `poll_tick` acts on agrees.
#[test_case]
fn disconnect_classification_from_live_portsc() {
    let port = stick_port();
    let live = xhci::portsc(port).expect("PORTSC readable");
    serial_println!("\n  stick port {} PORTSC {:#x}", port, live.0);
    assert!(live.ccs(), "stick connected");
    assert!(!live.csc(), "stick port's change bits acknowledged at enumeration");
    assert_eq!(ctx::classify(true, live), PortChange::Nothing);
    assert_eq!(ctx::watch_verdict(xhci::usbsts().unwrap(), live), None);
    let gone = PortSc((live.0 & !CCS) | CSC);
    assert_eq!(ctx::classify(true, gone), PortChange::Disconnected);
    assert_eq!(ctx::watch_verdict(xhci::usbsts().unwrap(), gone), Some(Trigger::StickRemoved));
    // The lock-free read used by the timer ISR sees the same port.
    assert_eq!(xhci::portsc_unlocked(port).map(|p| p.ccs()), Some(true));
}

/// Before the arming (spec §7.1 step 8) nothing triggers, not even a
/// controller that reads all-ones (E8: no stick watched, no user state).
#[test_case]
fn arming_required_before_any_trigger() {
    assert_eq!(xhci::watched_port(), None, "runs first (sorted by name), before any arming");
    let before = wipe::test_hook_count();
    xhci::simulate_read_all_ones(true);
    assert_eq!(xhci::usbsts(), Some(0xFFFF_FFFF), "simulation reaches the lock-free reads");
    poll(5);
    ticks(5);
    xhci::simulate_read_all_ones(false);
    assert_ne!(xhci::usbsts(), Some(0xFFFF_FFFF));
    assert_eq!(wipe::test_hook_count(), before);
    assert_eq!(before, 0);
}

/// Arming with the stick present: the arming check and many ISR polls of
/// the stick port's level (CCS=1) trigger nothing; arming is idempotent.
#[test_case]
fn stick_present_level_poll_does_not_trigger() {
    let port = stick_port();
    let before = armed();
    assert_eq!(xhci::watched_port(), Some(port));
    assert_eq!(usb::arm_stick_watch(), Ok(port), "idempotent");
    assert_eq!(xhci::arm_stick_watch(port), Ok(()));
    let other = if port == 1 { 2 } else { 1 };
    assert_eq!(xhci::arm_stick_watch(other), Err(xhci::XhciError::BadPort), "never re-pointed");
    assert_eq!(xhci::watched_port(), Some(port));
    poll(50);
    ticks(20);
    assert_eq!(wipe::test_hook_count(), before);
    assert!(!xhci::watch_fired());
    // The keyboard is untouched by the watch.
    assert_eq!(xhci::keyboard_in_flight(), Some(xhci::KEYBOARD_TRBS));
}

/// Brief, verbatim: a Port Status Change event on the keyboard's port never
/// triggers (the keyboard is still connected: it keeps running, too).
#[test_case]
fn keyboard_port_change_does_not_trigger() {
    let kbd = keyboard_port();
    assert_ne!(kbd, stick_port());
    let before = armed();
    xhci::simulate_port_event(kbd);
    poll(3);
    ticks(3);
    assert_eq!(wipe::test_hook_count(), before);
    assert!(usb::keyboard_present(), "connected keyboard not stopped by a change event");
    assert_eq!(xhci::keyboard_in_flight(), Some(xhci::KEYBOARD_TRBS));
}

/// An event on the stick's port is classified by the port's level, not by
/// its arrival: CCS=1 without CSC triggers nothing. Events on other (empty)
/// ports neither.
#[test_case]
fn stick_port_event_with_stick_present_does_not_trigger() {
    let port = stick_port();
    let before = armed();
    let conns = xhci::new_connections();
    xhci::simulate_port_event(port);
    let other = (1..=xhci::port_count()).find(|p| *p != port && *p != keyboard_port()).expect("a free port");
    xhci::simulate_port_event(other);
    poll(3);
    ticks(3);
    assert_eq!(wipe::test_hook_count(), before);
    assert!(xhci::portsc(port).unwrap().ccs());
    assert_eq!(xhci::new_connections(), conns, "empty port: no new connection");
}

/// Brief, verbatim: armed, a controller that reads all-ones (gone from the
/// bus) triggers at the first `poll_tick`, as `ControllerLost`; the trigger
/// fires once, however many ticks follow.
#[test_case]
fn controller_gone_triggers_wipe_hook() {
    let before = armed();
    assert_eq!(before, 0, "nothing triggered before");
    xhci::simulate_read_all_ones(true);
    poll(1);
    assert_eq!(wipe::test_hook_count(), 1, "first observation triggers");
    assert_eq!(wipe::test_hook_last(), Some(Trigger::ControllerLost));
    assert!(xhci::watch_fired());
    poll(10);
    ticks(5);
    xhci::simulate_read_all_ones(false);
    poll(3);
    assert_eq!(wipe::test_hook_count(), 1, "fires once");
    // For the cases sorted after this one: watch the (real, healthy)
    // controller again.
    xhci::reopen_watch_latch();
    poll(5);
    assert_eq!(wipe::test_hook_count(), 1);
}
