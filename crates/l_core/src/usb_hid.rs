//! USB HID boot-protocol keyboard reports to PS/2 set-1 scancodes.
//!
//! The USB keyboard path (M2 Task 22) feeds the emitted bytes into the same
//! queue and `HotkeyDetector` as the PS/2 ISR, so everything downstream sees
//! one scancode stream. Sources: USB "HID Usage Tables" 1.12, chapter 10
//! (Keyboard/Keypad page 0x07) and the IBM PS/2 scancode set 1 (with `E0`
//! extended prefixes as produced by an AT/PS/2 keyboard through the 8042
//! translation). Positions are physical: Usage 0x2F is the key right of `P`
//! (set 1 `1A`), 0x33 the key right of `L` (`27`), 0x64 the extra key left of
//! `Y`/`Z` on ISO boards (`56`); the layout (German etc.) is applied later.
//!
//! Reports come from untrusted hardware: every 8-byte value is handled
//! without panic.

/// An 8-byte boot keyboard report: modifier bits, reserved, six key usages.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BootReport(pub [u8; 8]);

impl BootReport {
    const fn keys(&self) -> [u8; 6] {
        [self.0[2], self.0[3], self.0[4], self.0[5], self.0[6], self.0[7]]
    }

    /// Phantom-state report (`ErrorRollOver`): usage 0x01 in all six slots.
    fn is_rollover_error(&self) -> bool {
        self.keys().iter().all(|&k| k == 0x01)
    }
}

/// Usage 0x04..=0x1D (`a`..`z`).
const LETTERS: [u8; 26] = [
    0x1E, 0x30, 0x2E, 0x20, 0x12, 0x21, 0x22, 0x23, 0x17, 0x24, 0x25, 0x26, 0x32, 0x31, 0x18,
    0x19, 0x10, 0x13, 0x1F, 0x14, 0x16, 0x2F, 0x11, 0x2D, 0x15, 0x2C,
];
/// Usage 0x59..=0x63 (keypad 1-9, 0, `.`), all without `E0`.
const KEYPAD_NUM: [u8; 11] = [0x4F, 0x50, 0x51, 0x4B, 0x4C, 0x4D, 0x47, 0x48, 0x49, 0x52, 0x53];

/// Map a key usage to `(e0_prefix, make_code)`. `None` for usages with no
/// set-1 key (no event, error codes, Pause which needs an `E1` sequence,
/// reserved and unassigned values).
pub fn usage_to_set1(usage: u8) -> Option<(bool, u8)> {
    let plain = |c: u8| Some((false, c));
    let ext = |c: u8| Some((true, c));
    match usage {
        0x04..=0x1D => plain(LETTERS[(usage - 0x04) as usize]),
        0x1E..=0x27 => plain(0x02 + (usage - 0x1E)), // 1..9, 0
        0x28 => plain(0x1C), // Enter
        0x29 => plain(0x01), // Escape
        0x2A => plain(0x0E), // Backspace
        0x2B => plain(0x0F), // Tab
        0x2C => plain(0x39), // Space
        0x2D => plain(0x0C),
        0x2E => plain(0x0D),
        0x2F => plain(0x1A),
        0x30 => plain(0x1B),
        0x31 => plain(0x2B),
        0x32 => plain(0x2B), // non-US '#': same position as '\' on set 1
        0x33 => plain(0x27),
        0x34 => plain(0x28),
        0x35 => plain(0x29),
        0x36 => plain(0x33),
        0x37 => plain(0x34),
        0x38 => plain(0x35),
        0x39 => plain(0x3A),                         // Caps Lock
        0x3A..=0x43 => plain(0x3B + (usage - 0x3A)), // F1..F10
        0x44 => plain(0x57),                         // F11
        0x45 => plain(0x58),                         // F12
        0x46 => ext(0x37),                           // Print Screen (make is E0 2A E0 37; E0 37 suffices)
        0x47 => plain(0x46),                         // Scroll Lock
        0x49 => ext(0x52),                           // Insert
        0x4A => ext(0x47),                           // Home
        0x4B => ext(0x49),                           // Page Up
        0x4C => ext(0x53),                           // Delete
        0x4D => ext(0x4F),                           // End
        0x4E => ext(0x51),                           // Page Down
        0x4F => ext(0x4D),                           // Right
        0x50 => ext(0x4B),                           // Left
        0x51 => ext(0x50),                           // Down
        0x52 => ext(0x48),                           // Up
        0x53 => plain(0x45),                         // Num Lock
        0x54 => ext(0x35),                           // keypad /
        0x55 => plain(0x37),                         // keypad *
        0x56 => plain(0x4A),                         // keypad -
        0x57 => plain(0x4E),                         // keypad +
        0x58 => ext(0x1C),                           // keypad Enter
        0x59..=0x63 => plain(KEYPAD_NUM[(usage - 0x59) as usize]),
        0x64 => plain(0x56), // non-US '\' / '<' (ISO key)
        0x65 => ext(0x5D),   // Application (menu)
        _ => None,
    }
}

