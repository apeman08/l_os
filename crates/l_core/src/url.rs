//! Strict `https://` URL parsing for `fetch`.
//!
//! Only `https` is accepted (an `http://` URL is refused, never "upgraded" or
//! tried in the clear). No user info, no IPv6 literals, ASCII only, bounded
//! length. The fragment is dropped (it is never sent to the server).

pub const MAX_URL: usize = 400;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UrlError {
    Empty,
    TooLong,
    /// Contains whitespace, control or non-ASCII characters.
    BadCharacter,
    /// `http://` and every other scheme.
    NotHttps,
    HasUserInfo,
    BadHost,
    BadPort,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Url<'a> {
    pub host: &'a str,
    pub port: u16,
    /// Path and query, always starting with `/`.
    pub target: &'a str,
}

pub fn parse(s: &str) -> Result<Url<'_>, UrlError> {
    if s.is_empty() {
        return Err(UrlError::Empty);
    }
    if s.len() > MAX_URL {
        return Err(UrlError::TooLong);
    }
    if !s.bytes().all(|b| (0x21..=0x7e).contains(&b)) {
        return Err(UrlError::BadCharacter);
    }
    let rest = match s.split_once("://") {
        Some((scheme, rest)) if scheme.eq_ignore_ascii_case("https") => rest,
        _ => return Err(UrlError::NotHttps),
    };
    let rest = rest.split('#').next().unwrap_or("");
    let (authority, target) = match rest.find(['/', '?']) {
        Some(i) if rest.as_bytes()[i] == b'/' => (&rest[..i], &rest[i..]),
        Some(i) => (&rest[..i], &rest[i..]), // query without a path: handled below
        None => (rest, "/"),
    };
    if authority.contains('@') {
        return Err(UrlError::HasUserInfo);
    }
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) => {
            if p.is_empty() || !p.bytes().all(|c| c.is_ascii_digit()) {
                return Err(UrlError::BadPort);
            }
            match p.parse::<u16>() {
                Ok(n) if n != 0 => (h, n),
                _ => return Err(UrlError::BadPort),
            }
        }
        None => (authority, 443),
    };
    let host_ok = !host.is_empty()
        && host.len() <= 253
        && host.split('.').all(|l| {
            !l.is_empty()
                && l.len() <= 63
                && !l.starts_with('-')
                && !l.ends_with('-')
                && l.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-')
        });
    if !host_ok {
        return Err(UrlError::BadHost);
    }
    // A bare "?query" target still needs the leading slash for the request line.
    if target.starts_with('?') {
        return Ok(Url { host, port, target: "/" }); // query dropped: documented limitation
    }
    Ok(Url { host, port, target })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_common_forms() {
        assert_eq!(parse("https://example.com"), Ok(Url { host: "example.com", port: 443, target: "/" }));
        assert_eq!(parse("https://example.com/"), Ok(Url { host: "example.com", port: 443, target: "/" }));
        assert_eq!(parse("HTTPS://Example.com:8443/a/b?q=1#frag"), Ok(Url { host: "Example.com", port: 8443, target: "/a/b?q=1" }));
        assert_eq!(parse("https://10.0.2.2:18443/x"), Ok(Url { host: "10.0.2.2", port: 18443, target: "/x" }));
    }

    #[test]
    fn plain_http_and_other_schemes_are_refused() {
        for u in ["http://example.com/", "ftp://example.com", "example.com", "//example.com", "file:///etc/passwd", "javascript:alert(1)", "httpss://example.com", "https:/example.com"] {
            assert_eq!(parse(u), Err(UrlError::NotHttps), "{u}");
        }
    }

    #[test]
    fn rejects_user_info_odd_hosts_and_ports() {
        assert_eq!(parse("https://user:pw@example.com/"), Err(UrlError::HasUserInfo));
        assert_eq!(parse("https://@example.com/"), Err(UrlError::HasUserInfo));
        for u in ["https://", "https:///x", "https://-a.com/", "https://a..com/", "https://a_b.com/", "https://[::1]/", "https://exa%6dple.com/", &["https://", &"a".repeat(64), ".com/"].concat()] {
            assert!(matches!(parse(u), Err(UrlError::BadHost) | Err(UrlError::BadPort)), "{u}");
        }
        for u in ["https://a.com:0/", "https://a.com:65536/", "https://a.com:/", "https://a.com:80a/", "https://a.com:-1/"] {
            assert_eq!(parse(u), Err(UrlError::BadPort), "{u}");
        }
    }

    #[test]
    fn rejects_control_whitespace_and_non_ascii() {
        for u in ["https://exa mple.com/", "https://example.com/a b", "https://example.com/\r\nHost: evil", "https://example.com/\u{7f}", "https://exämple.com/", "https://example.com/\t"] {
            assert_eq!(parse(u), Err(UrlError::BadCharacter), "{u:?}");
        }
        assert_eq!(parse(""), Err(UrlError::Empty));
        assert_eq!(parse(&["https://a.com/", &"x".repeat(400)].concat()), Err(UrlError::TooLong));
    }

    #[test]
    fn request_target_never_contains_line_breaks() {
        // Whitespace/control characters are rejected up front, so a URL can
        // never smuggle extra header lines into the request.
        for u in ["https://a.com/x%0d%0aHost:evil"] {
            let url = parse(u).unwrap();
            assert!(!url.target.contains('\r') && !url.target.contains('\n'));
        }
    }
}
