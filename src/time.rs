//! Tick counter driven by the legacy PIT (IRQ0).
//!
//! The bootloader does not reprogram the PIT, so it is assumed to run at its
//! power-on default (divisor 65536, 1193182 Hz base => ~18.2065 Hz). This has
//! only been checked under QEMU; real hardware and firmware may differ.

use core::sync::atomic::{AtomicU64, Ordering};

static TICKS: AtomicU64 = AtomicU64::new(0);

/// Called from the timer ISR only.
pub fn tick() {
    TICKS.fetch_add(1, Ordering::Relaxed);
}

pub fn ticks() -> u64 {
    TICKS.load(Ordering::Relaxed)
}

pub use l_core::time::approx_seconds;
