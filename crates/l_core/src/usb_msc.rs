//! USB Mass Storage, Bulk-Only Transport (BOT) with a read-only SCSI subset.
//!
//! Read-only by construction: [`Cbw`] has no public constructor except
//! `test_unit_ready`, `read_capacity10` and `read10`; no write command can be
//! built. All device-supplied bytes (CSW, capacity) are untrusted.

/// Errors of the BOT layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MscError {
    /// CSW is not exactly 13 bytes / capacity response shorter than 8 bytes.
    Short,
    /// CSW signature is not "USBS".
    BadSignature,
    /// CSW tag does not echo the CBW tag.
    TagMismatch,
    /// CSW status byte is not 0, 1 or 2.
    BadStatus,
    /// Block length not a power of two in 512..=4096.
    BadBlockLen,
    /// Capacity does not fit READ CAPACITY(10) (last LBA 0xFFFF_FFFF).
    TooLarge,
}

const CBW_SIG: [u8; 4] = *b"USBC";
const CSW_SIG: [u8; 4] = *b"USBS";

/// Command Block Wrapper. Only the three read-only commands are constructible.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cbw {
    tag: u32,
    data_len: u32,
    dir_in: bool,
    cb_len: u8,
    cb: [u8; 16],
}

impl Cbw {
    /// TEST UNIT READY (0x00), no data.
    pub fn test_unit_ready(tag: u32) -> Self {
        Cbw { tag, data_len: 0, dir_in: false, cb_len: 6, cb: [0; 16] }
    }

    /// READ CAPACITY(10) (0x25), 8 bytes in.
    pub fn read_capacity10(tag: u32) -> Self {
        let mut cb = [0u8; 16];
        cb[0] = 0x25;
        Cbw { tag, data_len: 8, dir_in: true, cb_len: 10, cb }
    }

    /// READ(10) (0x28) of `blocks` blocks of `block_len` bytes at `lba`.
    /// The data length saturates instead of wrapping (cannot occur for
    /// block lengths accepted by [`parse_read_capacity10`]).
    pub fn read10(tag: u32, lba: u32, blocks: u16, block_len: u32) -> Self {
        let mut cb = [0u8; 16];
        cb[0] = 0x28;
        cb[2..6].copy_from_slice(&lba.to_be_bytes());
        cb[7..9].copy_from_slice(&blocks.to_be_bytes());
        Cbw {
            tag,
            data_len: (blocks as u32).saturating_mul(block_len),
            dir_in: true,
            cb_len: 10,
            cb,
        }
    }

    pub fn encode(&self) -> [u8; 31] {
        let mut b = [0u8; 31];
        b[0..4].copy_from_slice(&CBW_SIG);
        b[4..8].copy_from_slice(&self.tag.to_le_bytes());
        b[8..12].copy_from_slice(&self.data_len.to_le_bytes());
        b[12] = if self.dir_in { 0x80 } else { 0 };
        b[13] = 0; // LUN 0
        b[14] = self.cb_len;
        b[15..31].copy_from_slice(&self.cb);
        b
    }

    pub fn data_len(&self) -> u32 {
        self.data_len
    }

