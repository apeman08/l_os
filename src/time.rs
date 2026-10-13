//! Tick counter driven by the legacy PIT (IRQ0).
//!
//! [`init_pit`] programs PIT channel 0 for `l_core::time::TICK_HZ` (100 Hz).
//! `tests/timer.rs` measures the rate against the RTC under QEMU; real
//! hardware is not checked.

use core::sync::atomic::{AtomicU64, Ordering};
use x86_64::instructions::port::Port;

static TICKS: AtomicU64 = AtomicU64::new(0);

/// Program PIT channel 0 as a rate generator firing IRQ0 `TICK_HZ` times per
/// second. Call once during boot, before interrupts are enabled.
pub fn init_pit() {
    let divisor = l_core::time::PIT_DIVISOR;
    let mut command = Port::<u8>::new(0x43);
    let mut channel0 = Port::<u8>::new(0x40);
    // SAFETY: 0x43 is the PIT mode/command register and 0x40 the channel-0
    // data port; nothing else in the kernel uses them. 0x34 selects channel 0,
    // access lobyte then hibyte, mode 2 (rate generator), binary counting; the
    // two data writes are the reload value. This only changes how often IRQ0
    // fires, the IRQ0 handler stays the same. Channels 1 and 2 are untouched.
    unsafe {
        command.write(0x34);
        channel0.write((divisor & 0xff) as u8);
        channel0.write((divisor >> 8) as u8);
    }
}

/// Called from the timer ISR only.
pub fn tick() {
    TICKS.fetch_add(1, Ordering::Relaxed);
}

pub fn ticks() -> u64 {
    TICKS.load(Ordering::Relaxed)
}

pub use l_core::time::{TICK_HZ, approx_seconds, ticks_from_ms, ticks_from_seconds, ticks_to_ms};
