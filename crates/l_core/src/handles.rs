//! Per-process capability handles.
//!
//! A process can only act through handles in its own table. A handle names a
//! kernel object plus the rights this holder has on it. Handles are unforgeable
//! by construction: user code passes an integer, the kernel looks it up in the
//! caller's table and rejects anything that is not a live entry.
//!
//! Rights can only shrink: `derive` intersects with a mask, and an entry can
//! only be passed on if it carries `GRANT`.

pub const MAX_HANDLES: usize = 16;
/// Sentinel for "no handle" in syscall arguments.
pub const NO_HANDLE: u64 = u64::MAX;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Object {
    Console,
    /// The right to *ask* the network policy; each destination still needs a grant.
    NetClient,
    Endpoint(u16),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rights(u8);

impl Rights {
    pub const NONE: Rights = Rights(0);
    pub const WRITE: Rights = Rights(1 << 0);
    pub const USE: Rights = Rights(1 << 1);
    pub const SEND: Rights = Rights(1 << 2);
    pub const RECV: Rights = Rights(1 << 3);
    /// May be passed on to another process (always attenuated by a mask).
    pub const GRANT: Rights = Rights(1 << 4);
    const ALL: u8 = 0b1_1111;

    pub const fn union(self, other: Rights) -> Rights {
        Rights(self.0 | other.0)
    }
    pub const fn intersect(self, other: Rights) -> Rights {
        Rights(self.0 & other.0)
    }
    pub const fn contains(self, needed: Rights) -> bool {
        self.0 & needed.0 == needed.0
    }
    pub const fn bits(self) -> u8 {
        self.0
    }
    /// Unknown bits are dropped.
    pub const fn from_bits(bits: u64) -> Rights {
        Rights((bits & Self::ALL as u64) as u8)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HandleError {
    /// Not a live handle in this table (out of range, closed, or stale).
    Invalid,
    InsufficientRights,
    TableFull,
}

#[derive(Debug, Clone, Copy)]
struct Entry {
    object: Object,
    rights: Rights,
}

pub struct HandleTable {
    slots: [Option<Entry>; MAX_HANDLES],
    /// Bumped on every close, so a closed handle value never matches a later
    /// object that reuses the slot.
    generation: [u32; MAX_HANDLES],
}

impl HandleTable {
    pub const fn new() -> Self {
        Self { slots: [None; MAX_HANDLES], generation: [0; MAX_HANDLES] }
    }

    fn encode(&self, index: usize) -> u64 {
        (((self.generation[index] & 0x00ff_ffff) as u64) << 8) | index as u64
    }

    /// Resolve a user-supplied value to a slot index, if it names a live entry.
    fn resolve(&self, handle: u64) -> Result<usize, HandleError> {
        if handle > u32::MAX as u64 {
            return Err(HandleError::Invalid);
        }
        let index = (handle & 0xff) as usize;
        if index >= MAX_HANDLES || self.slots[index].is_none() || self.encode(index) != handle {
            return Err(HandleError::Invalid);
        }
        Ok(index)
    }

    pub fn insert(&mut self, object: Object, rights: Rights) -> Result<u64, HandleError> {
        let index = self.slots.iter().position(Option::is_none).ok_or(HandleError::TableFull)?;
        self.slots[index] = Some(Entry { object, rights });
        Ok(self.encode(index))
    }

    /// Look up `handle` and require at least `needed` rights.
    pub fn lookup(&self, handle: u64, needed: Rights) -> Result<(Object, Rights), HandleError> {
        let e = self.slots[self.resolve(handle)?].expect("resolve returned a live slot");
        if !e.rights.contains(needed) {
            return Err(HandleError::InsufficientRights);
        }
        Ok((e.object, e.rights))
    }

    pub fn close(&mut self, handle: u64) -> Result<(), HandleError> {
        let index = self.resolve(handle)?;
        self.slots[index] = None;
        self.generation[index] = self.generation[index].wrapping_add(1);
        Ok(())
    }

    /// What may be handed to another process: the entry must carry `GRANT`;
    /// the result has `entry.rights ∩ mask` (so rights never grow, and `GRANT`
    /// survives only if the mask asks for it).
    pub fn derive(&self, handle: u64, mask: Rights) -> Result<(Object, Rights), HandleError> {
        let (object, rights) = self.lookup(handle, Rights::GRANT)?;
        Ok((object, rights.intersect(mask)))
    }

    pub fn count(&self) -> usize {
        self.slots.iter().flatten().count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const W: Rights = Rights::WRITE;

    #[test]
    fn rights_algebra() {
        assert!(W.union(Rights::GRANT).contains(W));
        assert!(!W.contains(Rights::GRANT));
        assert!(W.contains(Rights::NONE));
        assert_eq!(W.intersect(Rights::GRANT), Rights::NONE);
        assert_eq!(Rights::from_bits(0xffff_ffff).bits(), 0b1_1111);
    }

    #[test]
    fn insert_and_lookup() {
        let mut t = HandleTable::new();
        let h = t.insert(Object::Console, W).unwrap();
        assert_eq!(t.lookup(h, W), Ok((Object::Console, W)));
        assert_eq!(t.lookup(h, Rights::SEND), Err(HandleError::InsufficientRights));
    }

    #[test]
    fn forged_values_are_rejected() {
        let mut t = HandleTable::new();
        let h = t.insert(Object::Console, W).unwrap();
        for forged in [1, 15, 16, 0xff, 0x100, u64::from(u32::MAX), u64::MAX, NO_HANDLE, h + 0x100, h | 0x8000_0000] {
            if forged != h {
                assert_eq!(t.lookup(forged, Rights::NONE), Err(HandleError::Invalid), "{forged:#x}");
            }
        }
    }

    #[test]
    fn closed_handle_is_dead_and_reuse_does_not_resurrect_it() {
        let mut t = HandleTable::new();
        let old = t.insert(Object::Console, W).unwrap();
        t.close(old).unwrap();
        assert_eq!(t.lookup(old, Rights::NONE), Err(HandleError::Invalid));
        assert_eq!(t.close(old), Err(HandleError::Invalid)); // double close
        let new = t.insert(Object::NetClient, Rights::USE).unwrap(); // reuses slot 0
        assert_ne!(old, new);
        assert_eq!(t.lookup(old, Rights::NONE), Err(HandleError::Invalid)); // stale value
        assert_eq!(t.lookup(new, Rights::USE), Ok((Object::NetClient, Rights::USE)));
    }

    #[test]
    fn derive_needs_grant_and_only_attenuates() {
        let mut t = HandleTable::new();
        let plain = t.insert(Object::Console, W).unwrap();
        assert_eq!(t.derive(plain, W), Err(HandleError::InsufficientRights)); // no GRANT

        let rich = t.insert(Object::Console, W.union(Rights::GRANT)).unwrap();
        // Asking for more than we have does not add rights.
        let (_, r) = t.derive(rich, Rights::from_bits(0xff)).unwrap();
        assert_eq!(r, W.union(Rights::GRANT));
        // Narrow mask narrows.
        let (_, r) = t.derive(rich, W).unwrap();
        assert_eq!(r, W);
        assert!(!r.contains(Rights::GRANT)); // cannot be passed on again
        let (_, r) = t.derive(rich, Rights::SEND).unwrap();
        assert_eq!(r, Rights::NONE);
    }

    #[test]
    fn table_full() {
        let mut t = HandleTable::new();
        for _ in 0..MAX_HANDLES {
            t.insert(Object::Console, W).unwrap();
        }
        assert_eq!(t.insert(Object::Console, W), Err(HandleError::TableFull));
        assert_eq!(t.count(), MAX_HANDLES);
    }

    #[test]
    fn endpoint_identity_is_preserved() {
        let mut t = HandleTable::new();
        let h = t.insert(Object::Endpoint(3), Rights::SEND).unwrap();
        assert_eq!(t.lookup(h, Rights::SEND).unwrap().0, Object::Endpoint(3));
    }
}
