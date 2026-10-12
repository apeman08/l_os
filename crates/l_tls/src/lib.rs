//! TLS 1.3 client for Larp OS.
//!
//! * `rustls` does the protocol; certificate validation is `rustls-webpki`.
//! * Cryptography comes from the RustCrypto crates; this crate only adapts
//!   them to rustls' provider interface (the glue is derived from
//!   `rustls-rustcrypto`, MIT OR Apache-2.0, RustCrypto Developers). No
//!   cryptographic primitive is implemented here.
//! * Client only, TLS 1.3 only: no TLS 1.2 or older, no client certificates,
//!   no signing keys. A server that cannot do TLS 1.3 is refused; there is
//!   never a downgrade and never a plaintext fallback.
//! * Randomness comes only from [`entropy::set_source`]; without it, no
//!   connection can be made.
#![cfg_attr(not(feature = "std"), no_std)]

extern crate alloc;
#[cfg(feature = "std")]
extern crate std;

use alloc::{sync::Arc, vec::Vec};
use rustls::{
    CipherSuite, ClientConfig, SupportedCipherSuite, Tls13CipherSuite,
    crypto::{CipherSuiteCommon, CryptoProvider, GetRandomFailed, KeyProvider, SecureRandom},
    time_provider::TimeProvider,
};
use rustls_pki_types::{PrivateKeyDer, UnixTime};

mod aead;
pub mod client;
pub mod entropy;
mod hash;
mod hmac;
mod kx;
mod verify;

pub use rustls::RootCertStore;
pub use rustls_pki_types::{CertificateDer, ServerName};

#[derive(Debug)]
struct Provider;

impl SecureRandom for Provider {
    fn fill(&self, bytes: &mut [u8]) -> Result<(), GetRandomFailed> {
        if entropy::fill(bytes) { Ok(()) } else { Err(GetRandomFailed) }
    }
}

impl KeyProvider for Provider {
    fn load_private_key(&self, _key: PrivateKeyDer<'static>) -> Result<Arc<dyn rustls::sign::SigningKey>, rustls::Error> {
        // This is a client-only provider: no client authentication.
        Err(rustls::Error::General("private keys are not supported".into()))
    }
}

pub const TLS13_AES_128_GCM_SHA256: SupportedCipherSuite = SupportedCipherSuite::Tls13(&Tls13CipherSuite {
    common: CipherSuiteCommon {
        suite: CipherSuite::TLS13_AES_128_GCM_SHA256,
        hash_provider: hash::SHA256,
        confidentiality_limit: 1 << 24,
    },
    hkdf_provider: &rustls::crypto::tls13::HkdfUsingHmac(hmac::SHA256),
    aead_alg: &aead::gcm::Tls13Aes128Gcm,
    quic: None,
});

pub const TLS13_AES_256_GCM_SHA384: SupportedCipherSuite = SupportedCipherSuite::Tls13(&Tls13CipherSuite {
    common: CipherSuiteCommon {
        suite: CipherSuite::TLS13_AES_256_GCM_SHA384,
        hash_provider: hash::SHA384,
        confidentiality_limit: 1 << 24,
    },
    hkdf_provider: &rustls::crypto::tls13::HkdfUsingHmac(hmac::SHA384),
    aead_alg: &aead::gcm::Tls13Aes256Gcm,
    quic: None,
});

pub const TLS13_CHACHA20_POLY1305_SHA256: SupportedCipherSuite = SupportedCipherSuite::Tls13(&Tls13CipherSuite {
    common: CipherSuiteCommon {
        suite: CipherSuite::TLS13_CHACHA20_POLY1305_SHA256,
        hash_provider: hash::SHA256,
        confidentiality_limit: u64::MAX,
    },
    hkdf_provider: &rustls::crypto::tls13::HkdfUsingHmac(hmac::SHA256),
    aead_alg: &aead::chacha20::Chacha20Poly1305,
    quic: None,
});

static CIPHER_SUITES: &[SupportedCipherSuite] =
    &[TLS13_AES_256_GCM_SHA384, TLS13_AES_128_GCM_SHA256, TLS13_CHACHA20_POLY1305_SHA256];

pub fn provider() -> CryptoProvider {
    CryptoProvider {
        cipher_suites: CIPHER_SUITES.to_vec(),
        kx_groups: kx::ALL_KX_GROUPS.to_vec(),
        signature_verification_algorithms: verify::ALGORITHMS,
        secure_random: &Provider,
        key_provider: &Provider,
    }
}

/// Wall-clock source for certificate validity: seconds since the Unix epoch,
/// or `None` if the clock is unavailable or implausible (then every
/// handshake fails rather than skipping the validity check).
pub type ClockFn = fn() -> Option<u64>;

#[derive(Debug)]
struct Clock(ClockFn);

impl TimeProvider for Clock {
    fn current_time(&self) -> Option<UnixTime> {
        (self.0)().map(|secs| UnixTime::since_unix_epoch(core::time::Duration::from_secs(secs)))
    }
}

/// Build a client configuration that trusts exactly `roots`.
pub fn client_config(roots: RootCertStore, clock: ClockFn) -> Result<Arc<ClientConfig>, rustls::Error> {
    let config = ClientConfig::builder_with_details(Arc::new(provider()), Arc::new(Clock(clock)))
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .with_root_certificates(roots)
        .with_no_client_auth();
    Ok(Arc::new(config))
}

/// Parse DER certificates into a root store; fails on any malformed entry.
pub fn roots_from_der<'a>(certs: impl IntoIterator<Item = &'a [u8]>) -> Result<RootCertStore, rustls::Error> {
    let mut store = RootCertStore::empty();
    let certs: Vec<CertificateDer<'static>> = certs.into_iter().map(|c| CertificateDer::from(c.to_vec())).collect();
    for cert in certs {
        store.add(cert)?;
    }
    Ok(store)
}
