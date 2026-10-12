//! Hardware random numbers (RDSEED, then RDRAND). No software fallback: if the
//! CPU offers neither, or the instruction keeps failing, callers get an error
//! and must not proceed (TLS refuses to start without entropy).
//!
//! This trusts the CPU's generator. Its quality cannot be verified from
//! software; under QEMU it is backed by the host.

use core::arch::x86_64::{__cpuid, __cpuid_count, _rdrand64_step, _rdseed64_step};

fn has_rdrand() -> bool {
    __cpuid(1).ecx & (1 << 30) != 0
}

fn has_rdseed() -> bool {
    __cpuid(0).eax >= 7 && __cpuid_count(7, 0).ebx & (1 << 18) != 0
}

#[target_feature(enable = "rdseed")]
unsafe fn rdseed() -> Option<u64> {
    let mut v = 0u64;
    for _ in 0..64 {
        if _rdseed64_step(&mut v) == 1 {
            return Some(v);
        }
        core::hint::spin_loop();
    }
    None
}

#[target_feature(enable = "rdrand")]
unsafe fn rdrand() -> Option<u64> {
    let mut v = 0u64;
    for _ in 0..10 {
        if _rdrand64_step(&mut v) == 1 {
            return Some(v);
        }
    }
    None
}

pub fn available() -> bool {
    has_rdseed() || has_rdrand()
}

pub fn next_u64() -> Option<u64> {
    if has_rdseed() {
        // SAFETY: CPUID says RDSEED exists.
        if let Some(v) = unsafe { rdseed() } {
            return Some(v);
        }
    }
    if has_rdrand() {
        // SAFETY: CPUID says RDRAND exists.
        return unsafe { rdrand() };
    }
    None
}

/// Fill `buf` completely with hardware randomness, or return `false`.
pub fn fill(buf: &mut [u8]) -> bool {
    for chunk in buf.chunks_mut(8) {
        let Some(v) = next_u64() else { return false };
        chunk.copy_from_slice(&v.to_le_bytes()[..chunk.len()]);
    }
    true
}
