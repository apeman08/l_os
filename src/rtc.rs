//! Wall-clock time from the CMOS real-time clock.
//!
//! The RTC is set by firmware/the hypervisor and is NOT authenticated: under
//! QEMU it is the host's clock. Callers (TLS certificate validity) get `None`
//! when the value is implausible and must then refuse rather than guess.

use l_core::time::unix_from_civil;
use x86_64::instructions::port::Port;

fn cmos(reg: u8) -> u8 {
    let mut index = Port::<u8>::new(0x70);
    let mut data = Port::<u8>::new(0x71);
    // SAFETY: 0x70/0x71 are the CMOS index/data ports; reading a register
    // has no side effect besides selecting it (NMI mask bit left clear).
    unsafe {
        index.write(reg);
        data.read()
    }
}

fn update_in_progress() -> bool {
    cmos(0x0a) & 0x80 != 0
}

fn bcd(v: u8) -> u32 {
    ((v >> 4) as u32) * 10 + (v & 0x0f) as u32
}

struct Raw {
    sec: u8,
    min: u8,
    hour: u8,
    day: u8,
    month: u8,
    year: u8,
    century: u8,
}

fn read_raw() -> Raw {
    Raw {
        sec: cmos(0x00),
        min: cmos(0x02),
        hour: cmos(0x04),
        day: cmos(0x07),
        month: cmos(0x08),
        year: cmos(0x09),
        century: cmos(0x32),
    }
}

/// Seconds since the Unix epoch (UTC), or `None` if the RTC looks unset/invalid.
pub fn unix_time() -> Option<u64> {
    // Read until two consecutive reads agree and no update is in progress.
    let mut spins = 0;
    let raw = loop {
        while update_in_progress() {
            spins += 1;
            if spins > 1_000_000 {
                return None;
            }
        }
        let a = read_raw();
        if update_in_progress() {
            continue;
        }
        let b = read_raw();
        if (a.sec, a.min, a.hour, a.day, a.month, a.year) == (b.sec, b.min, b.hour, b.day, b.month, b.year) {
            break b;
        }
        spins += 1;
        if spins > 1_000_000 {
            return None;
        }
    };
    let status_b = cmos(0x0b);
    let binary = status_b & 0x04 != 0;
    let hour24 = status_b & 0x02 != 0;
    let conv = |v: u8| if binary { v as u32 } else { bcd(v) };

    let mut hour = if binary { (raw.hour & 0x7f) as u32 } else { bcd(raw.hour & 0x7f) };
    if !hour24 {
        let pm = raw.hour & 0x80 != 0;
        hour = hour % 12 + if pm { 12 } else { 0 };
    }
    let century = conv(raw.century);
    let year = if (19..=21).contains(&century) { century * 100 + conv(raw.year) } else { 2000 + conv(raw.year) };
    unix_from_civil(year, conv(raw.month), conv(raw.day), hour, conv(raw.min), conv(raw.sec))
}
