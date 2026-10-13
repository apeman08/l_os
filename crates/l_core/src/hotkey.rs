//! Emergency hotkey detection (Ctrl+Alt+Backspace) on the raw PS/2 set-1 stream.

/// Tracks modifier state from raw scancodes. State fits in one byte so an ISR
/// can keep it in an `AtomicU8` (`to_bits` / `from_bits`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HotkeyDetector {
    bits: u8,
}

const CTRL_L: u8 = 1 << 0;
const CTRL_R: u8 = 1 << 1;
const ALT_L: u8 = 1 << 2;
const ALT_R: u8 = 1 << 3;
const E0_PENDING: u8 = 1 << 4;
/// Bytes of an `E1` sequence (Pause) still to swallow, in bits 5..=6 (0..=2).
const E1_SHIFT: u8 = 5;
const E1_MASK: u8 = 0b11 << E1_SHIFT;

impl HotkeyDetector {
    pub const fn new() -> Self {
        Self { bits: 0 }
    }

    /// Feed one raw set-1 scancode byte. Returns `true` on a Backspace make
    /// code while a Ctrl and an Alt (left or right) are held. Auto-repeat
    /// fires again; the detector is never one-shot.
    ///
    /// A lost break code (e.g. a scancode dropped by a full queue upstream)
    /// leaves its modifier recorded as held until the next break of that key.
    #[must_use]
    pub fn feed(&mut self, scancode: u8) -> bool {
        let skip = (self.bits & E1_MASK) >> E1_SHIFT;
        if skip > 0 {
            // Inside an E1 sequence (Pause): ignore, so its 1D is not Ctrl.
            self.bits = (self.bits & !E1_MASK) | ((skip - 1) << E1_SHIFT);
            return false;
        }
        match scancode {
            0xE0 => {
                self.bits |= E0_PENDING;
                return false;
            }
            0xE1 => {
                self.bits = (self.bits & !E0_PENDING & !E1_MASK) | (2 << E1_SHIFT);
                return false;
            }
            _ => {}
        }
        let extended = self.bits & E0_PENDING != 0;
        self.bits &= !E0_PENDING;
        let release = scancode & 0x80 != 0;
        let (flag, key) = match (scancode & 0x7F, extended) {
            (0x1D, false) => (CTRL_L, true),
            (0x1D, true) => (CTRL_R, true),
            (0x38, false) => (ALT_L, true),
            (0x38, true) => (ALT_R, true),
            _ => (0, false),
        };
        if key {
            if release {
                self.bits &= !flag;
            } else {
                self.bits |= flag;
            }
            return false;
        }
        scancode == 0x0E
            && !extended
            && self.bits & (CTRL_L | CTRL_R) != 0
            && self.bits & (ALT_L | ALT_R) != 0
    }

    pub const fn to_bits(&self) -> u8 {
        self.bits
    }

    pub const fn from_bits(bits: u8) -> Self {
        // Mask off the unused top bit; E1 count 3 is invalid, clamp to 2.
        let mut bits = bits & 0x7F;
        if (bits & E1_MASK) >> E1_SHIFT == 3 {
            bits = (bits & !E1_MASK) | (2 << E1_SHIFT);
        }
        Self { bits }
    }
}

impl Default for HotkeyDetector {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed_all(d: &mut HotkeyDetector, codes: &[u8]) -> bool {
        let mut fired = false;
        for &c in codes {
            fired |= d.feed(c);
        }
        fired
    }

    #[test]
    fn ctrl_alt_backspace_fires() {
        let mut d = HotkeyDetector::new();
        assert!(!d.feed(0x1D));
        assert!(!d.feed(0x38));
        assert!(d.feed(0x0E));
    }

    #[test]
    fn right_ctrl_and_altgr_fire() {
        let mut d = HotkeyDetector::new();
        assert!(!feed_all(&mut d, &[0xE0, 0x1D, 0xE0, 0x38]));
        assert!(d.feed(0x0E));
    }

    #[test]
    fn backspace_alone_does_not_fire() {
        let mut d = HotkeyDetector::new();
        assert!(!d.feed(0x0E));
    }

    #[test]
    fn ctrl_backspace_without_alt_does_not_fire() {
        let mut d = HotkeyDetector::new();
        let _ = d.feed(0x1D);
        assert!(!d.feed(0x0E));
        let mut d = HotkeyDetector::new();
        let _ = d.feed(0x38);
        assert!(!d.feed(0x0E));
    }