/// Map modifier bit 0..=7 (LCtrl, LShift, LAlt, LGUI, RCtrl, RShift, RAlt,
/// RGUI) to `(e0_prefix, make_code)`. Bits above 7 wrap (`bit & 7`).
pub fn modifier_to_set1(bit: u8) -> (bool, u8) {
    match bit & 7 {
        0 => (false, 0x1D),
        1 => (false, 0x2A),
        2 => (false, 0x38),
        3 => (true, 0x5B),
        4 => (true, 0x1D),
        5 => (false, 0x36),
        6 => (true, 0x38),
        _ => (true, 0x5C),
    }
}

fn emit(push: &mut impl FnMut(u8), e0: bool, make: u8, release: bool) {
    if e0 {
        push(0xE0);
    }
    push(if release { make | 0x80 } else { make });
}

/// First occurrence of a non-zero usage in a key array (duplicates are one key).
fn is_first(keys: &[u8; 6], i: usize) -> bool {
    keys[i] != 0 && !keys[..i].contains(&keys[i])
}

/// Emit the set-1 bytes for the change `prev` -> `now`. Order: modifier
/// presses, key releases, key presses, modifier releases (so modifiers are
/// down around every key they affect). Keys held across reports do not
/// repeat (the host's typematic is not emulated). A rollover-error report is
/// ignored entirely; the caller keeps its last good report as `prev`. Usages
/// with no set-1 mapping are dropped.
pub fn report_to_set1(prev: &BootReport, now: &BootReport, mut push: impl FnMut(u8)) {
    if now.is_rollover_error() {
        return;
    }
    let (pm, nm) = (prev.0[0], now.0[0]);
    let pk = if prev.is_rollover_error() { [0; 6] } else { prev.keys() };
    let nk = now.keys();
    for bit in 0..8u8 {
        if nm & (1 << bit) != 0 && pm & (1 << bit) == 0 {
            let (e0, make) = modifier_to_set1(bit);
            emit(&mut push, e0, make, false);
        }
    }
    for i in 0..6 {
        if is_first(&pk, i) && !nk.contains(&pk[i]) {
            if let Some((e0, make)) = usage_to_set1(pk[i]) {
                emit(&mut push, e0, make, true);
            }
        }
    }
    for i in 0..6 {
        if is_first(&nk, i) && !pk.contains(&nk[i]) {
            if let Some((e0, make)) = usage_to_set1(nk[i]) {
                emit(&mut push, e0, make, false);
            }
        }
    }
    for bit in (0..8u8).rev() {
        if pm & (1 << bit) != 0 && nm & (1 << bit) == 0 {
            let (e0, make) = modifier_to_set1(bit);
            emit(&mut push, e0, make, true);
        }
    }
}

/// The decoder state of one boot keyboard: owns `prev`, the last *good*
/// report (ledger Task 15), so a caller cannot diff against a rejected one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeyboardState {
    prev: BootReport,
}

impl KeyboardState {
    /// Upper bound of the bytes [`KeyboardState::on_report`] emits for one
    /// report: 8 modifier and 6 + 6 key changes, each at most `E0` + code.
    pub const MAX_BYTES_PER_REPORT: usize = (8 + 6 + 6) * 2;

    /// No key held.
    pub const fn new() -> KeyboardState {
        KeyboardState { prev: BootReport([0; 8]) }
    }

    /// The last good report.
    pub fn prev(&self) -> BootReport {
        self.prev
    }

