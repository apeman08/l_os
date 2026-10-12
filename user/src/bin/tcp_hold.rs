//! Connects to `HOST PORT` (arguments) through handle 0 and keeps sending
//! until something stops it. Exit code: 0x8000 | -error of the call that
//! failed (e.g. ERR_REVOKED when the grant was revoked or expired), or 0x7001
//! if it was never stopped.
#![no_std]
#![no_main]

use l_user::{args, entry, net_connect, sock_send, yield_now};

fn err(code: i64) -> u16 {
    0x8000 | (-code) as u16
}

fn main() -> u16 {
    let Ok(text) = core::str::from_utf8(args()) else { return 0x7000 };
    let mut it = text.split(' ');
    let (Some(host), Some(port)) = (it.next(), it.next()) else { return 0x7002 };
    let Ok(port) = port.parse::<u16>() else { return 0x7003 };

    let sock = net_connect(0, host, port);
    if sock < 0 {
        return err(sock);
    }
    for _ in 0..4000 {
        let r = sock_send(sock as u64, b"keep\n");
        if r < 0 {
            return err(r);
        }
        yield_now();
    }
    0x7001
}

entry!(main);
