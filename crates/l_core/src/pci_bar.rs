//! PCI Base Address Registers: decoding and size probing (spec §7.1 step 1).
//!
//! Pure arithmetic on the raw 32-bit register values; the kernel
//! (`src/pci.rs`) does the configuration-space accesses. Every value comes
//! from hardware and is untrusted: no input panics, impossible combinations
//! decode as [`Bar::Invalid`] or size as `None`.

/// Number of BARs in a type-0 (endpoint) configuration header.
pub const BAR_COUNT: usize = 6;

/// One decoded BAR.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bar {
    /// I/O-space BAR (bit 0 set); `base` is the port address.
    Io { base: u32 },
    /// 32-bit memory BAR (type 0).
    Mem32 { base: u64, prefetchable: bool },
    /// 64-bit memory BAR (type 2); the next BAR holds the upper half.
    Mem64 { base: u64, prefetchable: bool },
    /// Reserved memory type (1 or 3), a 64-bit BAR without an upper half
    /// (last index), or an index outside the header.
    Invalid,
}

impl Bar {
    /// Physical base address of a memory BAR, `None` otherwise.
    pub fn mem_base(&self) -> Option<u64> {
        match *self {
            Bar::Mem32 { base, .. } | Bar::Mem64 { base, .. } => Some(base),
            _ => None,
        }
    }

    pub fn is64(&self) -> bool {
        matches!(self, Bar::Mem64 { .. })
    }
}

/// Decode a BAR from its low dword `lo` and, for a 64-bit memory BAR, the
/// following dword `hi` (ignored for every other kind).
pub fn decode(lo: u32, hi: u32) -> Bar {
    if lo & 1 == 1 {
        return Bar::Io { base: lo & !0x3 };
    }
    let prefetchable = lo & 0x8 != 0;
    let low = u64::from(lo & !0xf);
    match (lo >> 1) & 0x3 {
        0 => Bar::Mem32 { base: low, prefetchable },
        2 => Bar::Mem64 { base: u64::from(hi) << 32 | low, prefetchable },
        _ => Bar::Invalid,
    }
}

/// Decode BAR `idx` of a header whose six BAR dwords are `bars`. A 64-bit
/// BAR in the last slot (no upper half) and `idx >= 6` are [`Bar::Invalid`].
pub fn decode_at(bars: &[u32; BAR_COUNT], idx: usize) -> Bar {
    let Some(&lo) = bars.get(idx) else { return Bar::Invalid };
    match decode(lo, 0) {
        Bar::Mem64 { .. } => match bars.get(idx + 1) {
            Some(&hi) => decode(lo, hi),
            None => Bar::Invalid,
        },
        other => other,
    }
}