    /// Handle one report as received from the device (any length, untrusted):
    /// exactly 8 bytes that are not a rollover error are diffed against the
    /// last good report ([`report_to_set1`], bytes to `push`) and become the
    /// new last good report; `true`. Anything else (wrong length, rollover
    /// error) emits nothing, keeps the state and returns `false`.
    pub fn on_report(&mut self, data: &[u8], push: impl FnMut(u8)) -> bool {
        let Ok(bytes) = <[u8; 8]>::try_from(data) else { return false };
        let now = BootReport(bytes);
        if now.is_rollover_error() {
            return false;
        }
        report_to_set1(&self.prev, &now, push);
        self.prev = now;
        true
    }
}

impl Default for KeyboardState {
    fn default() -> Self {
        KeyboardState::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hotkey::HotkeyDetector;

    const EMPTY: BootReport = BootReport([0; 8]);

    fn rep(mods: u8, keys: &[u8]) -> BootReport {
        let mut r = [0u8; 8];
        r[0] = mods;
        r[2..2 + keys.len()].copy_from_slice(keys);
        BootReport(r)
    }

    fn run(prev: &BootReport, now: &BootReport) -> Vec<u8> {
        let mut v = Vec::new();
        report_to_set1(prev, now, |b| v.push(b));
        v
    }

    fn feed(k: &mut KeyboardState, data: &[u8]) -> (bool, Vec<u8>) {
        let mut v = Vec::new();
        let ok = k.on_report(data, |b| v.push(b));
        (ok, v)
    }

    #[test]
    fn keyboard_state_diffs_against_last_good_report() {
        let mut k = KeyboardState::new();
        assert_eq!(k.prev(), EMPTY);
        assert_eq!(feed(&mut k, &rep(0, &[0x04]).0), (true, vec![0x1E]));
        assert_eq!(k.prev(), rep(0, &[0x04]));
        // Held key: no repeat.
        assert_eq!(feed(&mut k, &rep(0, &[0x04]).0), (true, vec![]));
        assert_eq!(feed(&mut k, &EMPTY.0), (true, vec![0x9E]));
        assert_eq!(k.prev(), EMPTY);
    }

    /// Ledger Task 15: a rollover-error report emits nothing and does not
    /// replace `prev`; the next good report is diffed against the last good one.
    #[test]
    fn keyboard_state_rollover_error_keeps_prev() {
        let mut k = KeyboardState::new();
        let a = rep(0x02, &[0x04]);
        assert_eq!(feed(&mut k, &a.0), (true, vec![0x2A, 0x1E]));
        let err = rep(0x02, &[1, 1, 1, 1, 1, 1]);
        assert_eq!(feed(&mut k, &err.0), (false, vec![]));
        assert_eq!(k.prev(), a, "prev is still the last good report");
        // `a` still held: nothing new; then releasing gives the releases.
        assert_eq!(feed(&mut k, &a.0), (true, vec![]));
        assert_eq!(feed(&mut k, &EMPTY.0), (true, vec![0x9E, 0xAA]));
    }

    /// Hostile lengths: anything but exactly 8 bytes is ignored, state kept.
    #[test]
    fn keyboard_state_ignores_wrong_lengths() {
        let mut k = KeyboardState::new();
        assert!(feed(&mut k, &rep(0, &[0x04]).0).0);
        for n in [0usize, 1, 2, 3, 7, 9, 64] {
            let data = vec![0x05u8; n];
            assert_eq!(feed(&mut k, &data), (false, vec![]), "len {n}");
            assert_eq!(k.prev(), rep(0, &[0x04]));
        }
    }

    #[test]
    fn keyboard_state_hotkey_bytes_reach_detector() {
        let mut k = KeyboardState::new();
        let mut d = HotkeyDetector::new();
        let mut fired = false;
        for r in [rep(0x01, &[]), rep(0x05, &[]), rep(0x05, &[0x2A])] {
            k.on_report(&r.0, |b| fired |= d.feed(b));
        }
        assert!(fired);
    }

    #[test]
    fn keyboard_state_max_bytes_bound() {
        // Worst case for one report: every modifier and six keys change.
        let mut k = KeyboardState::new();
        let full = BootReport([0xFF, 0, 0x46, 0x49, 0x4A, 0x4B, 0x4C, 0x4D]);
        let (_, a) = feed(&mut k, &full.0);
        let (_, b) = feed(&mut k, &BootReport([0, 0, 0x4E, 0x4F, 0x50, 0x51, 0x52, 0x54]).0);
        assert!(a.len() <= KeyboardState::MAX_BYTES_PER_REPORT && b.len() <= KeyboardState::MAX_BYTES_PER_REPORT);
        // 8 modifier releases (4 plain, 4 with E0), 6 + 6 E0 keys.
        assert_eq!(b.len(), (4 + 8) + 12 + 12);
        assert_eq!(KeyboardState::MAX_BYTES_PER_REPORT, 40);
    }

    #[test]
    fn a_press_release_gives_1e_9e() {
        let down = rep(0, &[0x04]);
        assert_eq!(run(&EMPTY, &down), [0x1E]);
        assert_eq!(run(&down, &EMPTY), [0x9E]);
    }

    #[test]
    fn left_ctrl_gives_1d() {
        let r = rep(0x01, &[]);
        assert_eq!(run(&EMPTY, &r), [0x1D]);
        assert_eq!(run(&r, &EMPTY), [0x9D]);
    }

    #[test]
    fn right_ctrl_gives_e0_1d() {
        let r = rep(0x10, &[]);
        assert_eq!(run(&EMPTY, &r), [0xE0, 0x1D]);
        assert_eq!(run(&r, &EMPTY), [0xE0, 0x9D]);
    }

    #[test]
    fn arrow_up_gives_e0_48() {
        let r = rep(0, &[0x52]);
        assert_eq!(run(&EMPTY, &r), [0xE0, 0x48]);
        assert_eq!(run(&r, &EMPTY), [0xE0, 0xC8]);
    }

    #[test]
    fn enter_and_backspace() {
        assert_eq!(run(&EMPTY, &rep(0, &[0x28])), [0x1C]);
        assert_eq!(run(&EMPTY, &rep(0, &[0x2A])), [0x0E]);
    }

    #[test]
    fn german_positions_by_usage() {
        assert_eq!(usage_to_set1(0x2F), Some((false, 0x1A)));
        assert_eq!(usage_to_set1(0x33), Some((false, 0x27)));
        assert_eq!(usage_to_set1(0x64), Some((false, 0x56)));
    }

    #[test]
    fn rollover_error_report_is_ignored() {
        let err = rep(0x02, &[1, 1, 1, 1, 1, 1]);
        assert!(run(&EMPTY, &err).is_empty());
        let held = rep(0, &[0x04]);
        assert!(run(&held, &err).is_empty());
        // After the error, the next good report is diffed against the last good one.
        assert!(run(&held, &held).is_empty());
        // An error as prev means no keys known held; no spurious releases.
        assert_eq!(run(&rep(0, &[1, 1, 1, 1, 1, 1]), &held), [0x1E]);
    }

    #[test]
    fn unknown_usage_dropped() {
        for u in [0x01u8, 0x02, 0x03, 0x48, 0x66, 0xA0, 0xE0, 0xFF] {
            assert_eq!(usage_to_set1(u), None, "usage {u:#x}");
            let r = rep(0, &[u]);
            assert!(run(&EMPTY, &r).is_empty());
            assert!(run(&r, &EMPTY).is_empty());
        }
    }

    #[test]
    fn ctrl_alt_backspace_via_hid_fires_hotkey_detector() {
        let mut d = HotkeyDetector::new();
        let mut fired = false;
        let mut stream = Vec::new();
        // Modifiers and Backspace arrive in one report.
        report_to_set1(&EMPTY, &rep(0x01 | 0x04, &[0x2A]), |b| stream.push(b));
        for b in stream {
            fired |= d.feed(b);
        }
        assert!(fired);
        // Right Ctrl + AltGr (E0-prefixed) too, in separate reports.
        let mut d = HotkeyDetector::new();
        let mut fired = false;
        let r1 = rep(0x10 | 0x40, &[]);
        let r2 = rep(0x10 | 0x40, &[0x2A]);
        let mut stream = Vec::new();
        report_to_set1(&EMPTY, &r1, |b| stream.push(b));
        report_to_set1(&r1, &r2, |b| stream.push(b));
        for b in stream {
            fired |= d.feed(b);
        }
        assert!(fired);
    }

    #[test]
    fn held_key_does_not_repeat_and_rollover_of_keys() {
        let a = rep(0, &[0x04]);
        let ab = rep(0, &[0x04, 0x05]);
        assert!(run(&a, &a).is_empty());
        assert_eq!(run(&a, &ab), [0x30]);
        assert_eq!(run(&ab, &a), [0xB0]);
        // Swap: release first, then press.
        assert_eq!(run(&a, &rep(0, &[0x05])), [0x9E, 0x30]);
    }

    #[test]
    fn modifier_order_and_transitions() {
        let shift_a = rep(0x02, &[0x04]);
        assert_eq!(run(&EMPTY, &shift_a), [0x2A, 0x1E]);
        assert_eq!(run(&shift_a, &EMPTY), [0x9E, 0xAA]);
        // Modifier only changes: GUI and RAlt.
        assert_eq!(run(&EMPTY, &rep(0x08 | 0x40, &[])), [0xE0, 0x5B, 0xE0, 0x38]);
        assert_eq!(run(&rep(0x01, &[]), &rep(0x04, &[])), [0x38, 0x9D]);
    }

    #[test]
    fn duplicate_usage_in_one_report_is_one_key() {
        assert_eq!(run(&EMPTY, &rep(0, &[0x04, 0x04])), [0x1E]);
        assert_eq!(run(&rep(0, &[0x04, 0x04]), &EMPTY), [0x9E]);
    }

    #[test]
    fn table_is_complete_and_unique() {
        let mut seen = std::collections::HashMap::new();
        for u in 0..=255u8 {
            if let Some((e0, m)) = usage_to_set1(u) {
                assert!(m != 0 && m < 0x80, "usage {u:#x} make {m:#x}");
                if let Some(prev) = seen.insert((e0, m), u) {
                    // Only '#'(0x32) and '\'(0x31) share a position.
                    assert_eq!((prev, u), (0x31, 0x32));
                }
            }
        }
        for b in 0..8 {
            let (e0, m) = modifier_to_set1(b);
            assert!(!seen.contains_key(&(e0, m)) || (e0, m) == (false, 0x00));
        }
        // Spot checks: every letter/digit/F-key mapped.
        for u in (0x04..=0x45u8).chain(0x47..=0x47).chain(0x49..=0x65) {
            assert!(usage_to_set1(u).is_some(), "usage {u:#x}");
        }
        assert_eq!(usage_to_set1(0x44), Some((false, 0x57)));
        assert_eq!(usage_to_set1(0x45), Some((false, 0x58)));
        assert_eq!(usage_to_set1(0x58), Some((true, 0x1C)));
    }

    #[test]
    fn fuzz_any_report_never_panics_and_stream_is_well_formed() {
        let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
        let mut next = || {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x
        };
        let mut prev = EMPTY;
        let mut down = std::collections::HashSet::new();
        for i in 0..20_000 {
            let mut r = [0u8; 8];
            for b in r.iter_mut() {
                *b = next() as u8;
            }
            // Bias toward plausible usages so that presses/releases pair up.
            if i % 2 == 0 {
                for b in &mut r[2..] {
                    *b %= 0x70;
                }
            }
            let now = BootReport(r);
            let mut out = Vec::new();
            report_to_set1(&prev, &now, |b| out.push(b));
            let mut k = 0;
            while k < out.len() {
                let e0 = out[k] == 0xE0;
                assert_ne!(out[k], 0xE1);
                if e0 {
                    k += 1;
                }
                assert!(k < out.len() && out[k] != 0xE0 && out[k] != 0xE1);
                let key = (e0, out[k] & 0x7F);
                if out[k] & 0x80 == 0 {
                    down.insert(key);
                } else {
                    down.remove(&key);
                }
                k += 1;
            }
            if !now.is_rollover_error() {
                prev = now;
            }
        }
        // Releasing everything leaves no key stuck down (modulo shared positions).
        let mut out = Vec::new();
        report_to_set1(&prev, &EMPTY, |b| out.push(b));
        let mut k = 0;
        while k < out.len() {
            let e0 = out[k] == 0xE0;
            if e0 {
                k += 1;
            }
            down.remove(&(e0, out[k] & 0x7F));
            k += 1;
        }
        assert!(down.is_empty(), "stuck: {down:?}");
    }
}
