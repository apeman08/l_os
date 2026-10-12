use crate::entropy::Rng;
use alloc::boxed::Box;
use rustls::crypto::{self, SharedSecret, SupportedKxGroup};

#[derive(Debug)]
pub struct X25519;

impl SupportedKxGroup for X25519 {
    fn name(&self) -> rustls::NamedGroup {
        rustls::NamedGroup::X25519
    }

    fn start(&self) -> Result<Box<dyn crypto::ActiveKeyExchange>, rustls::Error> {
        // Check the entropy source first so a failure is an error, not a panic.
        let mut probe = [0u8; 1];
        if !crate::entropy::fill(&mut probe) {
            return Err(rustls::Error::General("no entropy available".into()));
        }
        let priv_key = x25519_dalek::EphemeralSecret::random_from_rng(Rng);
        let pub_key = (&priv_key).into();
        Ok(Box::new(X25519Exchange { priv_key, pub_key }))
    }
}

struct X25519Exchange {
    priv_key: x25519_dalek::EphemeralSecret,
    pub_key: x25519_dalek::PublicKey,
}

impl crypto::ActiveKeyExchange for X25519Exchange {
    fn complete(self: Box<Self>, peer: &[u8]) -> Result<SharedSecret, rustls::Error> {
        let peer: [u8; 32] = peer
            .try_into()
            .map_err(|_| rustls::Error::from(rustls::PeerMisbehaved::InvalidKeyShare))?;
        let shared = self.priv_key.diffie_hellman(&peer.into());
        // An all-zero result means the peer sent a low-order point: reject.
        if !shared.was_contributory() {
            return Err(rustls::Error::from(rustls::PeerMisbehaved::InvalidKeyShare));
        }
        Ok(SharedSecret::from(shared.as_bytes().as_slice()))
    }

    fn pub_key(&self) -> &[u8] {
        self.pub_key.as_bytes()
    }

    fn group(&self) -> rustls::NamedGroup {
        rustls::NamedGroup::X25519
    }
}

#[derive(Debug)]
pub struct SecP256R1;

impl SupportedKxGroup for SecP256R1 {
    fn name(&self) -> rustls::NamedGroup {
        rustls::NamedGroup::secp256r1
    }

    fn start(&self) -> Result<Box<dyn crypto::ActiveKeyExchange>, rustls::Error> {
        let mut probe = [0u8; 1];
        if !crate::entropy::fill(&mut probe) {
            return Err(rustls::Error::General("no entropy available".into()));
        }
        let priv_key = p256::ecdh::EphemeralSecret::random(&mut Rng);
        let pub_key = p256::PublicKey::from(&priv_key).to_sec1_bytes();
        Ok(Box::new(P256Exchange { priv_key, pub_key }))
    }
}

struct P256Exchange {
    priv_key: p256::ecdh::EphemeralSecret,
    pub_key: Box<[u8]>,
}

impl crypto::ActiveKeyExchange for P256Exchange {
    fn complete(self: Box<Self>, peer: &[u8]) -> Result<SharedSecret, rustls::Error> {
        let their = p256::PublicKey::from_sec1_bytes(peer)
            .map_err(|_| rustls::Error::from(rustls::PeerMisbehaved::InvalidKeyShare))?;
        let shared = self.priv_key.diffie_hellman(&their);
        Ok(SharedSecret::from(shared.raw_secret_bytes().as_slice()))
    }

    fn pub_key(&self) -> &[u8] {
        &self.pub_key
    }

    fn group(&self) -> rustls::NamedGroup {
        rustls::NamedGroup::secp256r1
    }
}

pub const ALL_KX_GROUPS: &[&dyn SupportedKxGroup] = &[&X25519, &SecP256R1];
