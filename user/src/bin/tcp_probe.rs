//! Connects to `HOST PORT MESSAGE...` (from its arguments) through handle 0
//! (a NetClient), sends the message, and checks the server echoes it back.
//!
//! Exit code: the echoed length on success; 0x7777 if the echo differs;
//! 0x8000 | -error for any syscall error (so tests can assert the exact error).
#![no_std]
#![no_main]

use l_user::{args, close, entry, net_connect, sock_recv, sock_send};

const NET: u64 = 0;

fn err(code: i64) -> u16 {
    0x8000 | (-code) as u16
}

fn main() -> u16 {
    let a = args();
    let Ok(text) = core::str::from_utf8(a) else { return 0x7001 };
    let mut it = text.splitn(3, ' ');
    let (Some(host), Some(port), Some(msg)) = (it.next(), it.next(), it.next()) else { return 0x7002 };
    let Ok(port) = port.parse::<u16>() else { return 0x7003 };

    let sock = net_connect(NET, host, port);
    if sock < 0 {
        return err(sock);
    }
    let sock = sock as u64;
    let mut sent = 0;
    while sent < msg.len() {
        let r = sock_send(sock, &msg.as_bytes()[sent..]);
        if r < 0 {
            return err(r);
        }
        sent += r as usize;
    }
    let mut got = [0u8; 256];
    let mut n = 0;
    while n < msg.len() {
        let r = sock_recv(sock, &mut got[n..]);
        if r < 0 {
            return err(r);
        }
        if r == 0 {
            break;
        }
        n += r as usize;
    }
    close(sock);
    if &got[..n] == msg.as_bytes() { n as u16 } else { 0x7777 }
}

entry!(main);
