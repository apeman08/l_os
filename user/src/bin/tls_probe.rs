//! Test program: HTTPS GET trusting the TEST CA from tests/fixtures/tls (never
//! the production root set). Arguments: `HOST PORT TARGET`.
//! Exit: HTTP status on success; 0x8000|-err for socket-level errors;
//! 0x9000|code for TLS failures (see `l_user::https::tls_code`).
#![no_std]
#![no_main]

extern crate alloc;

use l_user::{args, entry, https};

l_user::heap!(1024 * 1024);

static TEST_CA: &[u8] = include_bytes!("../../../tests/fixtures/tls/ca.der");

fn main() -> u16 {
    init_heap();
    let Ok(text) = core::str::from_utf8(args()) else { return 0x7000 };
    let mut it = text.split(' ');
    let (Some(host), Some(port), Some(target)) = (it.next(), it.next(), it.next()) else { return 0x7002 };
    let Ok(port) = port.parse::<u16>() else { return 0x7003 };
    let Ok(roots) = l_tls::roots_from_der([TEST_CA]) else { return 0x7004 };
    let Ok(config) = https::config(roots) else { return 0x7005 };

    let mut status = 0u16;
    let mut body = 0usize;
    let r = https::get(config, host, port, target, |head, _| status = head.status, |b| body += b.len());
    match r {
        Ok(()) if body > 0 => status,
        Ok(()) => 0x7006,
        Err(https::FetchError::Connect(c)) => 0x8000 | (-c) as u16,
        Err(https::FetchError::Tls(e)) => 0x9000 | https::tls_code(&e),
        Err(https::FetchError::Http(_)) => 0x9100,
    }
}

entry!(main);
