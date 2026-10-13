//! Holds the NetManager (handle 0) and System (handle 1) handles. Arguments:
//! `DELAY_MS HOST PORT`. After the delay it revokes that grant; exit code 1
//! if the grant existed.
#![no_std]
#![no_main]

use l_core::{
    syscall::{NETCTL_REVOKE, SYSINFO_TICKS},
    time::ticks_from_ms,
};
use l_user::{args, entry, netctl, sysinfo, yield_now};

fn main() -> u16 {
    let Ok(text) = core::str::from_utf8(args()) else { return 0x7000 };
    let mut it = text.split(' ');
    let (Some(delay), Some(host), Some(port)) = (it.next(), it.next(), it.next()) else { return 0x7002 };
    let (Ok(delay), Ok(port)) = (delay.parse::<u64>(), port.parse::<u64>()) else { return 0x7003 };

    let delay = ticks_from_ms(delay).min(i64::MAX as u64) as i64;
    let start = sysinfo(1, SYSINFO_TICKS);
    while sysinfo(1, SYSINFO_TICKS) - start < delay {
        yield_now();
    }
    let r = netctl(0, NETCTL_REVOKE, host.as_ptr() as u64, host.len() as u64, port);
    if r < 0 { 0x8000 | (-r) as u16 } else { r as u16 }
}

entry!(main);
