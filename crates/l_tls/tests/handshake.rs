//! Interoperability and failure-mode tests against a real TLS server
//! (`openssl s_server`). Run with: scripts/host_tests.sh
mod common;

use common::*;
use l_tls::{
    client::{Error, TlsStream},
    entropy, roots_from_der,
};
use rustls::{CertificateError, Error as TlsError};
use std::sync::Once;

static INIT: Once = Once::new();
fn init() {
    INIT.call_once(|| entropy::set_source(os_entropy));
}

fn config(clock: l_tls::ClockFn) -> std::sync::Arc<rustls::ClientConfig> {
    init();
    let roots = roots_from_der([fixture("ca.der").as_slice()]).unwrap();
    l_tls::client_config(roots, clock).unwrap()
}

fn get(stream: &mut TlsStream<Tcp>) -> String {
    stream.write_all(b"GET / HTTP/1.0\r\n\r\n").unwrap();
    let mut out = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        match stream.read(&mut buf).unwrap() {
            0 => break,
            n => out.extend_from_slice(&buf[..n]),
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn expect_cert_error(result: Result<TlsStream<Tcp>, Error>, check: impl Fn(&CertificateError) -> bool, what: &str) {
    match result {
        Err(Error::Tls(TlsError::InvalidCertificate(e))) if check(&e) => {}
        Err(other) => panic!("{what}: expected a certificate error, got {other:?}"),
        Ok(_) => panic!("{what}: the connection must not succeed"),
    }
}

#[test]
fn ecdsa_server_handshake_and_http_exchange() {
    let srv = s_server("server_ec.pem", "server_ec.key", "-tls1_3");
    let mut s = TlsStream::connect(config(clock_now), "test.example", connect_tcp(srv.port)).unwrap();
    let page = get(&mut s);
    assert!(page.contains("200 ok"), "{page}");
    assert!(page.contains("TLSv1.3"), "{page}");
}

#[test]
fn rsa_server_handshake_exercises_rsa_pss_verification() {
    let srv = s_server("server_rsa.pem", "server_rsa.key", "-tls1_3");
    let mut s = TlsStream::connect(config(clock_now), "test.example", connect_tcp(srv.port)).unwrap();
    assert!(get(&mut s).contains("200 ok"));
}

#[test]
fn large_response_is_read_completely() {
    // s_server -www on /?... serves a fixed page; send many requests on fresh
    // connections to exercise buffer handling repeatedly.
    let srv = s_server("server_ec.pem", "server_ec.key", "-tls1_3");
    for _ in 0..5 {
        let mut s = TlsStream::connect(config(clock_now), "test.example", connect_tcp(srv.port)).unwrap();
        assert!(get(&mut s).len() > 500);
    }
}

#[test]
fn certificate_for_another_name_is_rejected() {
    let srv = s_server("server_wrongname.pem", "server_wrongname.key", "-tls1_3");
    let r = TlsStream::connect(config(clock_now), "test.example", connect_tcp(srv.port));
    expect_cert_error(r, |e| matches!(e, CertificateError::NotValidForName | CertificateError::NotValidForNameContext { .. }), "wrong name");
}

#[test]
fn asking_for_a_different_name_than_the_certificate_is_rejected() {
    let srv = s_server("server_ec.pem", "server_ec.key", "-tls1_3");
    let r = TlsStream::connect(config(clock_now), "evil.example", connect_tcp(srv.port));
    expect_cert_error(r, |e| matches!(e, CertificateError::NotValidForName | CertificateError::NotValidForNameContext { .. }), "name mismatch");
}

#[test]
fn certificate_from_an_untrusted_ca_is_rejected() {
    let srv = s_server("server_rogue.pem", "server_rogue.key", "-tls1_3");
    let r = TlsStream::connect(config(clock_now), "test.example", connect_tcp(srv.port));
    expect_cert_error(r, |e| matches!(e, CertificateError::UnknownIssuer), "rogue CA");
}

#[test]
fn self_signed_certificate_is_rejected() {
    let srv = s_server("server_selfsigned.pem", "server_selfsigned.key", "-tls1_3");
    let r = TlsStream::connect(config(clock_now), "test.example", connect_tcp(srv.port));
    expect_cert_error(r, |e| matches!(e, CertificateError::UnknownIssuer), "self-signed");
}

#[test]
fn expired_and_not_yet_valid_certificates_are_rejected() {
    let srv = s_server("server_ec.pem", "server_ec.key", "-tls1_3");
    let far_future: l_tls::ClockFn = || Some(7_000_000_000); // year 2191, after notAfter (2126)
    let r = TlsStream::connect(config(far_future), "test.example", connect_tcp(srv.port));
    expect_cert_error(r, |e| matches!(e, CertificateError::Expired | CertificateError::ExpiredContext { .. }), "expired");

    let past: l_tls::ClockFn = || Some(946_684_800); // 2000-01-01, before notBefore
    let r = TlsStream::connect(config(past), "test.example", connect_tcp(srv.port));
    expect_cert_error(r, |e| matches!(e, CertificateError::NotValidYet | CertificateError::NotValidYetContext { .. }), "not yet valid");
}

#[test]
fn without_a_clock_validation_fails_closed() {
    let srv = s_server("server_ec.pem", "server_ec.key", "-tls1_3");
    let no_clock: l_tls::ClockFn = || None;
    let r = TlsStream::connect(config(no_clock), "test.example", connect_tcp(srv.port));
    assert!(r.is_err(), "a missing clock must never skip the validity check");
}

#[test]
fn tls12_only_server_is_refused_without_downgrade() {
    let srv = s_server("server_ec.pem", "server_ec.key", "-tls1_2");
    let r = TlsStream::connect(config(clock_now), "test.example", connect_tcp(srv.port));
    assert!(matches!(r, Err(Error::Tls(_))), "got {:?}", r.err());
}

#[test]
fn plaintext_server_gets_only_a_client_hello_never_http() {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let seen = std::thread::spawn(move || {
        let (mut c, _) = listener.accept().unwrap();
        let mut got = vec![0u8; 4096];
        let n = c.read(&mut got).unwrap_or(0);
        got.truncate(n);
        let _ = c.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nhello");
        got
    });
    let r = TlsStream::connect(config(clock_now), "test.example", connect_tcp(port));
    assert!(matches!(r, Err(Error::Tls(_)) | Err(Error::ClosedDuringHandshake)), "got {:?}", r.err());
    let got = seen.join().unwrap();
    assert_eq!(&got[..2], &[0x16, 0x03], "first bytes must be a TLS handshake record");
    assert!(!got.windows(3).any(|w| w == b"GET"), "no plaintext request may ever be sent");
}

#[test]
fn connection_cut_mid_stream_is_an_error_not_a_clean_end() {
    use std::io::{Read, Write};
    // A relay that forwards the server's bytes but stops after the handshake
    // plus a few application bytes, then drops the connection (no close_notify).
    let srv = s_server("server_ec.pem", "server_ec.key", "-tls1_3");
    let relay = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let relay_port = relay.local_addr().unwrap().port();
    let upstream = srv.port;
    std::thread::spawn(move || {
        let (mut client, _) = relay.accept().unwrap();
        let mut server = std::net::TcpStream::connect(("127.0.0.1", upstream)).unwrap();
        let (mut c2, mut s2) = (client.try_clone().unwrap(), server.try_clone().unwrap());
        std::thread::spawn(move || {
            let mut b = [0u8; 4096];
            while let Ok(n) = c2.read(&mut b) {
                if n == 0 || s2.write_all(&b[..n]).is_err() {
                    break;
                }
            }
        });
        let mut total = 0usize;
        let mut b = [0u8; 4096];
        while let Ok(n) = server.read(&mut b) {
            if n == 0 {
                break;
            }
            total += n;
            if client.write_all(&b[..n]).is_err() {
                break;
            }
            if total > 2500 {
                break; // cut: drop both sockets without TLS close
            }
        }
    });
    let outcome = (|| -> Result<Vec<u8>, Error> {
        let mut s = TlsStream::connect(config(clock_now), "test.example", connect_tcp(relay_port))?;
        s.write_all(b"GET / HTTP/1.0\r\n\r\n")?;
        let mut out = Vec::new();
        let mut buf = [0u8; 4096];
        loop {
            match s.read(&mut buf)? {
                0 => return Ok(out),
                n => out.extend_from_slice(&buf[..n]),
            }
        }
    })();
    assert!(outcome.is_err(), "a cut stream must never look like a complete response: {outcome:?}");
}
