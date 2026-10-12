//! Strict parser for statically linked x86_64 ELF executables.
//!
//! The input is untrusted, so everything is validated before the kernel maps
//! a single page: only `PT_LOAD` segments are used, they must lie inside the
//! allowed user window, be page aligned, non-overlapping and never writable
//! and executable at once. Anything unexpected is rejected, not "best effort".

pub const PAGE: u64 = 4096;
pub const MAX_SEGMENTS: usize = 6;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ElfError {
    TooShort,
    BadMagic,
    NotElf64LittleEndian,
    NotExecutable,
    WrongMachine,
    BadProgramHeaders,
    /// Dynamic linking / interpreters are not supported.
    Unsupported,
    TooManySegments,
    NoLoadSegments,
    SegmentOutOfRange,
    SegmentMisaligned,
    SegmentOverlap,
    WritableAndExecutable,
    FileRangeOutOfBounds,
    BadEntryPoint,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Segment {
    pub vaddr: u64,
    pub file_offset: u64,
    pub file_size: u64,
    pub mem_size: u64,
    pub writable: bool,
    pub executable: bool,
}

impl Segment {
    /// Pages covered in memory.
    pub fn pages(&self) -> u64 {
        self.mem_size.div_ceil(PAGE)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Image {
    pub entry: u64,
    pub segments: [Option<Segment>; MAX_SEGMENTS],
}

impl Image {
    pub fn segments(&self) -> impl Iterator<Item = &Segment> {
        self.segments.iter().flatten()
    }
}

fn u16_at(b: &[u8], o: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(o..o.checked_add(2)?)?.try_into().ok()?))
}
fn u32_at(b: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(o..o.checked_add(4)?)?.try_into().ok()?))
}
fn u64_at(b: &[u8], o: usize) -> Option<u64> {
    Some(u64::from_le_bytes(b.get(o..o.checked_add(8)?)?.try_into().ok()?))
}

const PT_LOAD: u32 = 1;
const PT_DYNAMIC: u32 = 2;
const PT_INTERP: u32 = 3;
const PF_X: u32 = 1;
const PF_W: u32 = 2;