    #[test]
    fn release_clears_modifiers() {
        for rel in [[0x9D_u8].as_slice(), &[0xE0, 0x9D]] {
            let mut d = HotkeyDetector::new();
            // press both the left and right variant, release the matching one
            let (press, alt): (&[u8], &[u8]) = if rel.len() == 1 {
                (&[0x1D], &[0x38])
            } else {
                (&[0xE0, 0x1D], &[0xE0, 0x38])
            };
            feed_all(&mut d, press);
            feed_all(&mut d, alt);
            assert!(d.feed(0x0E));
            feed_all(&mut d, rel);
            assert!(!d.feed(0x0E), "ctrl released");
        }
        for rel in [[0xB8_u8].as_slice(), &[0xE0, 0xB8]] {
            let mut d = HotkeyDetector::new();
            let (ctrl, alt): (&[u8], &[u8]) = if rel.len() == 1 {
                (&[0x1D], &[0x38])
            } else {
                (&[0xE0, 0x1D], &[0xE0, 0x38])
            };
            feed_all(&mut d, ctrl);
            feed_all(&mut d, alt);
            feed_all(&mut d, rel);
            assert!(!d.feed(0x0E), "alt released");
        }
    }

    #[test]
    fn left_and_right_are_tracked_independently() {
        let mut d = HotkeyDetector::new();
        feed_all(&mut d, &[0x1D, 0xE0, 0x1D, 0x38]);
        feed_all(&mut d, &[0x9D]); // release left ctrl only
        assert!(d.feed(0x0E)); // right ctrl still held
        feed_all(&mut d, &[0xE0, 0x9D]);
        assert!(!d.feed(0x0E));
    }

    #[test]
    fn e0_prefix_then_other_key_is_harmless() {
        let mut d = HotkeyDetector::new();
        feed_all(&mut d, &[0x1D, 0x38]);
        // E0 0E (not a real key) must not fire, and must not leave E0 pending
        assert!(!feed_all(&mut d, &[0xE0, 0x0E]));
        // E0 4B (left arrow) then plain Backspace still fires
        assert!(!feed_all(&mut d, &[0xE0, 0x4B]));
        assert!(d.feed(0x0E));
        // E0 prefix must not turn a plain 1D/38 into an extended one
        let mut d = HotkeyDetector::new();
        feed_all(&mut d, &[0xE0, 0x4B, 0x1D, 0x38]);
        assert!(d.feed(0x0E));
    }

    #[test]
    fn pause_sequence_does_not_fire() {
        let mut d = HotkeyDetector::new();
        let _ = d.feed(0x38); // Alt held
        assert!(!feed_all(&mut d, &[0xE1, 0x1D, 0x45, 0xE1, 0x9D, 0xC5]));
        // Pause's 1D must not have registered as Ctrl
        assert!(!d.feed(0x0E));
    }

    #[test]
    fn autorepeat_fires_again() {
        let mut d = HotkeyDetector::new();
        feed_all(&mut d, &[0x1D, 0x38]);
        assert!(d.feed(0x0E));
        assert!(d.feed(0x0E));
        assert!(d.feed(0x0E));
        // modifier auto-repeat is harmless too
        let _ = d.feed(0x1D);
        assert!(d.feed(0x0E));
    }

    #[test]
    fn backspace_break_does_not_fire() {
        let mut d = HotkeyDetector::new();
        feed_all(&mut d, &[0x1D, 0x38]);
        assert!(!d.feed(0x8E));
    }

    #[test]
    fn keeps_working_after_trigger_and_release_order() {
        let mut d = HotkeyDetector::new();
        assert!(feed_all(&mut d, &[0x38, 0x1D, 0x0E]));
        feed_all(&mut d, &[0x8E, 0xB8, 0x9D]); // release in any order
        assert!(!d.feed(0x0E));
        assert!(feed_all(&mut d, &[0x1D, 0x38, 0x0E]));
    }

    #[test]
    fn bits_roundtrip_preserves_state() {
        let mut d = HotkeyDetector::new();
        feed_all(&mut d, &[0xE0, 0x1D, 0x38]);
        let mut r = HotkeyDetector::from_bits(d.to_bits());
        assert_eq!(r, d);
        assert!(r.feed(0x0E));
        // mid-sequence state (pending E0 / E1) survives too
        let mut d = HotkeyDetector::new();
        let _ = d.feed(0x38);
        let _ = d.feed(0xE1);
        let mut r = HotkeyDetector::from_bits(d.to_bits());
        assert!(!feed_all(&mut r, &[0x1D, 0x45]));
        assert!(!r.feed(0x0E));
        assert_eq!(HotkeyDetector::new().to_bits(), 0);
    }
}
