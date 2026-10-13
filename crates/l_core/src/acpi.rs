//! ACPI table parsing for the S5 (soft-off) path. Pure logic, hostile-input
//! safe: every access goes through `get`, lengths are checked before use and
//! nothing here panics or overflows on malformed tables.
//!
//! Real QEMU/i440fx values (see docs/superpowers/notes/2026-10-13-usb-qemu-spike.md):
//! FADT is revision 1, 116 bytes (no `X_DSDT`, no `X_PM1a_CNT_BLK`), `_S5` is
//! `Package{0,0,0,0}`, `PM1a_CNT_BLK` = 0xB004, poweroff value 0x2000.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcpiError {
    BadSignature,
    BadChecksum,
    Truncated,
    NotFound,
    Unsupported,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rsdp {
    pub revision: u8,
    pub rsdt: u32,
    pub xsdt: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SdtHeader {
    pub signature: [u8; 4],
    pub length: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fadt {
    pub pm1a_cnt: u16,
    pub pm1b_cnt: u16,
    pub dsdt: u64,
    pub smi_cmd: u32,
    pub acpi_enable: u8,
}

const RSDP_V1_LEN: usize = 20;
const RSDP_V2_MIN_LEN: usize = 36;
const SDT_HEADER_LEN: usize = 36;
const MAX_ROOT_ENTRIES: usize = 64;

// FADT field offsets (ACPI spec, table 5.9).
const FADT_DSDT: usize = 40;
const FADT_SMI_CMD: usize = 48;
const FADT_ACPI_ENABLE: usize = 52;
const FADT_PM1A_CNT: usize = 64;
const FADT_PM1B_CNT: usize = 68;
const FADT_X_DSDT: usize = 140;
const FADT_X_PM1A_CNT: usize = 172;
const FADT_X_PM1B_CNT: usize = 184;
const GAS_LEN: usize = 12;
const GAS_SPACE_SYSTEM_IO: u8 = 1;

fn rd_u8(b: &[u8], off: usize) -> Option<u8> {
    b.get(off).copied()
}

fn rd_u32(b: &[u8], off: usize) -> Option<u32> {
    let end = off.checked_add(4)?;
    Some(u32::from_le_bytes(b.get(off..end)?.try_into().ok()?))
}

fn rd_u64(b: &[u8], off: usize) -> Option<u64> {
    let end = off.checked_add(8)?;
    Some(u64::from_le_bytes(b.get(off..end)?.try_into().ok()?))
}

fn checksum_ok(b: &[u8]) -> bool {
    b.iter().fold(0u8, |a, &x| a.wrapping_add(x)) == 0
}

pub fn parse_rsdp(b: &[u8]) -> Result<Rsdp, AcpiError> {
    let v1 = b.get(..RSDP_V1_LEN).ok_or(AcpiError::Truncated)?;
    if v1.get(..8) != Some(b"RSD PTR ".as_slice()) {
        return Err(AcpiError::BadSignature);
    }
    if !checksum_ok(v1) {
        return Err(AcpiError::BadChecksum);
    }
    let revision = rd_u8(b, 15).ok_or(AcpiError::Truncated)?;
    let rsdt = rd_u32(b, 16).ok_or(AcpiError::Truncated)?;
    let mut xsdt = None;
    if revision >= 2 {
        let len = rd_u32(b, 20).ok_or(AcpiError::Truncated)? as usize;
        if len < RSDP_V2_MIN_LEN {
            return Err(AcpiError::Truncated);
        }
        let whole = b.get(..len).ok_or(AcpiError::Truncated)?;
        if !checksum_ok(whole) {
            return Err(AcpiError::BadChecksum);
        }
        let x = rd_u64(b, 24).ok_or(AcpiError::Truncated)?;
        if x != 0 {
            xsdt = Some(x);
        }
    }
    Ok(Rsdp { revision, rsdt, xsdt })
}

pub fn parse_sdt(b: &[u8]) -> Result<SdtHeader, AcpiError> {
    let head = b.get(..SDT_HEADER_LEN).ok_or(AcpiError::Truncated)?;
    let signature: [u8; 4] = head[..4].try_into().map_err(|_| AcpiError::Truncated)?;
    let length = rd_u32(head, 4).ok_or(AcpiError::Truncated)?;
    if (length as usize) < SDT_HEADER_LEN {
        return Err(AcpiError::Truncated);
    }
    let whole = b.get(..length as usize).ok_or(AcpiError::Truncated)?;
    if !checksum_ok(whole) {
        return Err(AcpiError::BadChecksum);
    }
    Ok(SdtHeader { signature, length })
}

/// Walk one root table (XSDT: 8-byte entries, RSDT: 4-byte entries). The root's
/// own checksum cannot be verified without reading all of it; only signature
/// and length are checked, and entry headers are read one by one.
fn search_root<F: FnMut(u64, &mut [u8]) -> bool>(
    root: u64,
    root_sig: [u8; 4],
    entry_size: usize,
    sig: [u8; 4],
    read: &mut F,
) -> Result<u64, AcpiError> {
    let mut head = [0u8; SDT_HEADER_LEN];
    if !read(root, &mut head) {
        return Err(AcpiError::Truncated);
    }
    if head[..4] != root_sig {
        return Err(AcpiError::BadSignature);
    }
    let length = rd_u32(&head, 4).ok_or(AcpiError::Truncated)? as usize;
    let payload = length.checked_sub(SDT_HEADER_LEN).ok_or(AcpiError::Truncated)?;
    let count = (payload / entry_size).min(MAX_ROOT_ENTRIES);
    for i in 0..count {
        let at = root
            .checked_add(SDT_HEADER_LEN as u64)
            .and_then(|a| a.checked_add((i * entry_size) as u64))
            .ok_or(AcpiError::Truncated)?;
        let mut raw = [0u8; 8];
        let slot = raw.get_mut(..entry_size).ok_or(AcpiError::Unsupported)?;
        if !read(at, slot) {
            return Err(AcpiError::Truncated);
        }
        let entry = if entry_size == 8 {
            u64::from_le_bytes(raw)
        } else {
            u32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]) as u64
        };
        let mut sig_buf = [0u8; 4];
        // An unreadable or bogus entry must not hide the others.
        if entry != 0 && read(entry, &mut sig_buf) && sig_buf == sig {
            return Ok(entry);
        }
    }
    Err(AcpiError::NotFound)
}

