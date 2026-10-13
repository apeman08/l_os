//! Wipe timing math and the machine-readable report line (spec: amnesisch).

use crate::wipe::{Stage, Trigger};
use core::fmt::Write;

fn sat(v: u128) -> u64 {
    u64::try_from(v).unwrap_or(u64::MAX)
}

/// TSC frequency from a cycle delta measured over `delta_ticks` timer ticks
/// of `tick_hz`. `None` when no ticks elapsed. Saturates at `u64::MAX`.
pub fn tsc_hz(delta_cycles: u64, delta_ticks: u64, tick_hz: u32) -> Option<u64> {
    if delta_ticks == 0 {
        return None;
    }
    Some(sat(delta_cycles as u128 * tick_hz as u128 / delta_ticks as u128))
}

/// Cycles to microseconds; saturating, `hz == 0` yields 0.
pub fn cycles_to_us(cycles: u64, hz: u64) -> u64 {
    if hz == 0 {
        return 0;
    }
    sat(cycles as u128 * 1_000_000 / hz as u128)
}

/// Throughput in MiB/s, rounded half up; `us == 0` yields 0.
pub fn mib_per_s(bytes: u64, us: u64) -> u64 {
    if us == 0 {
        return 0;
    }
    let num = bytes as u128 * 1_000_000;
    let den = us as u128 * (1 << 20);
    sat((num + den / 2) / den)
}

/// Cycle stamps per wipe stage. A stamp of 0 means "not marked".
#[derive(Clone, Copy, Debug)]
pub struct StageTimes {
    start: u64,
    at: [u64; 14],
}

impl StageTimes {
    pub const fn new() -> Self {
        StageTimes { start: 0, at: [0; 14] }
    }
    pub fn start(&mut self, cycles: u64) {
        self.start = cycles;
    }
    pub fn mark(&mut self, stage: Stage, cycles: u64) {
        // 0 is the "unmarked" sentinel, so a literal 0 is stored as 1.
        self.at[stage as usize] = cycles.max(1);
    }
    pub fn us_since_start(&self, stage: Stage, hz: u64) -> Option<u64> {
        let at = self.at[stage as usize];
        if at == 0 {
            return None;
        }
        Some(cycles_to_us(at.saturating_sub(self.start), hz))
    }
}

impl Default for StageTimes {
    fn default() -> Self {
        Self::new()
    }
}

/// Fixed buffer writer that silently truncates.
struct Trunc<'a> {
    buf: &'a mut [u8],
    len: usize,
}

impl Write for Trunc<'_> {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        let n = s.len().min(self.buf.len() - self.len);
        self.buf[self.len..self.len + n].copy_from_slice(&s.as_bytes()[..n]);
        self.len += n;
        Ok(())
    }
}

