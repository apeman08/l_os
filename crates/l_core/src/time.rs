//! Time helpers for the legacy PIT tick (pure math).
//!
//! The PIT runs at its power-on default (divisor 65536 of 1193182 Hz,
//! about 18.2 Hz). That default is an assumption observed only under QEMU.

pub const PIT_BASE_HZ: u64 = 1_193_182;
pub const PIT_DEFAULT_DIVISOR: u64 = 65_536;

/// Approximate whole seconds for a tick count at the assumed PIT rate.
pub fn approx_seconds(ticks: u64) -> u64 {
    ticks.saturating_mul(PIT_DEFAULT_DIVISOR) / PIT_BASE_HZ
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_default_pit_rate() {
        assert_eq!(approx_seconds(0), 0);
        assert_eq!(approx_seconds(18), 0);
        assert_eq!(approx_seconds(19), 1);
        assert!(approx_seconds(u64::MAX) > 0); // saturates, no panic
    }
}
