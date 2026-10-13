//! The PIT tick rate, measured against the CMOS real-time clock.
//!
//! Under QEMU the RTC follows the host clock, so this checks that the kernel
//! really programs the PIT for `l_core::time::TICK_HZ`.
#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(l_os::test_runner)]
#![reexport_test_harness_main = "test_main"]

use bootloader_api::{BootInfo, entry_point};
use core::panic::PanicInfo;
use l_core::time::TICK_HZ;
use l_os::{rtc, serial_println, time};

entry_point!(main, config = &l_os::BOOTLOADER_CONFIG);

fn main(_boot_info: &'static mut BootInfo) -> ! {
    l_os::init();
    test_main();
    l_os::hlt_loop();
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    l_os::test_panic_handler(info)
}

/// Busy-wait until the RTC's second changes; returns the tick count right
/// after the change.
fn next_rtc_second() -> (u64, u64) {
    let first = rtc::unix_time().expect("RTC unreadable or implausible");
    loop {
        let now = rtc::unix_time().expect("RTC unreadable or implausible");
        if now != first {
            return (now, time::ticks());
        }
        core::hint::spin_loop();
    }
}

#[test_case]
fn pit_ticks_at_tick_hz_against_the_rtc() {
    const SECONDS: u64 = 5;
    // Start on a second boundary so the interval is whole RTC seconds.
    let (t0, start) = next_rtc_second();
    let mut t = t0;
    let mut end = start;
    while t < t0 + SECONDS {
        (t, end) = next_rtc_second();
    }
    let ticks = end - start;
    let elapsed = t - t0;
    let deci_hz = ticks * 10 / elapsed;
    serial_println!(
        "  {} ticks in {} RTC seconds = {}.{} Hz (TICK_HZ = {})",
        ticks, elapsed, deci_hz / 10, deci_hz % 10, TICK_HZ
    );
    // Each boundary is seen within one poll of the CMOS (microseconds), so the
    // measurement itself is good to about one tick. The lower bound is only
    // half the rate: under TCG on a loaded host QEMU may drop PIT ticks (the
    // RTC follows the host clock, the guest does not get every interrupt), so
    // a tight band would flake; 50 % still rejects the old 18.2 Hz clearly.
    // Ticks cannot arrive faster than programmed, so the upper bound stays at
    // +10 % (measurement slack only).
    let expected = TICK_HZ as u64 * elapsed;
    assert!(
        ticks >= expected / 2 && ticks <= expected * 11 / 10,
        "{} ticks in {} s: rate not within 50%..110% of {} Hz",
        ticks, elapsed, TICK_HZ
    );
}
