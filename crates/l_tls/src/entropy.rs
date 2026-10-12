//! The single source of randomness for all TLS key generation.
//!
//! The embedding program registers a function once (on Larp OS: a syscall to
//! the kernel's hardware RNG). If none is registered, or the source fails,
//! key generation fails and no connection is made: there is no fallback to a
//! weaker generator.

use rand_core::{CryptoRng, Error, RngCore};

/// Fills the buffer completely and returns `true`, or returns `false` on failure.
pub type FillFn = fn(&mut [u8]) -> bool;

static SOURCE: spin::Once<FillFn> = spin::Once::new();

/// Register the entropy source. Only the first registration takes effect.
pub fn set_source(f: FillFn) {
    SOURCE.call_once(|| f);
}

pub fn fill(buf: &mut [u8]) -> bool {
    match SOURCE.get() {
        Some(f) => f(buf),
        None => false,
    }
}

/// Adapter so RustCrypto key generation can draw from [`fill`].
pub struct Rng;

impl RngCore for Rng {
    fn next_u32(&mut self) -> u32 {
        let mut b = [0u8; 4];
        self.fill_bytes(&mut b);
        u32::from_le_bytes(b)
    }
    fn next_u64(&mut self) -> u64 {
        let mut b = [0u8; 8];
        self.fill_bytes(&mut b);
        u64::from_le_bytes(b)
    }
    fn fill_bytes(&mut self, dest: &mut [u8]) {
        // The infallible interface cannot report failure; refuse to continue
        // with predictable bytes instead of returning zeros.
        if !fill(dest) {
            panic!("entropy source failed");
        }
    }
    fn try_fill_bytes(&mut self, dest: &mut [u8]) -> Result<(), Error> {
        if fill(dest) { Ok(()) } else { Err(Error::from(core::num::NonZeroU32::new(Error::CUSTOM_START).expect("non-zero"))) }
    }
}

impl CryptoRng for Rng {}