/// Parse `bytes`; every segment must fit in `[window_start, window_end)`.
pub fn parse(bytes: &[u8], window_start: u64, window_end: u64) -> Result<Image, ElfError> {
    if bytes.len() < 64 {
        return Err(ElfError::TooShort);
    }
    if &bytes[0..4] != b"\x7fELF" {
        return Err(ElfError::BadMagic);
    }
    // class 64-bit, little endian, version 1
    if bytes[4] != 2 || bytes[5] != 1 || bytes[6] != 1 {
        return Err(ElfError::NotElf64LittleEndian);
    }
    if u16_at(bytes, 16) != Some(2) {
        return Err(ElfError::NotExecutable); // ET_EXEC only (no PIE/shared)
    }
    if u16_at(bytes, 18) != Some(62) {
        return Err(ElfError::WrongMachine);
    }
    let entry = u64_at(bytes, 24).ok_or(ElfError::TooShort)?;
    let phoff = u64_at(bytes, 32).ok_or(ElfError::TooShort)?;
    let phentsize = u16_at(bytes, 54).ok_or(ElfError::TooShort)?;
    let phnum = u16_at(bytes, 56).ok_or(ElfError::TooShort)? as u64;
    if phentsize != 56 || phnum == 0 || phnum > 32 {
        return Err(ElfError::BadProgramHeaders);
    }
    let table_end = phnum
        .checked_mul(56)
        .and_then(|n| n.checked_add(phoff))
        .ok_or(ElfError::BadProgramHeaders)?;
    if table_end > bytes.len() as u64 {
        return Err(ElfError::BadProgramHeaders);
    }

    let mut segments: [Option<Segment>; MAX_SEGMENTS] = [None; MAX_SEGMENTS];
    let mut count = 0;
    for i in 0..phnum {
        let o = (phoff + i * 56) as usize;
        let p_type = u32_at(bytes, o).ok_or(ElfError::BadProgramHeaders)?;
        let flags = u32_at(bytes, o + 4).ok_or(ElfError::BadProgramHeaders)?;
        let offset = u64_at(bytes, o + 8).ok_or(ElfError::BadProgramHeaders)?;
        let vaddr = u64_at(bytes, o + 16).ok_or(ElfError::BadProgramHeaders)?;
        let filesz = u64_at(bytes, o + 32).ok_or(ElfError::BadProgramHeaders)?;
        let memsz = u64_at(bytes, o + 40).ok_or(ElfError::BadProgramHeaders)?;
        match p_type {
            PT_DYNAMIC | PT_INTERP => return Err(ElfError::Unsupported),
            PT_LOAD => {}
            _ => continue, // notes, stack hints, ... carry no mapping
        }
        if memsz == 0 {
            continue;
        }
        let (writable, executable) = (flags & PF_W != 0, flags & PF_X != 0);
        if writable && executable {
            return Err(ElfError::WritableAndExecutable);
        }
        if vaddr % PAGE != 0 {
            return Err(ElfError::SegmentMisaligned);
        }
        if filesz > memsz {
            return Err(ElfError::FileRangeOutOfBounds);
        }
        let mem_end = vaddr.checked_add(memsz).ok_or(ElfError::SegmentOutOfRange)?;
        let mapped_end = mem_end.checked_next_multiple_of(PAGE).ok_or(ElfError::SegmentOutOfRange)?;
        if vaddr < window_start || mapped_end > window_end {
            return Err(ElfError::SegmentOutOfRange);
        }
        let file_end = offset.checked_add(filesz).ok_or(ElfError::FileRangeOutOfBounds)?;
        if file_end > bytes.len() as u64 {
            return Err(ElfError::FileRangeOutOfBounds);
        }
        if count == MAX_SEGMENTS {
            return Err(ElfError::TooManySegments);
        }
        let seg = Segment { vaddr, file_offset: offset, file_size: filesz, mem_size: memsz, writable, executable };
        // No two segments may share a page (that would let flags conflict).
        for other in segments.iter().flatten() {
            let (a0, a1) = (seg.vaddr, seg.vaddr + seg.pages() * PAGE);
            let (b0, b1) = (other.vaddr, other.vaddr + other.pages() * PAGE);
            if a0 < b1 && b0 < a1 {
                return Err(ElfError::SegmentOverlap);
            }
        }
        segments[count] = Some(seg);
        count += 1;
    }
    if count == 0 {
        return Err(ElfError::NoLoadSegments);
    }
    // The entry point must be inside an executable segment's file-backed bytes.
    let entry_ok = segments
        .iter()
        .flatten()
        .any(|s| s.executable && entry >= s.vaddr && entry < s.vaddr + s.file_size);
    if !entry_ok {
        return Err(ElfError::BadEntryPoint);
    }
    Ok(Image { entry, segments })
}

#[cfg(test)]
mod tests {
    use super::*;
    extern crate std;
    use std::vec::Vec;

    const W0: u64 = 0x7000_0000_0000;
    const W1: u64 = 0x7000_000e_0000;

    struct Ph {
        p_type: u32,
        flags: u32,
        offset: u64,
        vaddr: u64,
        filesz: u64,
        memsz: u64,
    }

    fn rx(vaddr: u64) -> Ph {
        Ph { p_type: 1, flags: 5, offset: 0x1000, vaddr, filesz: 0x100, memsz: 0x100 }
    }

    /// Build a minimal ELF with the given program headers; file data is zero.
    fn build(entry: u64, phs: &[Ph]) -> Vec<u8> {
        let mut v = std::vec![0u8; 0x3000];
        v[0..4].copy_from_slice(b"\x7fELF");
        v[4] = 2;
        v[5] = 1;
        v[6] = 1;
        v[16..18].copy_from_slice(&2u16.to_le_bytes());
        v[18..20].copy_from_slice(&62u16.to_le_bytes());
        v[24..32].copy_from_slice(&entry.to_le_bytes());
        v[32..40].copy_from_slice(&64u64.to_le_bytes());
        v[54..56].copy_from_slice(&56u16.to_le_bytes());
        v[56..58].copy_from_slice(&(phs.len() as u16).to_le_bytes());
        for (i, p) in phs.iter().enumerate() {
            let o = 64 + i * 56;
            v[o..o + 4].copy_from_slice(&p.p_type.to_le_bytes());
            v[o + 4..o + 8].copy_from_slice(&p.flags.to_le_bytes());
            v[o + 8..o + 16].copy_from_slice(&p.offset.to_le_bytes());
            v[o + 16..o + 24].copy_from_slice(&p.vaddr.to_le_bytes());
            v[o + 32..o + 40].copy_from_slice(&p.filesz.to_le_bytes());
            v[o + 40..o + 48].copy_from_slice(&p.memsz.to_le_bytes());
        }
        v
    }

