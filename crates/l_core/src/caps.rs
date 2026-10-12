//! Per-process capability set. A process starts with none (`Caps::NONE`) and
//! can only gain one if the kernel grants it when the process is created.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cap {
    /// May write to the console.
    Console = 1 << 0,
    /// May ask to use the network at all. Each destination still needs a
    /// `NetPolicy` grant; this only gates the right to ask.
    Net = 1 << 1,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Caps(u32);

impl Caps {
    pub const NONE: Caps = Caps(0);

    pub const fn with(self, cap: Cap) -> Caps {
        Caps(self.0 | cap as u32)
    }

    pub const fn has(self, cap: Cap) -> bool {
        self.0 & cap as u32 != 0
    }

    pub const fn bits(self) -> u32 {
        self.0
    }

    /// Rebuild from stored bits; unknown bits are dropped (least authority).
    pub const fn from_bits(bits: u32) -> Caps {
        Caps(bits & (Cap::Console as u32 | Cap::Net as u32))
    }
}

impl Default for Caps {
    fn default() -> Self {
        Caps::NONE
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_has_no_capabilities() {
        let c = Caps::default();
        assert!(!c.has(Cap::Console));
        assert!(!c.has(Cap::Net));
    }

    #[test]
    fn capabilities_are_independent() {
        let c = Caps::NONE.with(Cap::Console);
        assert!(c.has(Cap::Console));
        assert!(!c.has(Cap::Net));
    }

    #[test]
    fn unknown_bits_are_dropped() {
        assert_eq!(Caps::from_bits(0xffff_fff0), Caps::NONE);
        assert_eq!(Caps::from_bits(0xffff_fff1), Caps::NONE.with(Cap::Console));
    }
}
