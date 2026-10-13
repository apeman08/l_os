//! Boot-ID marker: the same 16-byte ID is embedded in the kernel image (at a
//! placeholder) and written to LBA 0 of the boot stick, so the running kernel
//! can recognise its own boot stick.

pub const PLACEHOLDER: [u8; 32] = *b"L_OS-BOOT-ID-PLACEHOLDER-v1\0\0\0\0\0";
pub type BootId = [u8; 16];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarkError {
    Missing,
    Duplicate,
}

const EMBED_MAGIC: [u8; 8] = *b"L_OSID01";
const LBA0_MAGIC: [u8; 8] = *b"L_OSBOOT";
const FNV_OFFSET: u128 = 0x6c62272e07bb014262b821756295c58d;
const FNV_PRIME: u128 = 0x0000000001000000000000000000013b;

/// Offset of the single placeholder in `kernel`. Linear scan with a cheap
/// first-byte filter; overlapping occurrences count as duplicates.
pub fn find_placeholder(kernel: &[u8]) -> Result<usize, MarkError> {
    let mut found: Option<usize> = None;
    let mut from = 0usize;
    while from + 32 <= kernel.len() {
        let hay = &kernel[from..kernel.len() - 31];
        let Some(rel) = hay.iter().position(|&c| c == PLACEHOLDER[0]) else {
            break;
        };
        let i = from + rel;
        if kernel[i..i + 32] == PLACEHOLDER {
            if found.is_some() {
                return Err(MarkError::Duplicate);
            }
            found = Some(i);
        }
        from = i + 1;
    }
    found.ok_or(MarkError::Missing)
}

/// FNV-1a-128 over `kernel` with bytes `at..at+32` treated as zeros (clamped
/// to the slice), so the ID is the same before and after patching. Big-endian.
pub fn compute_id(kernel: &[u8], at: usize) -> BootId {
    let end = at.saturating_add(32);
    let mut h = FNV_OFFSET;
    for (i, &byte) in kernel.iter().enumerate() {
        let v = if i >= at && i < end { 0 } else { byte };
        h = (h ^ v as u128).wrapping_mul(FNV_PRIME);
    }
    h.to_be_bytes()
}

fn marker(magic: &[u8; 8], id: &BootId) -> [u8; 32] {
    let mut m = [0u8; 32];
    m[..8].copy_from_slice(magic);
    m[8..24].copy_from_slice(id);
    m
}

/// Bytes that replace the placeholder inside the kernel image.
pub fn embedded(id: &BootId) -> [u8; 32] {
    marker(&EMBED_MAGIC, id)
}

/// ID embedded in the kernel; `None` while the placeholder is still there.
pub fn parse_embedded(b: &[u8; 32]) -> Option<BootId> {
    if b[..8] != EMBED_MAGIC {
        return None;
    }
    b[8..24].try_into().ok()
}

/// Marker for offset 0 of LBA 0 of the boot stick.
pub fn lba0_marker(id: &BootId) -> [u8; 32] {
    marker(&LBA0_MAGIC, id)
}

