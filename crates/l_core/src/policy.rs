//! Network authorization policy (pure logic, default deny).
//!
//! IMPORTANT: this is only the *decision* engine. A decision is meaningful
//! only if the kernel consults it at a boundary untrusted code cannot bypass.
//! No such boundary exists yet (no user mode, no network); until then nothing
//! in the OS is protected by this module.
//!
//! Rules:
//! * Nothing is allowed unless an exact (host, port) grant exists.
//! * Hosts are matched exactly (case-insensitive). No wildcards, no implicit
//!   subdomains, no trailing-dot variants.
//! * Anything that fails validation is denied (fail closed).
//! * Grants may expire; expired grants deny.

pub const MAX_GRANTS: usize = 16;
pub const MAX_HOST_LEN: usize = 253;
const MAX_LABEL_LEN: usize = 63;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolicyError {
    InvalidHost,
    InvalidPort,
    TableFull,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DenyReason {
    /// No grant exists for this destination.
    NoGrant,
    /// A grant existed but has expired.
    Expired,
    /// The requested destination is malformed.
    InvalidDestination,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Allow,
    Deny(DenyReason),
}

#[derive(Clone, Copy)]
struct Host {
    bytes: [u8; MAX_HOST_LEN],
    len: usize,
}

impl Host {
    /// Validate and lowercase a hostname or IPv4 literal.
    fn parse(s: &str) -> Result<Self, PolicyError> {
        let b = s.as_bytes();
        if b.is_empty() || b.len() > MAX_HOST_LEN {
            return Err(PolicyError::InvalidHost);
        }
        let mut host = Host { bytes: [0; MAX_HOST_LEN], len: b.len() };
        let mut label_len = 0usize;
        let mut prev_dot_or_start = true;
        for (i, &c) in b.iter().enumerate() {
            let c = c.to_ascii_lowercase();
            match c {
                b'a'..=b'z' | b'0'..=b'9' => {
                    label_len += 1;
                    prev_dot_or_start = false;
                }
                b'-' => {
                    // A label may not start with '-'.
                    if prev_dot_or_start {
                        return Err(PolicyError::InvalidHost);
                    }
                    label_len += 1;
                    prev_dot_or_start = false;
                }
                b'.' => {
                    // No empty labels, no leading dot (trailing dot caught below).
                    if prev_dot_or_start || b[i - 1] == b'-' {
                        return Err(PolicyError::InvalidHost);
                    }
                    label_len = 0;
                    prev_dot_or_start = true;
                }
                _ => return Err(PolicyError::InvalidHost),
            }
            if label_len > MAX_LABEL_LEN {
                return Err(PolicyError::InvalidHost);
            }
            host.bytes[i] = c;
        }
        // Reject trailing dot and trailing '-'.
        if prev_dot_or_start || b[b.len() - 1] == b'-' {
            return Err(PolicyError::InvalidHost);
        }
        Ok(host)
    }

    fn as_bytes(&self) -> &[u8] {
        &self.bytes[..self.len]
    }
}

#[derive(Clone, Copy)]
struct Grant {
    host: Host,
    port: u16,
    /// Tick at which the grant stops being valid; `None` = until revoked.
    expires_at: Option<u64>,
}

pub struct NetPolicy {
    grants: [Option<Grant>; MAX_GRANTS],
}

impl NetPolicy {
    /// An empty policy: everything is denied.
    pub const fn new() -> Self {
        Self { grants: [None; MAX_GRANTS] }
    }

    /// Authorize `host:port`. Re-granting an existing destination replaces its
    /// expiry instead of adding a second entry.
    pub fn grant(&mut self, host: &str, port: u16, expires_at: Option<u64>) -> Result<(), PolicyError> {
        if port == 0 {
            return Err(PolicyError::InvalidPort);
        }
        let host = Host::parse(host)?;
        if let Some(slot) = self.find(host.as_bytes(), port) {
            if let Some(g) = &mut self.grants[slot] {
                g.expires_at = expires_at;
            }
            return Ok(());
        }
        let free = self.grants.iter().position(Option::is_none).ok_or(PolicyError::TableFull)?;
        self.grants[free] = Some(Grant { host, port, expires_at });
        Ok(())
    }

    /// Remove a grant. Returns whether one existed. Invalid input revokes nothing.
    pub fn revoke(&mut self, host: &str, port: u16) -> bool {
        let Ok(host) = Host::parse(host) else { return false };
        match self.find(host.as_bytes(), port) {
            Some(slot) => {
                self.grants[slot] = None;
                true
            }
            None => false,
        }
    }

    /// Remove every grant.
    pub fn revoke_all(&mut self) {
        self.grants = [None; MAX_GRANTS];
    }

    /// Decide whether `host:port` may be contacted at tick `now`.
    pub fn check(&self, host: &str, port: u16, now: u64) -> Decision {
        let Ok(host) = Host::parse(host) else {
            return Decision::Deny(DenyReason::InvalidDestination);
        };
        if port == 0 {
            return Decision::Deny(DenyReason::InvalidDestination);
        }
        match self.find(host.as_bytes(), port).and_then(|s| self.grants[s].as_ref()) {
            None => Decision::Deny(DenyReason::NoGrant),
            Some(g) => match g.expires_at {
                Some(t) if now >= t => Decision::Deny(DenyReason::Expired),
                _ => Decision::Allow,
            },
        }
    }

    /// Current grants as `(host, port, expires_at)`, in table order.
    pub fn grants(&self) -> impl Iterator<Item = (&str, u16, Option<u64>)> {
        self.grants.iter().flatten().map(|g| {
            // Hosts are validated ASCII, so this cannot fail.
            (core::str::from_utf8(g.host.as_bytes()).unwrap_or(""), g.port, g.expires_at)
        })
    }

    pub fn grant_count(&self) -> usize {
        self.grants.iter().flatten().count()
    }

    fn find(&self, host: &[u8], port: u16) -> Option<usize> {
        self.grants.iter().position(|g| matches!(g, Some(g) if g.port == port && g.host.as_bytes() == host))
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use Decision::{Allow, Deny};

    #[test]
    fn default_is_deny_everything() {
        let p = NetPolicy::new();
        assert_eq!(p.check("example.com", 443, 0), Deny(DenyReason::NoGrant));
        assert_eq!(p.check("127.0.0.1", 80, 0), Deny(DenyReason::NoGrant));
        assert_eq!(p.grant_count(), 0);
    }

    #[test]
    fn grant_is_exact_on_host_and_port() {
        let mut p = NetPolicy::new();
        p.grant("example.com", 443, None).unwrap();
        assert_eq!(p.check("example.com", 443, 5), Allow);
        assert_eq!(p.check("EXAMPLE.com", 443, 5), Allow); // case-insensitive
        assert_eq!(p.check("example.com", 80, 5), Deny(DenyReason::NoGrant));
        assert_eq!(p.check("sub.example.com", 443, 5), Deny(DenyReason::NoGrant));
        assert_eq!(p.check("badexample.com", 443, 5), Deny(DenyReason::NoGrant));
        assert_eq!(p.check("example.com.evil.com", 443, 5), Deny(DenyReason::NoGrant));
        assert_eq!(p.check("example.org", 443, 5), Deny(DenyReason::NoGrant));
    }

    #[test]
    fn revoked_permission_is_denied() {
        let mut p = NetPolicy::new();
        p.grant("example.com", 443, None).unwrap();
        assert!(p.revoke("Example.COM", 443));
        assert_eq!(p.check("example.com", 443, 0), Deny(DenyReason::NoGrant));
        assert!(!p.revoke("example.com", 443)); // already gone
    }

    #[test]
    fn revoke_all_clears_everything() {
        let mut p = NetPolicy::new();
        p.grant("a.example", 1, None).unwrap();
        p.grant("b.example", 2, None).unwrap();
        p.revoke_all();
        assert_eq!(p.grant_count(), 0);
        assert_eq!(p.check("a.example", 1, 0), Deny(DenyReason::NoGrant));
    }

    #[test]
    fn expiry_boundary_is_exclusive() {
        let mut p = NetPolicy::new();
        p.grant("example.com", 443, Some(100)).unwrap();
        assert_eq!(p.check("example.com", 443, 99), Allow);
        assert_eq!(p.check("example.com", 443, 100), Deny(DenyReason::Expired));
        assert_eq!(p.check("example.com", 443, u64::MAX), Deny(DenyReason::Expired));
    }

    #[test]
    fn regrant_updates_expiry_without_duplicating() {
        let mut p = NetPolicy::new();
        p.grant("example.com", 443, Some(10)).unwrap();
        p.grant("example.com", 443, Some(1000)).unwrap();
        assert_eq!(p.grant_count(), 1);
        assert_eq!(p.check("example.com", 443, 500), Allow);
        p.grant("example.com", 443, None).unwrap();
        assert_eq!(p.check("example.com", 443, u64::MAX), Allow);
    }

    #[test]
    fn malformed_destinations_are_rejected_and_denied() {
        let mut p = NetPolicy::new();
        let long_label = "a".repeat(64);
        let long_host = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.".repeat(5) + "com"; // 303 chars > 253
        let bad = [
            "", " ", "exa mple.com", "*.example.com", "example.com.", ".example.com", "a..b",
            "-a.com", "a-.com", "a.-b.com", "ex_ample.com", "example.com/path", "user@example.com",
            "example.com:443", "exämple.com", "a\0b", "a\nb", &long_label, &long_host,
        ];
        for h in bad {
            assert_eq!(p.grant(h, 443, None), Err(PolicyError::InvalidHost), "{h:?}");
            assert_eq!(p.check(h, 443, 0), Deny(DenyReason::InvalidDestination), "{h:?}");
        }
        assert_eq!(p.grant("example.com", 0, None), Err(PolicyError::InvalidPort));
        assert_eq!(p.check("example.com", 0, 0), Deny(DenyReason::InvalidDestination));
        assert_eq!(p.grant_count(), 0);
    }

    #[test]
    fn ip_literals_are_matched_exactly() {
        let mut p = NetPolicy::new();
        p.grant("93.184.216.34", 80, None).unwrap();
        assert_eq!(p.check("93.184.216.34", 80, 0), Allow);
        assert_eq!(p.check("93.184.216.35", 80, 0), Deny(DenyReason::NoGrant));
    }

    #[test]
    fn table_full_fails_closed() {
        let mut p = NetPolicy::new();
        for i in 0..MAX_GRANTS {
            p.grant("host.example", 1000 + i as u16, None).unwrap();
        }
        assert_eq!(p.grant("other.example", 1, None), Err(PolicyError::TableFull));
        assert_eq!(p.check("other.example", 1, 0), Deny(DenyReason::NoGrant));
        // Freeing a slot makes room again.
        assert!(p.revoke("host.example", 1000));
        p.grant("other.example", 1, None).unwrap();
    }

    #[test]
    fn grants_lists_normalized_entries() {
        let mut p = NetPolicy::new();
        p.grant("Example.COM", 443, Some(9)).unwrap();
        p.grant("10.0.2.2", 80, None).unwrap();
        let mut v: std::vec::Vec<_> = p.grants().collect();
        v.sort();
        assert_eq!(v, std::vec![("10.0.2.2", 80, None), ("example.com", 443, Some(9))]);
    }

    #[test]
    fn invalid_revoke_does_not_remove_anything() {
        let mut p = NetPolicy::new();
        p.grant("example.com", 443, None).unwrap();
        assert!(!p.revoke("*.com", 443));
        assert!(!p.revoke("", 443));
        assert_eq!(p.grant_count(), 1);
    }
}
