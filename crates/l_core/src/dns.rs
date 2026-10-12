//! Minimal, strict DNS client messages (A records over UDP).
//!
//! Responses come from the network and are untrusted. The parser accepts only
//! a well-formed answer to exactly the question that was asked (same ID, same
//! name, type A, class IN), follows CNAME chains within the answer section,
//! and rejects truncated, oversized, looping or inconsistent messages.

pub const MAX_NAME: usize = 253;
pub const MAX_LABEL: usize = 63;
pub const QUERY_MAX_LEN: usize = 12 + MAX_NAME + 2 + 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DnsError {
    InvalidName,
    BufferTooSmall,
    Malformed,
    /// Not a response, wrong ID, or not an answer to our question.
    Unexpected,
    Truncated,
    /// The server answered with an error code (NXDOMAIN, SERVFAIL, ...).
    ServerError(u8),
    NoAddress,
}

fn valid_name(name: &str) -> bool {
    let b = name.as_bytes();
    if b.is_empty() || b.len() > MAX_NAME {
        return false;
    }
    let mut label = 0;
    for (i, &c) in b.iter().enumerate() {
        match c {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' => {
                label += 1;
                if label > MAX_LABEL || (c == b'-' && (label == 1 || i + 1 == b.len() || b[i + 1] == b'.')) {
                    return false;
                }
            }
            b'.' => {
                if label == 0 || i + 1 == b.len() {
                    return false;
                }
                label = 0;
            }
            _ => return false,
        }
    }
    true
}

/// Build a recursive A query for `name`. Returns the message length.
pub fn encode_query(id: u16, name: &str, out: &mut [u8]) -> Result<usize, DnsError> {
    if !valid_name(name) {
        return Err(DnsError::InvalidName);
    }
    let need = 12 + name.len() + 2 + 4;
    if out.len() < need {
        return Err(DnsError::BufferTooSmall);
    }
    out[..12].fill(0);
    out[0..2].copy_from_slice(&id.to_be_bytes());
    out[2] = 0x01; // RD
    out[5] = 1; // QDCOUNT = 1
    let mut o = 12;
    for label in name.split('.') {
        out[o] = label.len() as u8;
        out[o + 1..o + 1 + label.len()].copy_from_slice(label.as_bytes());
        o += 1 + label.len();
    }
    out[o] = 0;
    out[o + 1..o + 3].copy_from_slice(&1u16.to_be_bytes()); // QTYPE A
    out[o + 3..o + 5].copy_from_slice(&1u16.to_be_bytes()); // QCLASS IN
    Ok(o + 5)
}

/// Read a (possibly compressed) name at `pos` into `out` (lowercased, dotted).
/// Returns the name length and the position just after the name in the
/// *original* byte stream.
fn read_name(msg: &[u8], mut pos: usize, out: &mut [u8; MAX_NAME + 1]) -> Result<(usize, usize), DnsError> {
    let mut len = 0usize;
    let mut next_after: Option<usize> = None;
    let mut jumps = 0;
    loop {
        let l = *msg.get(pos).ok_or(DnsError::Malformed)? as usize;
        match l & 0xc0 {
            0x00 => {
                if l == 0 {
                    let after = next_after.unwrap_or(pos + 1);
                    return Ok((len, after));
                }
                if l > MAX_LABEL {
                    return Err(DnsError::Malformed);
                }
                let label = msg.get(pos + 1..pos + 1 + l).ok_or(DnsError::Malformed)?;
                if len + l + 1 > MAX_NAME + 1 {
                    return Err(DnsError::Malformed);
                }
                if len > 0 {
                    out[len] = b'.';
                    len += 1;
                }
                for &c in label {
                    out[len] = c.to_ascii_lowercase();
                    len += 1;
                }
                pos += 1 + l;
            }
            0xc0 => {
                let lo = *msg.get(pos + 1).ok_or(DnsError::Malformed)? as usize;
                let target = (l & 0x3f) << 8 | lo;
                jumps += 1;
                if jumps > 16 || target >= pos {
                    // Pointers must go backwards; this also rules out loops.
                    return Err(DnsError::Malformed);
                }
                next_after.get_or_insert(pos + 2);
                pos = target;
            }
            _ => return Err(DnsError::Malformed), // reserved label types
        }
    }
}