/// Size of a BAR from the values read back after writing all-ones to it
/// (`lo`; `hi` for the upper half of a 64-bit BAR, ignored if `!is64`).
/// The size is the lowest writable address bit; `None` if no address bit is
/// writable (BAR not implemented) or the type bits contradict `is64`.
pub fn size_from_probe(lo: u32, hi: u32, is64: bool) -> Option<u64> {
    let mask = if lo & 1 == 1 {
        if is64 {
            return None;
        }
        // I/O BAR: bits 31..2 are address bits (often only 15..2 implemented).
        u64::from(lo & !0x3)
    } else {
        let ty = (lo >> 1) & 0x3;
        match (ty, is64) {
            (0, false) => u64::from(lo & !0xf),
            (2, true) => u64::from(hi) << 32 | u64::from(lo & !0xf),
            _ => return None,
        }
    };
    if mask == 0 {
        return None;
    }
    // Lowest set bit: the naturally aligned size the BAR decodes.
    Some(mask & mask.wrapping_neg())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_io_bar() {
        assert_eq!(decode(0x0000_c041, 0xdead_beef), Bar::Io { base: 0xc040 });
        assert_eq!(decode(0x0000_c041, 0).mem_base(), None);
    }

    #[test]
    fn decode_mem32_prefetch() {
        assert_eq!(decode(0xfe00_0008, 0x1234), Bar::Mem32 { base: 0xfe00_0000, prefetchable: true });
        assert_eq!(decode(0xfebf_1000, 0), Bar::Mem32 { base: 0xfebf_1000, prefetchable: false });
        assert!(!decode(0xfe00_0008, 0).is64());
    }

    #[test]
    fn decode_mem64_combines_hi() {
        // qemu-xhci in the spike: BAR0 raw 0x4, BAR1 raw 0xc0.
        let b = decode(0x0000_0004, 0x0000_00c0);
        assert_eq!(b, Bar::Mem64 { base: 0xc0_0000_0000, prefetchable: false });
        assert!(b.is64());
        assert_eq!(b.mem_base(), Some(0xc0_0000_0000));
        assert_eq!(
            decode(0xfeb0_000c, 0x1),
            Bar::Mem64 { base: 0x1_feb0_0000, prefetchable: true }
        );
    }

    #[test]
    fn decode_reserved_types_are_invalid() {
        assert_eq!(decode(0x0000_0002, 0), Bar::Invalid); // type 1 (legacy <1 MiB)
        assert_eq!(decode(0x0000_0006, 0), Bar::Invalid); // type 3
    }

    #[test]
    fn decode_at_handles_last_index_and_out_of_range() {
        let mut bars = [0u32; BAR_COUNT];
        bars[0] = 0x4;
        bars[1] = 0xc0;
        assert_eq!(decode_at(&bars, 0), Bar::Mem64 { base: 0xc0_0000_0000, prefetchable: false });
        bars[5] = 0x4; // 64-bit BAR without an upper half
        assert_eq!(decode_at(&bars, 5), Bar::Invalid);
        bars[5] = 0xfe00_0000;
        assert_eq!(decode_at(&bars, 5), Bar::Mem32 { base: 0xfe00_0000, prefetchable: false });
        assert_eq!(decode_at(&bars, 6), Bar::Invalid);
        assert_eq!(decode_at(&bars, usize::MAX), Bar::Invalid);
    }

    #[test]
    fn size_from_probe_64k() {
        assert_eq!(size_from_probe(0xffff_0000, 0, false), Some(0x1_0000));
        assert_eq!(size_from_probe(0xffff_000c, 0xffff_ffff, true), Some(0x1_0000));
        // qemu-xhci in the spike: lo 0xffffc004, hi 0xffffffff -> 16 KiB.
        assert_eq!(size_from_probe(0xffff_c004, 0xffff_ffff, true), Some(0x4000));
        // I/O BAR with only the low 16 bits implemented: 32 ports.
        assert_eq!(size_from_probe(0x0000_ffe1, 0, false), Some(0x20));
    }

    #[test]
    fn size_from_probe_zero_is_none() {
        assert_eq!(size_from_probe(0, 0, false), None);
        assert_eq!(size_from_probe(0, 0, true), None);
        assert_eq!(size_from_probe(0x4, 0, true), None); // 64-bit, no address bit
        assert_eq!(size_from_probe(0x1, 0, false), None); // I/O, no address bit
    }

    #[test]
    fn size_from_probe_hostile_inputs() {
        // Type bits contradict the caller's is64.
        assert_eq!(size_from_probe(0xffff_0004, 0xffff_ffff, false), None);
        assert_eq!(size_from_probe(0xffff_0000, 0xffff_ffff, true), None);
        assert_eq!(size_from_probe(0xffff_ffe1, 0, true), None);
        // Reserved types.
        assert_eq!(size_from_probe(0xffff_0002, 0, false), None);
        assert_eq!(size_from_probe(0xffff_0006, 0xffff_ffff, true), None);
        // Only the upper half writable: 4 GiB granularity, no overflow.
        assert_eq!(size_from_probe(0x0000_0004, 0xffff_ffff, true), Some(1 << 32));
        // Only bit 63 writable: the largest representable size.
        assert_eq!(size_from_probe(0x0000_0004, 0x8000_0000, true), Some(1 << 63));
        // Non-contiguous mask (broken device): lowest writable bit wins.
        assert_eq!(size_from_probe(0xf0f0_1000, 0, false), Some(0x1000));
        // All ones everywhere (device vanished): 16 bytes, no panic.
        assert_eq!(size_from_probe(0xffff_ffff, 0xffff_ffff, false), Some(4));
        assert_eq!(size_from_probe(0xffff_fff4, 0xffff_ffff, true), Some(16));
    }
}