pub fn find_table(
    rsdp: &Rsdp,
    sig: [u8; 4],
    mut read: impl FnMut(u64, &mut [u8]) -> bool,
) -> Result<u64, AcpiError> {
    let mut first_err = None;
    if let Some(x) = rsdp.xsdt {
        match search_root(x, *b"XSDT", 8, sig, &mut read) {
            Ok(a) => return Ok(a),
            Err(e) => first_err = Some(e),
        }
    }
    if rsdp.rsdt != 0 {
        match search_root(rsdp.rsdt as u64, *b"RSDT", 4, sig, &mut read) {
            Ok(a) => return Ok(a),
            Err(e) => first_err = first_err.or(Some(e)),
        }
    }
    Err(first_err.unwrap_or(AcpiError::NotFound))
}

/// Read a PM1 control block: legacy 32-bit port first, else the extended GAS
/// (only if the table is long enough to contain it, only System I/O space).
fn pm_block(t: &[u8], legacy_off: usize, x_off: usize) -> Result<u16, AcpiError> {
    let legacy = rd_u32(t, legacy_off).ok_or(AcpiError::Truncated)?;
    if legacy != 0 {
        return u16::try_from(legacy).map_err(|_| AcpiError::Unsupported);
    }
    let Some(end) = x_off.checked_add(GAS_LEN) else {
        return Ok(0);
    };
    let Some(g) = t.get(x_off..end) else {
        return Ok(0); // old FADT without extended fields
    };
    let addr = rd_u64(g, 4).ok_or(AcpiError::Truncated)?;
    if addr == 0 {
        return Ok(0);
    }
    if g[0] != GAS_SPACE_SYSTEM_IO {
        return Err(AcpiError::Unsupported);
    }
    u16::try_from(addr).map_err(|_| AcpiError::Unsupported)
}

