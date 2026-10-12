//! Just enough HTTP/1.x for a text-mode `fetch`: build a request, parse the
//! response head, and sanitize server-controlled text before it reaches the
//! terminal.

pub const MAX_HEAD: usize = 8 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HttpError {
    /// The head is not complete yet (need more bytes).
    Incomplete,
    /// Head larger than [`MAX_HEAD`] without terminating.
    TooLarge,
    BadStatusLine,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Head<'a> {
    pub status: u16,
    pub reason: &'a str,
    pub location: Option<&'a str>,
    pub content_type: Option<&'a str>,
    /// Number of bytes the head occupies, including the blank line.
    pub head_len: usize,
}

/// Build `GET target HTTP/1.0` (no chunked encoding, no keep-alive). Writes into
/// `out`; returns the length, or `None` if it does not fit.
pub fn build_request(host: &str, target: &str, out: &mut [u8]) -> Option<usize> {
    let parts: [&str; 7] = [
        "GET ", target, " HTTP/1.0\r\nHost: ", host,
        "\r\nUser-Agent: LarpOS-fetch/0.1\r\nAccept: text/*\r\nConnection: close\r\n\r\n", "", "",
    ];
    let mut n = 0;
    for p in parts {
        let b = p.as_bytes();
        if n + b.len() > out.len() {
            return None;
        }
        out[n..n + b.len()].copy_from_slice(b);
        n += b.len();
    }
    Some(n)
}

/// Parse the status line and the few headers we use.
pub fn parse_head(buf: &[u8]) -> Result<Head<'_>, HttpError> {
    let end = match buf.windows(4).position(|w| w == b"\r\n\r\n") {
        Some(i) => i + 4,
        None if buf.len() >= MAX_HEAD => return Err(HttpError::TooLarge),
        None => return Err(HttpError::Incomplete),
    };
    if end > MAX_HEAD {
        return Err(HttpError::TooLarge);
    }
    let head = core::str::from_utf8(&buf[..end - 4]).map_err(|_| HttpError::BadStatusLine)?;
    let mut lines = head.split("\r\n");
    let status_line = lines.next().ok_or(HttpError::BadStatusLine)?;
    let mut it = status_line.splitn(3, ' ');
    let version = it.next().ok_or(HttpError::BadStatusLine)?;
    if !(version == "HTTP/1.0" || version == "HTTP/1.1") {
        return Err(HttpError::BadStatusLine);
    }
    let code = it.next().ok_or(HttpError::BadStatusLine)?;
    if code.len() != 3 || !code.bytes().all(|c| c.is_ascii_digit()) {
        return Err(HttpError::BadStatusLine);
    }
    let status: u16 = code.parse().map_err(|_| HttpError::BadStatusLine)?;
    let reason = it.next().unwrap_or("");
    let (mut location, mut content_type) = (None, None);
    for line in lines {
        if let Some((name, value)) = line.split_once(':') {
            let value = value.trim_matches(|c| c == ' ' || c == '\t');
            if name.eq_ignore_ascii_case("location") {
                location = Some(value);
            } else if name.eq_ignore_ascii_case("content-type") {
                content_type = Some(value);
            }
        }
    }
    Ok(Head { status, reason, location, content_type, head_len: end })
}

/// What a byte from the network may become on screen: printable ASCII and
/// newlines pass; tabs become spaces; everything else (ESC, backspace, form
/// feed, NUL, high bytes, ...) becomes `.` so server data can never drive the
/// terminal.
pub fn sanitize(b: u8) -> u8 {
    match b {
        b'\n' => b'\n',
        b'\t' => b' ',
        0x20..=0x7e => b,
        _ => b'.',
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    extern crate std;

    #[test]
    fn builds_a_plain_request() {
        let mut b = [0u8; 256];
        let n = build_request("example.com", "/a?b=1", &mut b).unwrap();
        let s = core::str::from_utf8(&b[..n]).unwrap();
        assert!(s.starts_with("GET /a?b=1 HTTP/1.0\r\nHost: example.com\r\n"));
        assert!(s.ends_with("\r\n\r\n"));
        assert!(!s.contains("Cookie") && !s.contains("Referer"));
        assert_eq!(build_request("example.com", "/", &mut b[..20]), None);
    }

    #[test]
    fn parses_status_and_headers() {
        let raw = b"HTTP/1.1 301 Moved Permanently\r\nLocation: https://x.example/new\r\nContent-Type: text/html; charset=utf-8\r\n\r\nbody";
        let h = parse_head(raw).unwrap();
        assert_eq!((h.status, h.reason), (301, "Moved Permanently"));
        assert_eq!(h.location, Some("https://x.example/new"));
        assert_eq!(h.content_type, Some("text/html; charset=utf-8"));
        assert_eq!(&raw[h.head_len..], b"body");
    }

    #[test]
    fn incomplete_oversized_and_malformed_heads() {
        assert_eq!(parse_head(b"HTTP/1.1 200 OK\r\nA: b\r\n"), Err(HttpError::Incomplete));
        let big = std::vec![b'a'; MAX_HEAD];
        assert_eq!(parse_head(&big), Err(HttpError::TooLarge));
        for bad in [&b"FTP/1.1 200 OK\r\n\r\n"[..], b"HTTP/2 200 OK\r\n\r\n", b"HTTP/1.1 20 OK\r\n\r\n", b"HTTP/1.1 2x0 OK\r\n\r\n", b"\r\n\r\n", b"HTTP/1.1\r\n\r\n", b"HTTP/1.1 200 \xff\xfe\r\n\r\n"] {
            assert_eq!(parse_head(bad), Err(HttpError::BadStatusLine), "{:?}", std::string::String::from_utf8_lossy(bad));
        }
    }

    #[test]
    fn sanitize_neutralizes_terminal_controls() {
        let evil = b"ok\x1b[2J\x08\x0c\x00\x7f\xff\rtail\tx\n";
        let clean: std::vec::Vec<u8> = evil.iter().map(|&b| sanitize(b)).collect();
        // ESC, BS, FF, NUL, DEL, 0xFF and CR all become '.'; TAB becomes a space.
        assert_eq!(clean.as_slice(), b"ok.[2J......tail x\n".as_slice());
        for b in 0..=255u8 {
            let c = sanitize(b);
            assert!(c == b'\n' || (0x20..=0x7e).contains(&c), "byte {b:#x} -> {c:#x}");
        }
    }

    #[test]
    fn no_panic_on_garbage() {
        for len in 0..64 {
            let junk: std::vec::Vec<u8> = (0..len).map(|i| (i * 37 % 256) as u8).collect();
            let _ = parse_head(&junk);
        }
    }
}