    fn parse_ok(v: &[u8]) -> Image {
        parse(v, W0, W1).unwrap()
    }

    #[test]
    fn accepts_text_rodata_data_bss() {
        let v = build(
            W0,
            &[
                rx(W0),
                Ph { p_type: 1, flags: 4, offset: 0x1000, vaddr: W0 + 0x1000, filesz: 0x80, memsz: 0x80 },
                Ph { p_type: 1, flags: 6, offset: 0x2000, vaddr: W0 + 0x2000, filesz: 0x40, memsz: 0x3000 },
            ],
        );
        let img = parse_ok(&v);
        assert_eq!(img.entry, W0);
        let segs: Vec<_> = img.segments().collect();
        assert_eq!(segs.len(), 3);
        assert!(segs[0].executable && !segs[0].writable);
        assert!(segs[2].writable && !segs[2].executable);
        assert_eq!(segs[2].pages(), 3); // bss extends beyond the file bytes
    }

    #[test]
    fn ignores_non_load_segments_but_rejects_dynamic() {
        let mut phs = std::vec![rx(W0), Ph { p_type: 0x6474e551, flags: 6, offset: 0, vaddr: 0, filesz: 0, memsz: 0 }];
        assert!(parse(&build(W0, &phs), W0, W1).is_ok());
        phs.push(Ph { p_type: 3, flags: 4, offset: 0, vaddr: 0, filesz: 0, memsz: 0 });
        assert_eq!(parse(&build(W0, &phs), W0, W1), Err(ElfError::Unsupported));
        phs.pop();
        phs.push(Ph { p_type: 2, flags: 4, offset: 0, vaddr: 0, filesz: 0, memsz: 0 });
        assert_eq!(parse(&build(W0, &phs), W0, W1), Err(ElfError::Unsupported));
    }

    #[test]
    fn rejects_writable_and_executable() {
        let v = build(W0, &[Ph { flags: 7, ..rx(W0) }]);
        assert_eq!(parse(&v, W0, W1), Err(ElfError::WritableAndExecutable));
    }

    #[test]
    fn rejects_segments_outside_the_user_window() {
        for bad in [0u64, 0x1000, W0 - 0x1000, W1, 0xffff_8000_0000_0000, u64::MAX - 0xfff] {
            let v = build(bad, &[rx(bad)]);
            assert_eq!(parse(&v, W0, W1), Err(ElfError::SegmentOutOfRange), "{bad:#x}");
        }
        // Runs past the end of the window through memsz (bss).
        let v = build(W0, &[Ph { memsz: W1 - W0 + 1, ..rx(W0) }]);
        assert_eq!(parse(&v, W0, W1), Err(ElfError::SegmentOutOfRange));
        // memsz overflow
        let v = build(W0, &[Ph { memsz: u64::MAX, ..rx(W0) }]);
        assert!(parse(&v, W0, W1).is_err());
    }

    #[test]
    fn rejects_misaligned_and_overlapping() {
        let v = build(W0 + 1, &[Ph { vaddr: W0 + 1, ..rx(W0) }]);
        assert_eq!(parse(&v, W0, W1), Err(ElfError::SegmentMisaligned));
        let v = build(W0, &[rx(W0), Ph { flags: 4, offset: 0x1100, vaddr: W0, filesz: 0x10, memsz: 0x10, p_type: 1 }]);
        assert_eq!(parse(&v, W0, W1), Err(ElfError::SegmentOverlap));
        // Two segments in the same page but different addresses also overlap by page.
        let v = build(W0, &[Ph { memsz: 0x1001, ..rx(W0) }, Ph { flags: 4, offset: 0x1100, vaddr: W0 + 0x1000, filesz: 0x10, memsz: 0x10, p_type: 1 }]);
        assert_eq!(parse(&v, W0, W1), Err(ElfError::SegmentOverlap));
    }