/// ID found at the start of a sector read from the stick.
pub fn parse_lba0(sector: &[u8]) -> Option<BootId> {
    if sector.len() < 32 || sector[..8] != LBA0_MAGIC {
        return None;
    }
    sector[8..24].try_into().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    extern crate std;
    use std::vec;
    use std::vec::Vec;

    fn image(prefix: &[u8], suffix: &[u8]) -> Vec<u8> {
        let mut v = prefix.to_vec();
        v.extend_from_slice(&PLACEHOLDER);
        v.extend_from_slice(suffix);
        v
    }

    #[test]
    fn placeholder_found_once() {
        let k = image(b"some header bytes L_OS-BOOT", b"tail");
        assert_eq!(find_placeholder(&k), Ok(27));
        // multi-MB image, placeholder deep inside
        let mut big = vec![b'L'; 4 * 1024 * 1024];
        big.extend_from_slice(&PLACEHOLDER);
        big.extend_from_slice(&[0u8; 1000]);
        assert_eq!(find_placeholder(&big), Ok(4 * 1024 * 1024));
        // placeholder at very end and very start
        assert_eq!(find_placeholder(&PLACEHOLDER), Ok(0));
    }

    #[test]
    fn missing_placeholder_reported() {
        assert_eq!(find_placeholder(&[]), Err(MarkError::Missing));
        assert_eq!(find_placeholder(&PLACEHOLDER[..31]), Err(MarkError::Missing));
        let mut k = image(b"ab", b"cd");
        k[10] ^= 1; // corrupt one byte
        assert_eq!(find_placeholder(&k), Err(MarkError::Missing));
    }

    #[test]
    fn duplicate_placeholder_rejected() {
        let mut k = image(b"x", b"y");
        k.extend_from_slice(&PLACEHOLDER);
        assert_eq!(find_placeholder(&k), Err(MarkError::Duplicate));
        // back to back
        let mut k = PLACEHOLDER.to_vec();
        k.extend_from_slice(&PLACEHOLDER);
        assert_eq!(find_placeholder(&k), Err(MarkError::Duplicate));
    }

    #[test]
    fn id_same_before_and_after_patch() {
        let mut k = image(b"head", b"tail");
        let at = find_placeholder(&k).unwrap();
        let before = compute_id(&k, at);
        k[at..at + 32].copy_from_slice(&embedded(&before));
        assert_eq!(compute_id(&k, at), before);
        // different content gives a different ID
        let mut k2 = k.clone();
        *k2.last_mut().unwrap() ^= 1;
        assert_ne!(compute_id(&k2, at), before);
        // Known vector, computed independently (python, big-int FNV-1a-128:
        // h = offset; for each byte: h = ((h ^ b) * prime) mod 2^128) over
        // b"abc" + 32 zero bytes + b"xyz"; ID = h as 16 big-endian bytes.
        let k3 = image(b"abc", b"xyz");
        let id = compute_id(&k3, 3);
        let want: [u8; 16] = [
            0x65, 0x49, 0x03, 0x51, 0xc7, 0x90, 0xb1, 0x5c, 0xee, 0x44, 0x2c, 0x5e, 0x23, 0xfb,
            0xc8, 0xd6,
        ];
        assert_eq!(id, want);
    }

    #[test]
    fn compute_id_range_is_safe() {
        // FNV-1a-128 of "a" (published test vector) with an out-of-range
        // placeholder position: nothing is zeroed, no panic.
        let want: [u8; 16] = [
            0xd2, 0x28, 0xcb, 0x69, 0x6f, 0x1a, 0x8c, 0xaf, 0x78, 0x91, 0x2b, 0x70, 0x4e, 0x4a,
            0x89, 0x64,
        ];
        assert_eq!(compute_id(b"a", 1), want);
        assert_eq!(compute_id(b"a", usize::MAX), want);
        // empty input = offset basis
        assert_eq!(
            compute_id(&[], 0),
            0x6c62272e07bb014262b821756295c58d_u128.to_be_bytes()
        );
        // partially overlapping range zeroes only the overlap
        let a = compute_id(&[1, 2, 3], 1);
        assert_eq!(a, compute_id(&[1, 0, 0], 99));
    }

    #[test]
    fn embedded_roundtrip_and_placeholder_is_none() {
        let id: BootId = *b"0123456789abcdef";
        let e = embedded(&id);
        assert_eq!(&e[..8], b"L_OSID01");
        assert_eq!(&e[8..24], &id);
        assert!(e[24..].iter().all(|&x| x == 0));
        assert_eq!(parse_embedded(&e), Some(id));
        assert_eq!(parse_embedded(&PLACEHOLDER), None);
        assert_eq!(parse_embedded(&[0u8; 32]), None);
    }

    #[test]
    fn lba0_roundtrip() {
        let id: BootId = [7; 16];
        let m = lba0_marker(&id);
        assert_eq!(&m[..8], b"L_OSBOOT");
        assert_eq!(&m[8..24], &id);
        assert!(m[24..].iter().all(|&x| x == 0));
        let mut sector = [0xEEu8; 512];
        sector[..32].copy_from_slice(&m);
        assert_eq!(parse_lba0(&sector), Some(id));
        assert_eq!(parse_lba0(&m), Some(id));
    }

    #[test]
    fn lba0_without_magic_is_none() {
        assert_eq!(parse_lba0(&[0u8; 512]), None);
        assert_eq!(parse_lba0(&[]), None);
        let m = lba0_marker(&[1; 16]);
        assert_eq!(parse_lba0(&m[..31]), None);
        // the kernel-embedded magic must not be accepted as a stick marker
        assert_eq!(parse_lba0(&embedded(&[1; 16])), None);
    }

    #[test]
    fn marker_fits_in_mbr_bootstrap_area() {
        assert!(lba0_marker(&[0; 16]).len() <= 440);
        assert!(PLACEHOLDER.len() <= 440);
    }
}