/// Validate `msg` as the answer to the A query `(expect_id, name)` and return
/// the first IPv4 address that belongs to the queried name (directly or via a
/// CNAME chain).
pub fn parse_a_response(msg: &[u8], expect_id: u16, name: &str) -> Result<[u8; 4], DnsError> {
    if msg.len() < 12 {
        return Err(DnsError::Malformed);
    }
    if u16::from_be_bytes([msg[0], msg[1]]) != expect_id {
        return Err(DnsError::Unexpected);
    }
    let flags = u16::from_be_bytes([msg[2], msg[3]]);
    if flags & 0x8000 == 0 || (flags >> 11) & 0xf != 0 {
        return Err(DnsError::Unexpected); // not a response / not a standard query
    }
    if flags & 0x0200 != 0 {
        return Err(DnsError::Truncated);
    }
    let rcode = (flags & 0xf) as u8;
    if rcode != 0 {
        return Err(DnsError::ServerError(rcode));
    }
    let qd = u16::from_be_bytes([msg[4], msg[5]]);
    let an = u16::from_be_bytes([msg[6], msg[7]]) as usize;
    if qd != 1 || an > 32 {
        return Err(DnsError::Unexpected);
    }

    // Our own name, lowercased, to compare against.
    let mut want = [0u8; MAX_NAME + 1];
    let mut want_len = 0;
    for c in name.bytes() {
        want[want_len] = c.to_ascii_lowercase();
        want_len += 1;
    }
    let want = &want[..want_len];
    let want = if want.last() == Some(&b'.') { &want[..want.len() - 1] } else { want };

    // Question section must repeat exactly what we asked.
    let mut buf = [0u8; MAX_NAME + 1];
    let (n, mut pos) = read_name(msg, 12, &mut buf)?;
    if &buf[..n] != want {
        return Err(DnsError::Unexpected);
    }
    let q = msg.get(pos..pos + 4).ok_or(DnsError::Malformed)?;
    if q != [0, 1, 0, 1] {
        return Err(DnsError::Unexpected);
    }
    pos += 4;

    // Names that currently stand for the queried name: the name itself plus
    // targets of CNAMEs found so far (kept as small fixed copies).
    const MAX_ALIASES: usize = 8;
    let mut aliases = [[0u8; MAX_NAME + 1]; MAX_ALIASES];
    let mut alias_len = [0usize; MAX_ALIASES];
    aliases[0][..want.len()].copy_from_slice(want);
    alias_len[0] = want.len();
    let mut alias_count = 1;

    let mut found: Option<[u8; 4]> = None;
    for _ in 0..an {
        let (n, p) = read_name(msg, pos, &mut buf)?;
        let hdr = msg.get(p..p + 10).ok_or(DnsError::Malformed)?;
        let rtype = u16::from_be_bytes([hdr[0], hdr[1]]);
        let class = u16::from_be_bytes([hdr[2], hdr[3]]);
        let rdlen = u16::from_be_bytes([hdr[8], hdr[9]]) as usize;
        let rdata_pos = p + 10;
        let rdata = msg.get(rdata_pos..rdata_pos + rdlen).ok_or(DnsError::Malformed)?;
        pos = rdata_pos + rdlen;
        if class != 1 {
            continue;
        }
        let owner_ok = (0..alias_count).any(|i| &aliases[i][..alias_len[i]] == &buf[..n]);
        if !owner_ok {
            continue; // an answer about some other name: ignore, never trust
        }
        match rtype {
            1 if rdlen == 4 => {
                if found.is_none() {
                    found = Some([rdata[0], rdata[1], rdata[2], rdata[3]]);
                }
            }
            5 => {
                let mut target = [0u8; MAX_NAME + 1];
                let (tn, _) = read_name(msg, rdata_pos, &mut target)?;
                if alias_count < MAX_ALIASES {
                    aliases[alias_count][..tn].copy_from_slice(&target[..tn]);
                    alias_len[alias_count] = tn;
                    alias_count += 1;
                }
            }
            _ => {}
        }
    }
    found.ok_or(DnsError::NoAddress)
}

#[cfg(test)]
mod tests {
    use super::*;
    extern crate std;
    use std::vec::Vec;

    /// Build a response for `name` (id) with the given answer records
    /// `(owner_is_pointer_to_question, rtype, rdata)`.
    fn response(id: u16, name: &str, rcode: u8, answers: &[(&str, u16, Vec<u8>)]) -> Vec<u8> {
        let mut q = [0u8; QUERY_MAX_LEN];
        let n = encode_query(id, name, &mut q).unwrap();
        let mut m = q[..n].to_vec();
        m[2] = 0x81;
        m[3] = 0x80 | rcode;
        m[6..8].copy_from_slice(&(answers.len() as u16).to_be_bytes());
        for (owner, rtype, rdata) in answers {
            if *owner == "@q" {
                m.extend_from_slice(&[0xc0, 0x0c]); // pointer to the question name
            } else {
                for label in owner.split('.') {
                    m.push(label.len() as u8);
                    m.extend_from_slice(label.as_bytes());
                }
                m.push(0);
            }
            m.extend_from_slice(&rtype.to_be_bytes());
            m.extend_from_slice(&1u16.to_be_bytes());
            m.extend_from_slice(&60u32.to_be_bytes());
            m.extend_from_slice(&(rdata.len() as u16).to_be_bytes());
            m.extend_from_slice(rdata);
        }
        m
    }

    fn cname(target: &str) -> Vec<u8> {
        let mut v = Vec::new();
        for label in target.split('.') {
            v.push(label.len() as u8);
            v.extend_from_slice(label.as_bytes());
        }
        v.push(0);
        v
    }

