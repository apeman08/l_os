//! `fetch https://HOST[:PORT]/PATH` - one HTTPS GET, shown as text.
//!
//! * https only; a failed TLS handshake is an error, never a fallback.
//! * Redirects are shown, not followed: a new destination needs its own
//!   authorization (`net request`).
//! * The body is sanitized before it reaches the terminal.
#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec::Vec;
use l_core::{
    http::sanitize,
    syscall::{DENIED_DNS_RESOLVER, DENIED_EXPIRED, DENIED_INVALID_DESTINATION, DENIED_NO_GRANT, ERR_CONNECT_FAILED, ERR_DNS_FAILED, ERR_NO_DEVICE, ERR_TIMEOUT},
    url::{self, UrlError},
};
use l_user::{Console, args, entry, h, https, println};

l_user::heap!(1024 * 1024);

const MAX_SHOWN: usize = 12 * 1024;

fn main() -> u16 {
    init_heap();
    let mut out = Console(h::CONSOLE);
    let Ok(arg) = core::str::from_utf8(args()) else {
        println!(out, "fetch: the argument is not text");
        return 1;
    };
    let arg = arg.trim_matches(' ');
    let url = match url::parse(arg) {
        Ok(u) => u,
        Err(UrlError::NotHttps) => {
            println!(out, "fetch: only https:// URLs are supported (plain http:// is refused, never tried)");
            return 1;
        }
        Err(e) => {
            println!(out, "fetch: invalid URL ({:?})", e);
            return 1;
        }
    };

    let roots = l_tls::RootCertStore { roots: webpki_roots::TLS_SERVER_ROOTS.to_vec() };
    let config = match https::config(roots) {
        Ok(c) => c,
        Err(_) => {
            println!(out, "fetch: cannot set up TLS");
            return 6;
        }
    };

    println!(out, "fetch {}:{}{}", url.host, url.port, url.target);
    let mut shown = 0usize;
    let mut total = 0usize;
    let mut line: Vec<u8> = Vec::new();
    let result = https::get(
        config,
        url.host,
        url.port,
        url.target,
        |head, tls| {
            println!(out, "TLS {} ({}), certificate verified for {}", tls.version, tls.suite, url.host);
            println!(out, "HTTP {} {}", head.status, head.reason);
            if let Some(ct) = head.content_type {
                let ct: Vec<u8> = ct.bytes().map(sanitize).collect();
                println!(out, "type: {}", core::str::from_utf8(&ct).unwrap_or("?"));
            }
            if let Some(loc) = head.location {
                let loc: Vec<u8> = loc.bytes().map(sanitize).take(200).collect();
                println!(out, "redirect to: {} (not followed; authorize the new host with 'net request' and run fetch again)", core::str::from_utf8(&loc).unwrap_or("?"));
            }
            println!(out, "---");
        },
        |body| {
            total += body.len();
            for &b in body {
                if shown >= MAX_SHOWN {
                    break;
                }
                shown += 1;
                line.push(sanitize(b));
                if b == b'\n' || line.len() >= 78 {
                    if b != b'\n' {
                        line.push(b'\n');
                    }
                    l_user::write(h::CONSOLE, &line);
                    line.clear();
                }
            }
        },
    );
    if !line.is_empty() {
        line.push(b'\n');
        l_user::write(h::CONSOLE, &line);
    }
    match result {
        Ok(()) => {
            println!(out, "---");
            if total > shown {
                println!(out, "[{} of {} bytes shown]", shown, total);
            } else {
                println!(out, "[{} bytes]", total);
            }
            0
        }
        Err(https::FetchError::Connect(code)) => {
            match code {
                DENIED_NO_GRANT | DENIED_EXPIRED => {
                    println!(out, "not authorized: run 'net request {}:{}' first", url.host, url.port);
                    2
                }
                DENIED_DNS_RESOLVER => {
                    println!(out, "not authorized: the DNS resolver is not allowed (see 'net resolver', then 'net request IP:PORT')");
                    2
                }
                DENIED_INVALID_DESTINATION => {
                    println!(out, "invalid destination");
                    1
                }
                ERR_DNS_FAILED => {
                    println!(out, "the name could not be resolved");
                    3
                }
                ERR_CONNECT_FAILED | ERR_TIMEOUT | ERR_NO_DEVICE => {
                    println!(out, "could not connect (error {})", code);
                    4
                }
                _ => {
                    println!(out, "connection failed (error {})", code);
                    4
                }
            }
        }
        Err(https::FetchError::Tls(e)) => {
            println!(out, "TLS error, nothing was shown: {}", https::tls_reason(&e));
            if https::tls_code(&e) <= 5 { 5 } else { 6 }
        }
        Err(https::FetchError::Http(_)) => {
            println!(out, "the server's reply was not valid HTTP");
            7
        }
    }
}

entry!(main);