    pub fn dir_in(&self) -> bool {
        self.dir_in
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CswStatus {
    Passed,
    Failed,
    PhaseError,
}

/// Parse a Command Status Wrapper (exactly 13 bytes), checking signature and
/// that the tag echoes `tag`. Returns the status and the data residue.
pub fn parse_csw(b: &[u8], tag: u32) -> Result<(CswStatus, u32), MscError> {
    if b.len() != 13 {
        return Err(MscError::Short);
    }
    if b[0..4] != CSW_SIG {
        return Err(MscError::BadSignature);
    }
    let got = u32::from_le_bytes([b[4], b[5], b[6], b[7]]);
    if got != tag {
        return Err(MscError::TagMismatch);
    }
    let residue = u32::from_le_bytes([b[8], b[9], b[10], b[11]]);
    let status = match b[12] {
        0 => CswStatus::Passed,
        1 => CswStatus::Failed,
        2 => CswStatus::PhaseError,
        _ => return Err(MscError::BadStatus),
    };
    Ok((status, residue))
}

/// Parse the 8-byte READ CAPACITY(10) response into (last LBA, block length).
pub fn parse_read_capacity10(b: &[u8]) -> Result<(u32, u32), MscError> {
    if b.len() < 8 {
        return Err(MscError::Short);
    }
    let last = u32::from_be_bytes([b[0], b[1], b[2], b[3]]);
    let len = u32::from_be_bytes([b[4], b[5], b[6], b[7]]);
    if !(512..=4096).contains(&len) || !len.is_power_of_two() {
        return Err(MscError::BadBlockLen);
    }
    if last == u32::MAX {
        return Err(MscError::TooLarge);
    }
    Ok((last, len))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cbw_signature_and_length_31() {
        let b = Cbw::test_unit_ready(0xAABBCCDD).encode();
        assert_eq!(b.len(), 31);
        assert_eq!(&b[0..4], b"USBC");
        assert_eq!(u32::from_le_bytes(b[0..4].try_into().unwrap()), 0x4342_5355);
        assert_eq!(u32::from_le_bytes(b[4..8].try_into().unwrap()), 0xAABBCCDD);
        assert_eq!(u32::from_le_bytes(b[8..12].try_into().unwrap()), 0);
        assert_eq!(b[12], 0x00); // OUT, no data
        assert_eq!(b[13], 0); // LUN
        assert_eq!(b[14], 6); // CB length
        assert!(b[15..].iter().all(|&x| x == 0)); // opcode 0x00
        let c = Cbw::test_unit_ready(1);
        assert_eq!(c.data_len(), 0);
        assert!(!c.dir_in());
    }

    #[test]
    fn read10_cdb_big_endian() {
        let b = Cbw::read10(7, 0x0102_0304, 0x0506, 512).encode();
        assert_eq!(b[12], 0x80);
        assert_eq!(b[14], 10);
        assert_eq!(
            &b[15..31],
            &[0x28, 0, 0x01, 0x02, 0x03, 0x04, 0, 0x05, 0x06, 0, 0, 0, 0, 0, 0, 0]
        );
        let c = Cbw::read_capacity10(9).encode();
        assert_eq!(c[12], 0x80);
        assert_eq!(c[14], 10);
        assert_eq!(u32::from_le_bytes(c[8..12].try_into().unwrap()), 8);
        assert_eq!(&c[15..31], &[0x25, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        assert!(Cbw::read_capacity10(9).dir_in());
        assert_eq!(Cbw::read_capacity10(9).data_len(), 8);
    }

    #[test]
    fn read10_data_len_is_blocks_times_len() {
        let c = Cbw::read10(1, 0, 8, 512);
        assert_eq!(c.data_len(), 4096);
        assert!(c.dir_in());
        let b = c.encode();
        assert_eq!(u32::from_le_bytes(b[8..12].try_into().unwrap()), 4096);
        assert_eq!(Cbw::read10(1, 0, 0xFFFF, 4096).data_len(), 0xFFFF * 4096);
    }

    fn csw(tag: u32, residue: u32, status: u8) -> [u8; 13] {
        let mut b = [0u8; 13];
        b[0..4].copy_from_slice(b"USBS");
        b[4..8].copy_from_slice(&tag.to_le_bytes());
        b[8..12].copy_from_slice(&residue.to_le_bytes());
        b[12] = status;
        b
    }

    #[test]
    fn csw_ok() {
        assert_eq!(parse_csw(&csw(5, 0, 0), 5), Ok((CswStatus::Passed, 0)));
        assert_eq!(parse_csw(&csw(5, 12, 1), 5), Ok((CswStatus::Failed, 12)));
        assert_eq!(parse_csw(&csw(5, 0, 2), 5), Ok((CswStatus::PhaseError, 0)));
        assert_eq!(parse_csw(&csw(5, 0, 3), 5), Err(MscError::BadStatus));
    }

    #[test]
    fn csw_wrong_signature_rejected() {
        let mut b = csw(5, 0, 0);
        b[0] = b'X';
        assert_eq!(parse_csw(&b, 5), Err(MscError::BadSignature));
    }

    #[test]
    fn csw_tag_mismatch_rejected() {
        assert_eq!(parse_csw(&csw(6, 0, 0), 5), Err(MscError::TagMismatch));
    }

    #[test]
    fn csw_short_rejected() {
        let b = csw(5, 0, 0);
        assert_eq!(parse_csw(&b[..12], 5), Err(MscError::Short));
        assert_eq!(parse_csw(&[], 5), Err(MscError::Short));
        assert_eq!(parse_csw(&[0u8; 14], 5), Err(MscError::Short));
    }

    #[test]
    fn read_capacity_parse() {
        let b = [0x00, 0x0F, 0xFF, 0xFF, 0x00, 0x00, 0x02, 0x00];
        assert_eq!(parse_read_capacity10(&b), Ok((0x000F_FFFF, 512)));
        let b = [0, 0, 0, 0, 0, 0, 0x10, 0];
        assert_eq!(parse_read_capacity10(&b), Ok((0, 4096)));
        assert_eq!(parse_read_capacity10(&b[..7]), Err(MscError::Short));
        let b = [0xFF, 0xFF, 0xFF, 0xFF, 0, 0, 2, 0];
        assert_eq!(parse_read_capacity10(&b), Err(MscError::TooLarge));
    }

    #[test]
    fn read_capacity_bad_block_len_rejected() {
        for bl in [0u32, 1, 256, 511, 513, 1000, 3000, 8192, 0xFFFF_FFFF] {
            let mut b = [0u8; 8];
            b[4..8].copy_from_slice(&bl.to_be_bytes());
            assert_eq!(parse_read_capacity10(&b), Err(MscError::BadBlockLen), "{bl}");
        }
    }
}
