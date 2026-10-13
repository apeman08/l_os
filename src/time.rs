//! Tick counter driven by the legacy PIT (IRQ0).
//!
//! [`init_pit`] programs PIT channel 0 for `l_core::time::TICK_HZ` (100 Hz).
//! `tests/timer.rs` measures the rate against the RTC under QEMU; real
//! hardware is not checked.

use core::sync::atomic::{AtomicU64, Ordering};
use x86_64::instructions::port::Port;

static TICKS: AtomicU64 = AtomicU64::new(0);
/// TSC frequency in Hz measured by [`calibrate_tsc`]; 0 = not calibrated.
static TSC_HZ: AtomicU64 = AtomicU64::new(0);
/// PIT ticks the calibration measures over (spec §8.6).
pub const TSC_CALIBRATION_TICKS: u64 = 5;
/// Upper bound on the TSC cycles spent waiting for one tick (a dead PIT must
/// not hang the boot). The PIT fires every 10 ms; the TSC frequency is not
/// known yet, so the bound is generous: 2e9 cycles is 0.5 s at 4 GHz and 1 s at
/// 2 GHz, 50 to 100 tick periods.
const TICK_WAIT_CYCLES: u64 = 2_000_000_000;

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

/// Read the time stamp counter.
pub fn rdtsc() -> u64 {
    // SAFETY: RDTSC only reads the counter; CR4.TSD is not set by this
    // kernel, so it is allowed at any privilege the kernel runs at.
    unsafe { core::arch::x86_64::_rdtsc() }
}

/// Spin (`pause`) until `done()` or until `max_cycles` TSC cycles passed;
/// `false` on timeout. Spins instead of `hlt`: with a dead timer `hlt` would
/// never return, so it could not be bounded.
fn spin_until(max_cycles: u64, mut done: impl FnMut() -> bool) -> bool {
    let start = rdtsc();
    loop {
        if done() {
            return true;
        }
        if rdtsc().wrapping_sub(start) >= max_cycles {
            return done();
        }
        core::hint::spin_loop();
    }
}

/// Wait until the tick counter differs from `from`; `false` if the PIT never
/// ticked within [`TICK_WAIT_CYCLES`].
fn wait_tick_after(from: u64) -> bool {
    spin_until(TICK_WAIT_CYCLES, || ticks() != from)
}

/// Measure the TSC frequency against [`TSC_CALIBRATION_TICKS`] PIT ticks
/// (about 50 ms) and remember it for [`tsc_hz`]. Called once at boot from
/// [`crate::init`] after interrupts are on. Stays uncalibrated (0) if
/// interrupts are off or the PIT does not tick.
pub fn calibrate_tsc() -> u64 {
    if !x86_64::instructions::interrupts::are_enabled() {
        return 0;
    }
    // Start on a tick edge, so that exactly N whole tick periods are timed.
    let t = ticks();
    if !wait_tick_after(t) {
        return 0;
    }
    let start_tick = ticks();
    let start = rdtsc();
    let mut now = start_tick;
    while now.wrapping_sub(start_tick) < TSC_CALIBRATION_TICKS {
        if !wait_tick_after(now) {
            return 0;
        }
        now = ticks();
    }
    let cycles = rdtsc().wrapping_sub(start);
    let hz = l_core::wipetime::tsc_hz(cycles, now.wrapping_sub(start_tick), TICK_HZ).unwrap_or(0);
    TSC_HZ.store(hz, Ordering::Relaxed);
    hz
}

/// TSC frequency in Hz from [`calibrate_tsc`]; 0 means "not calibrated" (the
/// wipe report then shows 0 µs for every stage).
pub fn tsc_hz() -> u64 {
    TSC_HZ.load(Ordering::Relaxed)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A condition that never comes true (a dead PIT) times out instead of hanging.
    #[test_case]
    fn spin_until_times_out_on_dead_condition() {
        let t0 = rdtsc();
        assert!(!spin_until(1_000_000, || false));
        assert!(rdtsc().wrapping_sub(t0) >= 1_000_000);
        assert!(spin_until(1_000_000, || true));
    }
}
