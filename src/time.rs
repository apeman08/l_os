//! Tick counter driven by the legacy PIT (IRQ0).
//!
//! The bootloader does not reprogram the PIT, so it is assumed to run at its
//! power-on default (divisor 65536, 1193182 Hz base => ~18.2065 Hz). This has
//! only been checked under QEMU; real hardware and firmware may differ.

use core::sync::atomic::{AtomicU64, Ordering};

static TICKS: AtomicU64 = AtomicU64::new(0);

const PIT_BASE_HZ: u64 = 1_193_182;
const PIT_DEFAULT_DIVISOR: u64 = 65_536;

/// Called from the timer ISR only.
pub fn tick() {
    TICKS.fetch_add(1, Ordering::Relaxed);
}

pub fn ticks() -> u64 {
    TICKS.load(Ordering::Relaxed)
}

/// Approximate whole seconds for a tick count at the assumed PIT rate.
pub fn approx_seconds(ticks: u64) -> u64 {
    ticks.saturating_mul(PIT_DEFAULT_DIVISOR) / PIT_BASE_HZ
}

#[test_case]
fn approx_seconds_matches_default_pit_rate() {
    assert_eq!(approx_seconds(0), 0);
    assert_eq!(approx_seconds(18), 0);
    assert_eq!(approx_seconds(19), 1);
    assert_eq!(approx_seconds(u64::MAX) > 0, true); // saturates, no panic
}
