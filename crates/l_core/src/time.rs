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

/// Timer ticks needed to cover `seconds` (rounded up).
pub fn ticks_from_seconds(seconds: u64) -> u64 {
    seconds.saturating_mul(PIT_BASE_HZ).div_ceil(PIT_DEFAULT_DIVISOR)
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
        assert_eq!(ticks_from_seconds(1), 19); // 18.2 Hz, rounded up
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
    fn matches_default_pit_rate() {
        assert_eq!(approx_seconds(0), 0);
        assert_eq!(approx_seconds(18), 0);
        assert_eq!(approx_seconds(19), 1);
        assert!(approx_seconds(u64::MAX) > 0); // saturates, no panic
    }
}
