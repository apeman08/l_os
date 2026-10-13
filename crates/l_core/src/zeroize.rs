//! Best-effort memory zeroing helpers (no `unsafe`).
//!
//! Limitation: with `#![forbid(unsafe_code)]` we cannot use volatile writes.
//! `zeroize` writes zeros, issues a `compiler_fence(SeqCst)` and passes the
//! buffer through `core::hint::black_box`. `black_box` is documented as a
//! best-effort hint, not a guarantee, but in practice it keeps the stores from
//! being eliminated as dead. Callers that need a hard guarantee must wipe via
//! kernel code using volatile writes.

use core::sync::atomic::{compiler_fence, Ordering};

pub fn zeroize(buf: &mut [u8]) {
    for b in buf.iter_mut() {
        *b = 0;
    }
    compiler_fence(Ordering::SeqCst);
    core::hint::black_box(&*buf);
}

pub fn is_zero(buf: &[u8]) -> bool {
    buf.iter().fold(0u8, |a, b| a | *b) == 0
}

/// Fixed-size secret that wipes itself on drop. Deliberately not `Clone`/`Copy`.
pub struct Secret<const N: usize> {
    bytes: [u8; N],
}

impl<const N: usize> Secret<N> {
    pub fn new() -> Self {
        Secret { bytes: [0u8; N] }
    }
    pub fn expose(&self) -> &[u8; N] {
        &self.bytes
    }
    pub fn expose_mut(&mut self) -> &mut [u8; N] {
        &mut self.bytes
    }
    pub fn wipe(&mut self) {
        zeroize(&mut self.bytes);
    }
}

impl<const N: usize> Default for Secret<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> Drop for Secret<N> {
    fn drop(&mut self) {
        self.wipe();
    }
}

impl<const N: usize> core::fmt::Debug for Secret<N> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "Secret<{}>([redacted])", N)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    extern crate std;
    use std::format;

    #[test]
    fn zeroize_clears_all_bytes() {
        let mut b = [0xAAu8; 257];
        zeroize(&mut b);
        assert!(b.iter().all(|x| *x == 0));
        zeroize(&mut []);
    }

    #[test]
    fn is_zero_detects_single_byte() {
        let mut b = [0u8; 64];
        assert!(is_zero(&b));
        assert!(is_zero(&[]));
        for i in [0, 31, 63] {
            b[i] = 1;
            assert!(!is_zero(&b));
            b[i] = 0;
        }
    }

    #[test]
    fn secret_wipe_clears() {
        let mut s = Secret::<32>::new();
        assert!(is_zero(s.expose()));
        s.expose_mut().copy_from_slice(&[7u8; 32]);
        assert_eq!(s.expose(), &[7u8; 32]);
        s.wipe();
        assert!(is_zero(s.expose()));
    }

    #[test]
    fn secret_debug_does_not_print_contents() {
        let mut s = Secret::<4>::new();
        s.expose_mut().copy_from_slice(&[0x12, 0x34, 0x56, 0x78]);
        let d = format!("{:?}", s);
        assert_eq!(d, "Secret<4>([redacted])");
    }
}
