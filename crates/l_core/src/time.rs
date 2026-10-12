//! Time helpers for the PIT timer tick (pure math).
//!
//! The kernel programs PIT channel 0 to fire [`TICK_HZ`] times per second
//! (divisor [`PIT_DIVISOR`] of the 1193182 Hz base clock, i.e. 99.9985 Hz;
//! the 15 ppm difference to 100 Hz is ignored here). Durations in the kernel
//! and in user programs should be written with these helpers, not as raw
//! tick numbers.

/// Timer interrupts per second.
pub const TICK_HZ: u32 = 100;
/// Input clock of the 8253/8254 PIT.
pub const PIT_BASE_HZ: u64 = 1_193_182;
/// PIT channel-0 reload value for [`TICK_HZ`], rounded to nearest (11932).
pub const PIT_DIVISOR: u16 = ((PIT_BASE_HZ + TICK_HZ as u64 / 2) / TICK_HZ as u64) as u16;

/// Whole seconds covered by a tick count (rounded down).
pub const fn approx_seconds(ticks: u64) -> u64 {
    ticks / TICK_HZ as u64
}

/// Timer ticks needed to cover `seconds`.
pub const fn ticks_from_seconds(seconds: u64) -> u64 {
    seconds.saturating_mul(TICK_HZ as u64)
}

/// Milliseconds covered by a tick count (saturating).
pub const fn ticks_to_ms(ticks: u64) -> u64 {
    ticks.saturating_mul(1000) / TICK_HZ as u64
}

/// Timer ticks needed to cover `ms` milliseconds (rounded up, so a wait or
/// budget is never shorter than asked).
pub const fn ticks_from_ms(ms: u64) -> u64 {
    ms.saturating_mul(TICK_HZ as u64).div_ceil(1000)
}

/// Oldest year the wall clock may report; anything earlier means the RTC is
/// unset or wrong, and callers must not trust it (e.g. for certificate validity).
pub const MIN_PLAUSIBLE_YEAR: u32 = 2026;
pub const MAX_PLAUSIBLE_YEAR: u32 = 2099;

/// Seconds since 1970-01-01T00:00:00Z for a civil UTC date and time, or `None`
/// if the fields are out of range or the year is implausible.
pub fn unix_from_civil(year: u32, month: u32, day: u32, hour: u32, minute: u32, second: u32) -> Option<u64> {
    if !(MIN_PLAUSIBLE_YEAR..=MAX_PLAUSIBLE_YEAR).contains(&year)
        || !(1..=12).contains(&month)
        || hour > 23
        || minute > 59
        || second > 59
    {
        return None;
    }
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let dim = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        _ => if leap { 29 } else { 28 },
    };
    if day < 1 || day > dim {
        return None;
    }
    // Days from civil (H. Hinnant's algorithm), proleptic Gregorian calendar.
    let y = if month <= 2 { year as i64 - 1 } else { year as i64 };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (month as i64 + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(days as u64 * 86_400 + hour as u64 * 3600 + minute as u64 * 60 + second as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seconds_to_ticks_rounds_up_and_is_consistent() {
        assert_eq!(ticks_from_seconds(0), 0);
        assert_eq!(ticks_from_seconds(1), 100);
        for s in [1u64, 5, 60, 300, 3600] {
            assert!(approx_seconds(ticks_from_seconds(s)) >= s - 1);
        }
        assert!(ticks_from_seconds(u64::MAX) > 0); // saturates
    }

    #[test]
    fn civil_to_unix_known_values() {
        // Expected values computed independently with Python's calendar.timegm.
        assert_eq!(unix_from_civil(2026, 1, 1, 0, 0, 0), Some(1_767_225_600));
        assert_eq!(unix_from_civil(2027, 1, 15, 8, 0, 0), Some(1_800_000_000));
        assert_eq!(unix_from_civil(2028, 2, 29, 23, 59, 59), Some(1_835_481_599)); // leap day
        assert_eq!(unix_from_civil(2099, 12, 31, 23, 59, 59), Some(4_102_444_799));
    }

    #[test]
    fn civil_rejects_invalid_and_implausible_dates() {
        assert_eq!(unix_from_civil(2025, 12, 31, 23, 59, 59), None); // before the plausibility floor
        assert_eq!(unix_from_civil(1970, 1, 1, 0, 0, 0), None); // RTC reset value
        assert_eq!(unix_from_civil(2100, 1, 1, 0, 0, 0), None);
        assert_eq!(unix_from_civil(2026, 13, 1, 0, 0, 0), None);
        assert_eq!(unix_from_civil(2026, 0, 1, 0, 0, 0), None);
        assert_eq!(unix_from_civil(2026, 2, 29, 0, 0, 0), None); // 2026 is not a leap year
        assert_eq!(unix_from_civil(2026, 4, 31, 0, 0, 0), None);
        assert_eq!(unix_from_civil(2026, 4, 30, 24, 0, 0), None);
        assert_eq!(unix_from_civil(2026, 4, 30, 0, 60, 0), None);
        assert_eq!(unix_from_civil(2026, 4, 30, 0, 0, 60), None);
    }

    #[test]
    fn ticks_to_ms_uses_100hz() {
        assert_eq!(TICK_HZ, 100);
        assert_eq!(ticks_to_ms(0), 0);
        assert_eq!(ticks_to_ms(1), 10);
        assert_eq!(ticks_to_ms(100), 1000);
        assert_eq!(ticks_to_ms(12_345), 123_450);
        assert!(ticks_to_ms(u64::MAX) > 0); // saturates, no panic
        assert_eq!(ticks_from_ms(0), 0);
        assert_eq!(ticks_from_ms(1), 1); // rounded up: never shorter than asked
        assert_eq!(ticks_from_ms(10), 1);
        assert_eq!(ticks_from_ms(11), 2);
        assert_eq!(ticks_from_ms(5500), 550);
        assert!(ticks_from_ms(u64::MAX) > 0);
        assert_eq!(approx_seconds(0), 0);
        assert_eq!(approx_seconds(99), 0);
        assert_eq!(approx_seconds(100), 1);
        assert_eq!(approx_seconds(6000), 60);
        assert!(approx_seconds(u64::MAX) > 0);
        // PIT divisor for 100 Hz: 1193182 / 100 = 11931.82, rounded.
        assert_eq!(PIT_DIVISOR, 11_932);
    }
}