    #[test]
    fn rejects_file_ranges_outside_the_file() {
        let len = build(W0, &[rx(W0)]).len() as u64;
        let v = build(W0, &[Ph { offset: len - 8, filesz: 0x100, memsz: 0x100, ..rx(W0) }]);
        assert_eq!(parse(&v, W0, W1), Err(ElfError::FileRangeOutOfBounds));
        let v = build(W0, &[Ph { offset: u64::MAX - 4, ..rx(W0) }]);
        assert_eq!(parse(&v, W0, W1), Err(ElfError::FileRangeOutOfBounds));
        let v = build(W0, &[Ph { filesz: 0x200, memsz: 0x100, ..rx(W0) }]);
        assert_eq!(parse(&v, W0, W1), Err(ElfError::FileRangeOutOfBounds));
    }

    #[test]
    fn entry_must_be_inside_executable_code() {
        assert_eq!(parse(&build(W0 + 0x100, &[rx(W0)]), W0, W1), Err(ElfError::BadEntryPoint)); // past file bytes
        assert_eq!(parse(&build(W0 - 4, &[rx(W0)]), W0, W1), Err(ElfError::BadEntryPoint));
        let data = Ph { p_type: 1, flags: 6, offset: 0x2000, vaddr: W0 + 0x1000, filesz: 0x40, memsz: 0x40 };
        assert_eq!(parse(&build(W0 + 0x1000, &[rx(W0), data]), W0, W1), Err(ElfError::BadEntryPoint)); // in data
        assert!(parse(&build(W0 + 0xff, &[rx(W0)]), W0, W1).is_ok());
    }

    #[test]
    fn rejects_malformed_headers() {
        let good = build(W0, &[rx(W0)]);
        assert_eq!(parse(&good[..10], W0, W1), Err(ElfError::TooShort));
        let mut v = good.clone();
        v[0] = 0;
        assert_eq!(parse(&v, W0, W1), Err(ElfError::BadMagic));
        let mut v = good.clone();
        v[4] = 1; // 32-bit
        assert_eq!(parse(&v, W0, W1), Err(ElfError::NotElf64LittleEndian));
        let mut v = good.clone();
        v[16] = 3; // ET_DYN
        assert_eq!(parse(&v, W0, W1), Err(ElfError::NotExecutable));
        let mut v = good.clone();
        v[18] = 40; // ARM
        assert_eq!(parse(&v, W0, W1), Err(ElfError::WrongMachine));
        let mut v = good.clone();
        v[54] = 32; // wrong phentsize
        assert_eq!(parse(&v, W0, W1), Err(ElfError::BadProgramHeaders));
        let mut v = good.clone();
        v[32..40].copy_from_slice(&u64::MAX.to_le_bytes()); // phoff overflow
        assert_eq!(parse(&v, W0, W1), Err(ElfError::BadProgramHeaders));
        let v = build(W0, &[]);
        assert_eq!(parse(&v, W0, W1), Err(ElfError::BadProgramHeaders));
        let v = build(W0, &[Ph { p_type: 4, flags: 4, offset: 0, vaddr: 0, filesz: 0, memsz: 0 }]);
        assert_eq!(parse(&v, W0, W1), Err(ElfError::NoLoadSegments));
    }

    #[test]
    fn too_many_segments() {
        let phs: Vec<Ph> = (0..7)
            .map(|i| Ph { p_type: 1, flags: 4, offset: 0x1000, vaddr: W0 + i * 0x1000, filesz: 0x10, memsz: 0x10 })
            .collect();
        let mut phs = phs;
        phs[0].flags = 5;
        assert_eq!(parse(&build(W0, &phs), W0, W1), Err(ElfError::TooManySegments));
    }

    #[test]
    fn never_panics_on_garbage() {
        // Cheap deterministic fuzz: flip every byte of a valid header region.
        let good = build(W0, &[rx(W0)]);
        for i in 0..good.len().min(200) {
            for x in [0u8, 0xff, 0x80, 1] {
                let mut v = good.clone();
                v[i] = x;
                let _ = parse(&v, W0, W1);
            }
        }
        for len in 0..100 {
            let _ = parse(&good[..len], W0, W1);
        }
    }
}