    #[test]
    fn encodes_a_wellformed_query() {
        let mut b = [0u8; QUERY_MAX_LEN];
        let n = encode_query(0x1234, "Test.example", &mut b).unwrap();
        assert_eq!(&b[..2], &[0x12, 0x34]);
        assert_eq!(b[2], 0x01);
        assert_eq!(&b[4..6], &[0, 1]);
        assert_eq!(&b[12..n], b"\x04Test\x07example\x00\x00\x01\x00\x01");
    }

    #[test]
    fn rejects_bad_names() {
        let mut b = [0u8; QUERY_MAX_LEN];
        for bad in ["", ".", "a..b", "a b.com", "-a.com", "a-.com", "a.com.", "ä.com", &"a".repeat(64), &["a.";130].concat()] {
            assert_eq!(encode_query(1, bad, &mut b), Err(DnsError::InvalidName), "{bad:?}");
        }
        assert_eq!(encode_query(1, "a.com", &mut b[..10]), Err(DnsError::BufferTooSmall));
    }

    #[test]
    fn accepts_a_direct_answer() {
        let r = response(7, "test.example", 0, &[("@q", 1, std::vec![10, 0, 2, 2])]);
        assert_eq!(parse_a_response(&r, 7, "test.example"), Ok([10, 0, 2, 2]));
        assert_eq!(parse_a_response(&r, 7, "TEST.example."), Ok([10, 0, 2, 2])); // case-insensitive
    }

    #[test]
    fn follows_cname_chains_but_ignores_unrelated_records() {
        let r = response(9, "www.test.example", 0, &[
            ("evil.example", 1, std::vec![6, 6, 6, 6]),             // unrelated owner: ignored
            ("@q", 5, cname("edge.cdn.example")),
            ("edge.cdn.example", 1, std::vec![93, 184, 216, 34]),
        ]);
        assert_eq!(parse_a_response(&r, 9, "www.test.example"), Ok([93, 184, 216, 34]));
    }

    #[test]
    fn rejects_wrong_id_wrong_question_and_non_responses() {
        let ok = response(5, "test.example", 0, &[("@q", 1, std::vec![1, 2, 3, 4])]);
        assert_eq!(parse_a_response(&ok, 6, "test.example"), Err(DnsError::Unexpected)); // id
        assert_eq!(parse_a_response(&ok, 5, "other.example"), Err(DnsError::Unexpected)); // question
        let mut q = ok.clone();
        q[2] &= 0x7f; // QR bit cleared: this is a query, not a response
        assert_eq!(parse_a_response(&q, 5, "test.example"), Err(DnsError::Unexpected));
    }

    #[test]
    fn reports_server_errors_and_truncation() {
        let nx = response(5, "test.example", 3, &[]);
        assert_eq!(parse_a_response(&nx, 5, "test.example"), Err(DnsError::ServerError(3)));
        let mut tc = response(5, "test.example", 0, &[("@q", 1, std::vec![1, 2, 3, 4])]);
        tc[2] |= 0x02;
        assert_eq!(parse_a_response(&tc, 5, "test.example"), Err(DnsError::Truncated));
        let none = response(5, "test.example", 0, &[]);
        assert_eq!(parse_a_response(&none, 5, "test.example"), Err(DnsError::NoAddress));
    }

    #[test]
    fn rejects_compression_loops_and_forward_pointers() {
        let mut r = response(5, "test.example", 0, &[("@q", 1, std::vec![1, 2, 3, 4])]);
        let q_end = 12 + "test.example".len() + 2 + 4;
        r[q_end] = 0xc0;
        r[q_end + 1] = q_end as u8; // answer owner points at itself
        assert_eq!(parse_a_response(&r, 5, "test.example"), Err(DnsError::Malformed));
        r[q_end + 1] = (q_end + 2) as u8; // forward pointer
        assert_eq!(parse_a_response(&r, 5, "test.example"), Err(DnsError::Malformed));
    }

    #[test]
    fn never_panics_on_garbage() {
        let good = response(5, "test.example", 0, &[("@q", 1, std::vec![1, 2, 3, 4])]);
        for len in 0..good.len() {
            let _ = parse_a_response(&good[..len], 5, "test.example");
        }
        for i in 0..good.len() {
            for x in [0u8, 0xff, 0xc0, 0x3f, 0x80] {
                let mut m = good.clone();
                m[i] = x;
                let _ = parse_a_response(&m, 5, "test.example");
            }
        }
    }

    #[test]
    fn rdlength_past_the_end_is_malformed() {
        let mut r = response(5, "test.example", 0, &[("@q", 1, std::vec![1, 2, 3, 4])]);
        let l = r.len();
        r[l - 5] = 0xff; // rdlength high byte
        assert_eq!(parse_a_response(&r, 5, "test.example"), Err(DnsError::Malformed));
    }
}