pub fn parse_fadt(b: &[u8]) -> Result<Fadt, AcpiError> {
    let h = parse_sdt(b)?;
    if &h.signature != b"FACP" {
        return Err(AcpiError::BadSignature);
    }
    // parse_sdt verified length <= b.len().
    let t = b.get(..h.length as usize).ok_or(AcpiError::Truncated)?;
    let smi_cmd = rd_u32(t, FADT_SMI_CMD).ok_or(AcpiError::Truncated)?;
    let acpi_enable = rd_u8(t, FADT_ACPI_ENABLE).ok_or(AcpiError::Truncated)?;
    let mut dsdt = rd_u32(t, FADT_DSDT).ok_or(AcpiError::Truncated)? as u64;
    if let Some(x) = rd_u64(t, FADT_X_DSDT) {
        if x != 0 {
            dsdt = x;
        }
    }
    let pm1a_cnt = pm_block(t, FADT_PM1A_CNT, FADT_X_PM1A_CNT)?;
    let pm1b_cnt = pm_block(t, FADT_PM1B_CNT, FADT_X_PM1B_CNT)?;
    if pm1a_cnt == 0 {
        return Err(AcpiError::Unsupported);
    }
    Ok(Fadt { pm1a_cnt, pm1b_cnt, dsdt, smi_cmd, acpi_enable })
}

/// One AML integer element: `0x00`, `0x01` or `0x0A nn`. Returns (value, bytes used).
fn aml_small_int(b: &[u8]) -> Option<(u8, usize)> {
    match *b.first()? {
        0x00 => Some((0, 1)),
        0x01 => Some((1, 1)),
        0x0a => Some((*b.get(1)?, 2)),
        _ => None,
    }
}

fn s5_at(d: &[u8]) -> Option<(u8, u8)> {
    // d starts right after "_S5_".
    if *d.first()? != 0x12 {
        return None;
    }
    let lead = *d.get(1)?;
    let extra = (lead >> 6) as usize;
    let mut pkg_len: u32 = if extra == 0 { (lead & 0x3f) as u32 } else { (lead & 0x0f) as u32 };
    for i in 0..extra {
        let byte = *d.get(2 + i)? as u32;
        pkg_len |= byte << (4 + 8 * i);
    }
    let after_len = 2 + extra;
    let num = *d.get(after_len)?;
    if num < 2 || (pkg_len as usize) < 1 + extra + 1 {
        return None;
    }
    let (a, n) = aml_small_int(d.get(after_len + 1..)?)?;
    let (b, _) = aml_small_int(d.get(after_len + 1 + n..)?)?;
    Some((a, b))
}

pub fn find_s5(dsdt: &[u8]) -> Option<(u8, u8)> {
    let mut i = 0usize;
    while let Some(w) = dsdt.get(i..i.checked_add(4)?) {
        if w == b"_S5_" {
            if let Some(r) = dsdt.get(i + 4..).and_then(s5_at) {
                return Some(r);
            }
        }
        i += 1;
    }
    None
}

