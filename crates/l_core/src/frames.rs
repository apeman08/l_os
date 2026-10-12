//! Pure physical-frame cursor logic (no hardware access, easy to test).
//!
//! Walks a list of memory regions in order and hands out each 4 KiB frame of
//! every usable region exactly once. Frames are never freed (bump strategy).

pub const FRAME_SIZE: u64 = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Region {
    pub start: u64,
    pub end: u64,
    pub usable: bool,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Cursor {
    region: usize,
    addr: u64,
}

impl Cursor {
    pub const fn new() -> Self {
        Self { region: 0, addr: 0 }
    }

    /// Next unused frame address, skipping unusable regions and the zero frame.
    /// `region(i)` returns region `i`, or `None` past the end.
    pub fn next_frame(&mut self, region: impl Fn(usize) -> Option<Region>) -> Option<u64> {
        loop {
            let r = region(self.region)?;
            if !r.usable {
                self.region += 1;
                self.addr = 0;
                continue;
            }
            let aligned_start = r.start.checked_add(FRAME_SIZE - 1)? & !(FRAME_SIZE - 1);
            // Never hand out physical frame 0 (null-pointer safety net).
            let first = aligned_start.max(FRAME_SIZE);
            let candidate = self.addr.max(first);
            match candidate.checked_add(FRAME_SIZE) {
                Some(end) if end <= r.end => {
                    self.addr = end;
                    return Some(candidate);
                }
                _ => {
                    self.region += 1;
                    self.addr = 0;
                }
            }
        }
    }
}

#[test]
fn cursor_skips_unusable_and_exhausts() {
    let regions = [
        Region { start: 0x0, end: 0x3000, usable: true },      // frame 0 skipped -> 0x1000, 0x2000
        Region { start: 0x3000, end: 0x8000, usable: false },
        Region { start: 0x8800, end: 0xB000, usable: true },   // unaligned start -> 0x9000, 0xA000
        Region { start: 0xC000, end: 0xC800, usable: true },   // too small for a frame
    ];
    let mut c = Cursor::new();
    let get = |i: usize| regions.get(i).copied();
    let mut got = [0u64; 8];
    let mut n = 0;
    while let Some(f) = c.next_frame(get) {
        got[n] = f;
        n += 1;
    }
    assert_eq!(&got[..n], &[0x1000, 0x2000, 0x9000, 0xA000]);
    assert!(c.next_frame(get).is_none()); // stays exhausted
}

#[test]
fn cursor_never_returns_duplicates() {
    let regions = [Region { start: 0x1000, end: 0x1_0000, usable: true }];
    let mut c = Cursor::new();
    let mut last = 0;
    let mut count = 0;
    while let Some(f) = c.next_frame(|i| regions.get(i).copied()) {
        assert!(f > last);
        last = f;
        count += 1;
    }
    assert_eq!(count, 15);
}
