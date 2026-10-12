//! HTTPS GET over a kernel socket: TLS 1.3 from `l_tls`, plain HTTP/1.0 on top.
//!
//! Handle layout every network program expects (the shell grants them in this
//! order, tests do too): console 0, NetClient 1, Random 2, System 3.

use crate::{net_connect, random, sock_recv, sock_send, close, sysinfo};
use alloc::{string::String, sync::Arc, vec::Vec};
use core::sync::atomic::{AtomicU64, Ordering};
use l_core::{
    http::{self, Head, HttpError},
    syscall::{SYSINFO_WALL_CLOCK, ERR_NO_CLOCK},
};
use l_tls::client::{Error as TlsError, TlsStream, Transport, TransportError};
use l_tls::rustls::{CertificateError, ClientConfig, Error as RustlsError};

pub const H_NET: u64 = 1;
pub const H_RANDOM: u64 = 2;
pub const H_SYSTEM: u64 = 3;

static RANDOM_HANDLE: AtomicU64 = AtomicU64::new(H_RANDOM);
static SYSTEM_HANDLE: AtomicU64 = AtomicU64::new(H_SYSTEM);

fn entropy(buf: &mut [u8]) -> bool {
    random(RANDOM_HANDLE.load(Ordering::Relaxed), buf) >= 0
}

fn clock() -> Option<u64> {
    let r = sysinfo(SYSTEM_HANDLE.load(Ordering::Relaxed), SYSINFO_WALL_CLOCK);
    // ERR_NO_CLOCK or any other error: no time, no certificate validation.
    if r > 0 && r != ERR_NO_CLOCK { Some(r as u64) } else { None }
}

/// Register the kernel RNG with the TLS stack and build a client config that
/// trusts exactly `roots`.
pub fn config(roots: l_tls::RootCertStore) -> Result<Arc<ClientConfig>, RustlsError> {
    l_tls::entropy::set_source(entropy);
    l_tls::client_config(roots, clock)
}

pub struct SocketTransport(pub u64);

impl Transport for SocketTransport {
    fn send(&mut self, data: &[u8]) -> Result<usize, TransportError> {
        let r = sock_send(self.0, data);
        if r > 0 { Ok(r as usize) } else { Err(TransportError(r)) }
    }
    fn recv(&mut self, buf: &mut [u8]) -> Result<usize, TransportError> {
        let r = sock_recv(self.0, buf);
        if r >= 0 { Ok(r as usize) } else { Err(TransportError(r)) }
    }
}

#[derive(Debug)]
pub enum FetchError {
    /// `net_connect` failed: carries the (negative) kernel error code.
    Connect(i64),
    Tls(TlsError),
    Http(HttpError),
}

pub struct TlsInfo {
    pub version: String,
    pub suite: String,
}

/// GET `target` from `host:port`. `on_head` sees the parsed head and the TLS
/// parameters; `on_body` is called with raw body bytes as they arrive.
pub fn get(
    config: Arc<ClientConfig>,
    host: &str,
    port: u16,
    target: &str,
    mut on_head: impl FnMut(&Head<'_>, &TlsInfo),
    mut on_body: impl FnMut(&[u8]),
) -> Result<(), FetchError> {
    let sock = net_connect(H_NET, host, port);
    if sock < 0 {
        return Err(FetchError::Connect(sock));
    }
    let sock = sock as u64;
    let result = (|| {
        let mut tls = TlsStream::connect(config, host, SocketTransport(sock)).map_err(FetchError::Tls)?;
        let info = tls
            .negotiated()
            .map(|(version, suite)| TlsInfo { version, suite })
            .unwrap_or(TlsInfo { version: String::from("?"), suite: String::from("?") });

        let mut req = [0u8; 700];
        let n = http::build_request(host, target, &mut req).ok_or(FetchError::Http(HttpError::TooLarge))?;
        tls.write_all(&req[..n]).map_err(FetchError::Tls)?;

        let mut head_buf: Vec<u8> = Vec::new();
        let mut head_done = false;
        let mut chunk = [0u8; 1024];
        loop {
            let n = tls.read(&mut chunk).map_err(FetchError::Tls)?;
            if n == 0 {
                break;
            }
            if head_done {
                on_body(&chunk[..n]);
                continue;
            }
            head_buf.extend_from_slice(&chunk[..n]);
            match http::parse_head(&head_buf) {
                Ok(head) => {
                    on_head(&head, &info);
                    on_body(&head_buf[head.head_len..]);
                    head_done = true;
                }
                Err(HttpError::Incomplete) => {}
                Err(e) => return Err(FetchError::Http(e)),
            }
        }
        if !head_done {
            return Err(FetchError::Http(HttpError::Incomplete));
        }
        tls.close();
        Ok(())
    })();
    close(sock);
    result
}

/// A short, stable code for how a TLS failure should be reported.
pub fn tls_code(e: &TlsError) -> u16 {
    match e {
        TlsError::Tls(RustlsError::InvalidCertificate(c)) => match c {
            CertificateError::UnknownIssuer => 1,
            CertificateError::NotValidForName | CertificateError::NotValidForNameContext { .. } => 2,
            CertificateError::Expired | CertificateError::ExpiredContext { .. } => 3,
            CertificateError::NotValidYet | CertificateError::NotValidYetContext { .. } => 4,
            _ => 5,
        },
        TlsError::Tls(_) => 6,
        TlsError::TruncatedStream | TlsError::ClosedDuringHandshake => 7,
        TlsError::Transport(_) => 8,
        TlsError::TooMuchData => 9,
    }
}

/// Human-readable reason for a TLS failure.
pub fn tls_reason(e: &TlsError) -> &'static str {
    match tls_code(e) {
        1 => "the certificate was issued by an authority this system does not trust",
        2 => "the certificate is not valid for this host name",
        3 => "the certificate has expired",
        4 => "the certificate is not yet valid",
        5 => "the certificate was rejected",
        6 => "the TLS handshake failed (server may not support TLS 1.3)",
        7 => "the connection was closed before the response was complete",
        8 => "the network connection failed during TLS",
        _ => "the server sent too much data",
    }
}