pub fn s5_value(slp_typ: u8) -> u16 {
    (((slp_typ & 7) as u16) << 10) | (1 << 13)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fix(buf: &mut [u8], at: usize) {
        buf[at] = 0;
        let s = buf.iter().fold(0u8, |a, &x| a.wrapping_add(x));
        buf[at] = 0u8.wrapping_sub(s);
    }

    fn mk_rsdp(rev: u8, rsdt: u32, xsdt: u64) -> Vec<u8> {
        let mut v = vec![0u8; if rev >= 2 { 36 } else { 20 }];
        v[..8].copy_from_slice(b"RSD PTR ");
        v[9..15].copy_from_slice(b"BOCHS ");
        v[15] = rev;
        v[16..20].copy_from_slice(&rsdt.to_le_bytes());
        if rev >= 2 {
            v[20..24].copy_from_slice(&36u32.to_le_bytes());
            v[24..32].copy_from_slice(&xsdt.to_le_bytes());
            fix(&mut v[..20], 8);
            fix(&mut v, 32);
        } else {
            fix(&mut v, 8);
        }
        v
    }

    fn mk_sdt(sig: &[u8; 4], rev: u8, body: &[u8]) -> Vec<u8> {
        let mut v = vec![0u8; 36];
        v[..4].copy_from_slice(sig);
        v[8] = rev;
        v.extend_from_slice(body);
        let len = v.len() as u32;
        v[4..8].copy_from_slice(&len.to_le_bytes());
        fix(&mut v, 9);
        v
    }

    /// Sparse physical memory: (base, bytes) regions.
    struct Mem(Vec<(u64, Vec<u8>)>);
    impl Mem {
        fn read(&self, addr: u64, buf: &mut [u8]) -> bool {
            for (base, data) in &self.0 {
                if addr >= *base {
                    let off = (addr - *base) as usize;
                    if let Some(s) = data.get(off..off + buf.len()) {
                        buf.copy_from_slice(s);
                        return true;
                    }
                }
            }
            false
        }
    }

    fn rsdt_with(entries: &[u32]) -> Vec<u8> {
        let body: Vec<u8> = entries.iter().flat_map(|e| e.to_le_bytes()).collect();
        mk_sdt(b"RSDT", 1, &body)
    }
    fn xsdt_with(entries: &[u64]) -> Vec<u8> {
        let body: Vec<u8> = entries.iter().flat_map(|e| e.to_le_bytes()).collect();
        mk_sdt(b"XSDT", 1, &body)
    }

    // QEMU i440fx FADT: rev 1, 116 bytes, from the spike note (section 8).
    fn qemu_fadt() -> Vec<u8> {
        let mut body = vec![0xafu8; 116 - 36];
        let mut put = |off: usize, d: &[u8]| body[off - 36..off - 36 + d.len()].copy_from_slice(d);
        put(40, &0x0fb7_b000u32.to_le_bytes()); // DSDT
        put(48, &0xb2u32.to_le_bytes()); // SMI_CMD
        put(52, &[0xf1]); // ACPI_ENABLE
        put(64, &0xb004u32.to_le_bytes()); // PM1a_CNT_BLK
        put(68, &0u32.to_le_bytes()); // PM1b_CNT_BLK
        mk_sdt(b"FACP", 1, &body)
    }

    /// FADT rev 3+, 244 bytes, zeroed, with optional overrides.
    fn big_fadt(f: impl FnOnce(&mut Vec<u8>)) -> Vec<u8> {
        let mut raw = vec![0u8; 244];
        raw[..4].copy_from_slice(b"FACP");
        raw[4..8].copy_from_slice(&244u32.to_le_bytes());
        f(&mut raw);
        fix(&mut raw, 9);
        raw
    }
    fn gas(raw: &mut [u8], off: usize, space: u8, addr: u64) {
        raw[off] = space;
        raw[off + 4..off + 12].copy_from_slice(&addr.to_le_bytes());
    }

    #[test]
    fn rsdp_v1_ok() {
        let r = parse_rsdp(&mk_rsdp(0, 0xfb7d074, 0)).unwrap();
        assert_eq!(r, Rsdp { revision: 0, rsdt: 0xfb7d074, xsdt: None });
    }

    #[test]
    fn rsdp_v2_qemu_values() {
        let r = parse_rsdp(&mk_rsdp(2, 0xfb7d074, 0xfb7d0e8)).unwrap();
        assert_eq!(r, Rsdp { revision: 2, rsdt: 0xfb7d074, xsdt: Some(0xfb7d0e8) });
    }

    #[test]
    fn rsdp_bad_checksum_rejected() {
        let mut v = mk_rsdp(0, 1, 0);
        v[16] ^= 1;
        assert_eq!(parse_rsdp(&v), Err(AcpiError::BadChecksum));
        let mut v = mk_rsdp(0, 1, 0);
        v[0] = b'X';
        assert_eq!(parse_rsdp(&v), Err(AcpiError::BadSignature));
    }

    #[test]
    fn rsdp_truncated_and_bogus_length() {
        assert_eq!(parse_rsdp(&[]), Err(AcpiError::Truncated));
        assert_eq!(parse_rsdp(&mk_rsdp(0, 1, 0)[..19]), Err(AcpiError::Truncated));
        // v2 header but only 20 bytes supplied
        assert_eq!(parse_rsdp(&mk_rsdp(2, 1, 2)[..20]), Err(AcpiError::Truncated));
        // length beyond the buffer / below the minimum
        for bogus in [0u32, 35, 37, 0xffff_ffff] {
            let mut v = mk_rsdp(2, 1, 2);
            v[20..24].copy_from_slice(&bogus.to_le_bytes());
            fix(&mut v, 32);
            assert!(parse_rsdp(&v).is_err(), "length {bogus}");
        }
    }

    #[test]
    fn rsdp_v2_extended_checksum_checked() {
        let mut v = mk_rsdp(2, 1, 0x1234);
        v[24] ^= 0xff; // xsdt byte: first 20 bytes still fine
        assert_eq!(parse_rsdp(&v), Err(AcpiError::BadChecksum));
    }

    #[test]
    fn sdt_checks() {
        let t = mk_sdt(b"TEST", 1, &[1, 2, 3]);
        assert_eq!(parse_sdt(&t).unwrap(), SdtHeader { signature: *b"TEST", length: 39 });
        let mut bad = t.clone();
        bad[36] ^= 1;
        assert_eq!(parse_sdt(&bad), Err(AcpiError::BadChecksum));
        assert_eq!(parse_sdt(&t[..38]), Err(AcpiError::Truncated));
        assert_eq!(parse_sdt(&t[..10]), Err(AcpiError::Truncated));
        let mut short = t.clone();
        short[4..8].copy_from_slice(&10u32.to_le_bytes());
        assert!(parse_sdt(&short).is_err());
        let mut huge = t.clone();
        huge[4..8].copy_from_slice(&u32::MAX.to_le_bytes());
        assert_eq!(parse_sdt(&huge), Err(AcpiError::Truncated));
    }

    #[test]
    fn xsdt_preferred_over_rsdt() {
        let mem = Mem(vec![
            (0x1000, rsdt_with(&[0x5000])),
            (0x2000, xsdt_with(&[0x6000_0000_0000])),
            (0x5000, mk_sdt(b"FACP", 1, &[])),
            (0x6000_0000_0000, mk_sdt(b"FACP", 1, &[])),
        ]);
        let both = Rsdp { revision: 2, rsdt: 0x1000, xsdt: Some(0x2000) };
        assert_eq!(find_table(&both, *b"FACP", |a, b| mem.read(a, b)), Ok(0x6000_0000_0000));
        let rsdt_only = Rsdp { revision: 0, rsdt: 0x1000, xsdt: None };
        assert_eq!(find_table(&rsdt_only, *b"FACP", |a, b| mem.read(a, b)), Ok(0x5000));
    }

    #[test]
    fn find_table_qemu_chain() {
        let mem = Mem(vec![
            (0xfb7d0e8, xsdt_with(&[0xfb7a000, 0xfb79000])),
            (0xfb7a000, qemu_fadt()),
            (0xfb79000, mk_sdt(b"APIC", 3, &[0; 8])),
        ]);
        let r = Rsdp { revision: 2, rsdt: 0xfb7d074, xsdt: Some(0xfb7d0e8) };
        assert_eq!(find_table(&r, *b"FACP", |a, b| mem.read(a, b)), Ok(0xfb7a000));
        assert_eq!(find_table(&r, *b"APIC", |a, b| mem.read(a, b)), Ok(0xfb79000));
    }

    #[test]
    fn find_table_not_found() {
        let mem = Mem(vec![
            (0x1000, rsdt_with(&[0x5000])),
            (0x5000, mk_sdt(b"APIC", 1, &[])),
        ]);
        let r = Rsdp { revision: 0, rsdt: 0x1000, xsdt: None };
        assert_eq!(find_table(&r, *b"FACP", |a, b| mem.read(a, b)), Err(AcpiError::NotFound));
    }

    #[test]
    fn find_table_wrong_root_signature() {
        let mem = Mem(vec![(0x1000, mk_sdt(b"XXXX", 1, &[]))]);
        let r = Rsdp { revision: 0, rsdt: 0x1000, xsdt: None };
        assert_eq!(find_table(&r, *b"FACP", |a, b| mem.read(a, b)), Err(AcpiError::BadSignature));
    }

    #[test]
    fn sdt_length_beyond_read_is_truncated_error() {
        // Root claims 10 entries but the readable memory ends after 1.
        let mut root = rsdt_with(&[0x5000]);
        root[4..8].copy_from_slice(&(36u32 + 40).to_le_bytes());
        let mem = Mem(vec![(0x1000, root), (0x5000, mk_sdt(b"APIC", 1, &[]))]);
        let r = Rsdp { revision: 0, rsdt: 0x1000, xsdt: None };
        assert_eq!(find_table(&r, *b"FACP", |a, b| mem.read(a, b)), Err(AcpiError::Truncated));
        // Root itself unreadable.
        let none = Mem(vec![]);
        assert_eq!(find_table(&r, *b"FACP", |a, b| none.read(a, b)), Err(AcpiError::Truncated));
    }

    #[test]
    fn find_table_hostile_lengths_do_not_panic() {
        for len in [0u32, 1, 35, 36, 37, 43, u32::MAX] {
            let mut root = rsdt_with(&[0x5000]);
            root[4..8].copy_from_slice(&len.to_le_bytes());
            let mem = Mem(vec![(0x1000, root)]);
            let r = Rsdp { revision: 0, rsdt: 0x1000, xsdt: None };
            let _ = find_table(&r, *b"FACP", |a, b| mem.read(a, b));
        }
        // Entry near u64::MAX / unreadable entry: skipped, no overflow.
        let mem = Mem(vec![(0x1000, xsdt_with(&[u64::MAX, 0x5000])), (0x5000, mk_sdt(b"FACP", 1, &[]))]);
        let r = Rsdp { revision: 2, rsdt: 0, xsdt: Some(0x1000) };
        assert_eq!(find_table(&r, *b"FACP", |a, b| mem.read(a, b)), Ok(0x5000));
        let r = Rsdp { revision: 2, rsdt: 0, xsdt: Some(u64::MAX - 4) };
        assert!(find_table(&r, *b"FACP", |a, b| mem.read(a, b)).is_err());
    }

    #[test]
    fn find_table_caps_entries_at_64() {
        // 70 entries; the wanted one is #66 and must not be reached.
        let mut entries = vec![0x5000u32; 70];
        entries[66] = 0x6000;
        let mem = Mem(vec![
            (0x1000, rsdt_with(&entries)),
            (0x5000, mk_sdt(b"APIC", 1, &[])),
            (0x6000, mk_sdt(b"FACP", 1, &[])),
        ]);
        let r = Rsdp { revision: 0, rsdt: 0x1000, xsdt: None };
        assert_eq!(find_table(&r, *b"FACP", |a, b| mem.read(a, b)), Err(AcpiError::NotFound));
    }

    #[test]
    fn fadt_legacy_pm1a() {
        let f = parse_fadt(&qemu_fadt()).unwrap();
        assert_eq!(
            f,
            Fadt { pm1a_cnt: 0xb004, pm1b_cnt: 0, dsdt: 0x0fb7_b000, smi_cmd: 0xb2, acpi_enable: 0xf1 }
        );
    }

    #[test]
    fn fadt_rev1_never_reads_x_fields() {
        // Bytes behind the 116-byte table are 0xaf filler; they must be ignored.
        let mut padded = qemu_fadt();
        padded.extend_from_slice(&[0xaf; 200]);
        let f = parse_fadt(&padded).unwrap();
        assert_eq!(f.dsdt, 0x0fb7_b000);
        assert_eq!(f.pm1a_cnt, 0xb004);
    }

    #[test]
    fn fadt_x_block_used_when_legacy_zero() {
        let t = big_fadt(|r| {
            r[FADT_DSDT..FADT_DSDT + 4].copy_from_slice(&0x1000u32.to_le_bytes());
            gas(r, FADT_X_PM1A_CNT, 1, 0x604);
            gas(r, FADT_X_PM1B_CNT, 1, 0x608);
        });
        let f = parse_fadt(&t).unwrap();
        assert_eq!((f.pm1a_cnt, f.pm1b_cnt), (0x604, 0x608));
    }

    #[test]
    fn fadt_x_block_memory_space_unsupported() {
        let t = big_fadt(|r| gas(r, FADT_X_PM1A_CNT, 0, 0xfed0_0000));
        assert_eq!(parse_fadt(&t), Err(AcpiError::Unsupported));
        let t = big_fadt(|r| gas(r, FADT_X_PM1A_CNT, 1, 0x1_0000));
        assert_eq!(parse_fadt(&t), Err(AcpiError::Unsupported));
    }

    #[test]
    fn fadt_x_dsdt_preferred() {
        let t = big_fadt(|r| {
            r[FADT_DSDT..FADT_DSDT + 4].copy_from_slice(&0x1000u32.to_le_bytes());
            r[FADT_X_DSDT..FADT_X_DSDT + 8].copy_from_slice(&0x2_0000_0000u64.to_le_bytes());
            r[FADT_PM1A_CNT..FADT_PM1A_CNT + 4].copy_from_slice(&0xb004u32.to_le_bytes());
        });
        assert_eq!(parse_fadt(&t).unwrap().dsdt, 0x2_0000_0000);
        let t = big_fadt(|r| {
            r[FADT_DSDT..FADT_DSDT + 4].copy_from_slice(&0x1000u32.to_le_bytes());
            r[FADT_PM1A_CNT..FADT_PM1A_CNT + 4].copy_from_slice(&0xb004u32.to_le_bytes());
        });
        assert_eq!(parse_fadt(&t).unwrap().dsdt, 0x1000);
    }

    #[test]
    fn fadt_malformed() {
        let t = qemu_fadt();
        assert_eq!(parse_fadt(&t[..60]), Err(AcpiError::Truncated));
        let mut bad = t.clone();
        bad[50] ^= 1;
        assert_eq!(parse_fadt(&bad), Err(AcpiError::BadChecksum));
        let other = mk_sdt(b"APIC", 1, &[0; 100]);
        assert_eq!(parse_fadt(&other), Err(AcpiError::BadSignature));
        // Valid checksum but length too small to hold the legacy fields.
        let tiny = mk_sdt(b"FACP", 1, &[0; 8]);
        assert_eq!(parse_fadt(&tiny), Err(AcpiError::Truncated));
        // No PM1a block anywhere: unusable for poweroff.
        let none = big_fadt(|_| {});
        assert_eq!(parse_fadt(&none), Err(AcpiError::Unsupported));
        // Legacy port above 16 bits.
        let wide = big_fadt(|r| r[FADT_PM1A_CNT..FADT_PM1A_CNT + 4].copy_from_slice(&0x1_0004u32.to_le_bytes()));
        assert_eq!(parse_fadt(&wide), Err(AcpiError::Unsupported));
        assert!(parse_fadt(&[]).is_err());
    }

    // QEMU: 5f 53 35 5f 12 06 04 00 00 00 00
    #[test]
    fn s5_qemu_fixture() {
        let mut d = vec![0x10, 0x3b, 0x5c, 0x2e, 0x5f];
        d.extend_from_slice(&[0x08, 0x5f, 0x53, 0x35, 0x5f, 0x12, 0x06, 0x04, 0x00, 0x00, 0x00, 0x00, 0x10, 0x3b]);
        assert_eq!(find_s5(&d), Some((0, 0)));
    }

    #[test]
    fn s5_byteprefix_values() {
        let d = [0x08, b'_', b'S', b'5', b'_', 0x12, 0x0a, 0x04, 0x0a, 0x05, 0x0a, 0x07, 0x00, 0x00];
        assert_eq!(find_s5(&d), Some((5, 7)));
    }

    #[test]
    fn s5_zero_and_one_ops() {
        let d = [b'_', b'S', b'5', b'_', 0x12, 0x06, 0x04, 0x01, 0x00, 0x00, 0x00];
        assert_eq!(find_s5(&d), Some((1, 0)));
    }

    #[test]
    fn s5_with_root_prefix() {
        let d = [0x08, 0x5c, b'_', b'S', b'5', b'_', 0x12, 0x07, 0x04, 0x0a, 0x03, 0x0a, 0x04, 0x00, 0x00];
        assert_eq!(find_s5(&d), Some((3, 4)));
    }

    #[test]
    fn s5_multibyte_pkglength() {
        // PkgLength 0x41: lead 0x41 (1 extra byte): low nibble 1 | 0x04 << 4 ...
        let d = [b'_', b'S', b'5', b'_', 0x12, 0x41, 0x04, 0x04, 0x0a, 0x02, 0x0a, 0x06];
        assert_eq!(find_s5(&d), Some((2, 6)));
        let d = [b'_', b'S', b'5', b'_', 0x12, 0xc1, 0x01, 0x00, 0x00, 0x04, 0x01, 0x01];
        assert_eq!(find_s5(&d), Some((1, 1)));
    }

    #[test]
    fn s5_missing_is_none() {
        assert_eq!(find_s5(&[]), None);
        assert_eq!(find_s5(&[0x12, 0x06, 0x04, 0, 0, 0, 0]), None);
        // _S5_ present but not followed by a package
        assert_eq!(find_s5(&[b'_', b'S', b'5', b'_', 0x0a, 0x01]), None);
        // wrong element encoding (WordPrefix) and too few elements
        assert_eq!(find_s5(&[b'_', b'S', b'5', b'_', 0x12, 0x06, 0x04, 0x0b, 0, 0, 0, 0]), None);
        assert_eq!(find_s5(&[b'_', b'S', b'5', b'_', 0x12, 0x03, 0x01, 0x00]), None);
    }

    #[test]
    fn s5_second_occurrence_used_if_first_is_junk() {
        let d = [
            b'_', b'S', b'5', b'_', 0xff, 0xff, //
            b'_', b'S', b'5', b'_', 0x12, 0x06, 0x04, 0x0a, 0x01, 0x0a, 0x02, 0x00,
        ];
        assert_eq!(find_s5(&d), Some((1, 2)));
    }

    #[test]
    fn s5_truncated_does_not_panic() {
        let full = [
            0x08u8, 0x5c, b'_', b'S', b'5', b'_', 0x12, 0x0a, 0x04, 0x0a, 0x05, 0x0a, 0x07, 0x00, 0x00,
        ];
        for n in 0..=full.len() {
            let _ = find_s5(&full[..n]);
        }
        assert_eq!(find_s5(&full[..12]), None);
        for lead in 0..=255u8 {
            let d = [b'_', b'S', b'5', b'_', 0x12, lead];
            let _ = find_s5(&d);
            let d = [b'_', b'S', b'5', b'_', 0x12, lead, 0xff, 0xff, 0xff, 0x04, 0x0a];
            let _ = find_s5(&d);
        }
    }

    #[test]
    fn s5_value_sets_slp_en() {
        assert_eq!(s5_value(0), 0x2000);
        assert_eq!(s5_value(5), 0x2000 | (5 << 10));
        assert_eq!(s5_value(0xff), 0x2000 | (7 << 10));
    }
}