/// Writes the `WIPE-REPORT` line (parsed by the QEMU test, keep stable) into
/// `out`, truncating if too small. Returns bytes written. Unmarked stages
/// report 0; `total_us` is the time to `KernelZeroed`.
pub fn report_line(times: &StageTimes, trigger: Trigger, hz: u64, bytes: u64, out: &mut [u8]) -> usize {
    let us = |s: Stage| times.us_since_start(s, hz).unwrap_or(0);
    let mut w = Trunc { buf: out, len: 0 };
    let _ = write!(
        w,
        "WIPE-REPORT trigger={} keys_us={} user_us={} allocated_us={} usable_us={} kernel_us={} total_us={} bytes={}\n",
        trigger.as_str(),
        us(Stage::KeysZeroed),
        us(Stage::UserZeroed),
        us(Stage::AllocatedZeroed),
        us(Stage::UsableZeroed),
        us(Stage::KernelZeroed),
        us(Stage::KernelZeroed),
        bytes,
    );
    w.len
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tsc_hz_from_100hz_ticks() {
        assert_eq!(tsc_hz(2_000_000_000, 100, 100), Some(2_000_000_000));
    }

    #[test]
    fn tsc_hz_zero_ticks_is_none() {
        assert_eq!(tsc_hz(1, 0, 100), None);
        assert_eq!(tsc_hz(u64::MAX, u64::MAX, 0), Some(0));
        // Saturates instead of overflowing.
        assert_eq!(tsc_hz(u64::MAX, 1, u32::MAX), Some(u64::MAX));
    }

    #[test]
    fn cycles_to_us_no_overflow_at_u64_max() {
        assert_eq!(cycles_to_us(u64::MAX, 1), u64::MAX);
        assert_eq!(cycles_to_us(u64::MAX, 1_000_000), u64::MAX);
        assert_eq!(cycles_to_us(2_000_000_000, 2_000_000_000), 1_000_000);
        assert_eq!(cycles_to_us(5, 0), 0);
        assert_eq!(cycles_to_us(0, 7), 0);
    }

    #[test]
    fn mib_per_s_rounding() {
        // 1 MiB in 1 s.
        assert_eq!(mib_per_s(1 << 20, 1_000_000), 1);
        // 1 GiB in 0.5 s = 2048 MiB/s.
        assert_eq!(mib_per_s(1 << 30, 500_000), 2048);
        // 1.5 MiB/s rounds half up to 2; 1.4 rounds down to 1.
        assert_eq!(mib_per_s(3 << 19, 1_000_000), 2);
        assert_eq!(mib_per_s(1_468_006, 1_000_000), 1);
        assert_eq!(mib_per_s(100, 0), 0);
        assert_eq!(mib_per_s(u64::MAX, 1), 17_592_186_044_415_999_999); // no overflow
        assert_eq!(mib_per_s(u64::MAX, u64::MAX), 1);
    }

    #[test]
    fn stage_times_relative_to_start() {
        let mut t = StageTimes::new();
        t.start(1_000);
        t.mark(Stage::KeysZeroed, 3_000);
        assert_eq!(t.us_since_start(Stage::KeysZeroed, 1_000_000), Some(2_000));
        // Mark before start saturates to 0.
        t.mark(Stage::UserZeroed, 500);
        assert_eq!(t.us_since_start(Stage::UserZeroed, 1_000_000), Some(0));
    }

    #[test]
    fn unmarked_stage_is_none() {
        let mut t = StageTimes::new();
        t.start(10);
        assert_eq!(t.us_since_start(Stage::Halted, 1_000_000), None);
    }

    fn sample() -> StageTimes {
        let mut t = StageTimes::new();
        t.start(1_000);
        t.mark(Stage::KeysZeroed, 1_000 + 10);
        t.mark(Stage::UserZeroed, 1_000 + 20);
        t.mark(Stage::AllocatedZeroed, 1_000 + 30);
        t.mark(Stage::UsableZeroed, 1_000 + 40);
        t.mark(Stage::KernelZeroed, 1_000 + 50);
        t
    }

    const LINE: &str = "WIPE-REPORT trigger=hotkey keys_us=10 user_us=20 allocated_us=30 usable_us=40 kernel_us=50 total_us=50 bytes=4096\n";

    #[test]
    fn report_line_format_is_stable() {
        let mut buf = [0u8; 256];
        let n = report_line(&sample(), Trigger::Hotkey, 1_000_000, 4096, &mut buf);
        assert_eq!(core::str::from_utf8(&buf[..n]).unwrap(), LINE);
    }

    #[test]
    fn report_line_unmarked_stages_are_zero_and_hz_zero_ok() {
        let mut buf = [0u8; 256];
        let n = report_line(&StageTimes::new(), Trigger::StickRemoved, 0, 0, &mut buf);
        assert_eq!(
            core::str::from_utf8(&buf[..n]).unwrap(),
            "WIPE-REPORT trigger=unplug keys_us=0 user_us=0 allocated_us=0 usable_us=0 kernel_us=0 total_us=0 bytes=0\n"
        );
    }

    #[test]
    fn report_line_truncates_without_panic() {
        for cap in 0..LINE.len() + 4 {
            let mut buf = alloc::vec![0u8; cap];
            let n = report_line(&sample(), Trigger::Hotkey, 1_000_000, 4096, &mut buf);
            assert_eq!(n, cap.min(LINE.len()));
            assert_eq!(&buf[..n], &LINE.as_bytes()[..n]);
        }
    }
}
