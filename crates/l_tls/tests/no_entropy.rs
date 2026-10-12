//! Separate test binary: no entropy source is registered here, so TLS must
//! refuse to start rather than fall back to anything weaker.
mod common;

use common::*;
use l_tls::{client::TlsStream, roots_from_der};

#[test]
fn without_entropy_no_connection_is_attempted() {
    let srv = s_server("server_ec.pem", "server_ec.key", "-tls1_3");
    let roots = roots_from_der([fixture("ca.der").as_slice()]).unwrap();
    let config = l_tls::client_config(roots, clock_now).unwrap();
    let r = TlsStream::connect(config, "test.example", connect_tcp(srv.port));
    assert!(r.is_err());
}
