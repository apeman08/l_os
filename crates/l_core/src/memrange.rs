//! Physical range arithmetic without heap (spec: amnesisch wipe planning).

pub const PAGE: u64 = 4096;

/// Fixed-capacity list is full.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Full;

/// Half-open physical range `[start, end)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PhysRange {
    pub start: u64,
    pub end: u64,
}

impl PhysRange {
    pub fn len(&self) -> u64 {
        self.end.saturating_sub(self.start)
    }
    pub fn is_empty(&self) -> bool {
        self.end <= self.start
    }
    pub fn page_aligned(&self) -> bool {
        self.start % PAGE == 0 && self.end % PAGE == 0
    }
}

/// Sorted, disjoint, non-adjacent list of ranges with fixed capacity.
#[derive(Clone, Copy, Debug)]
pub struct RangeList<const N: usize> {
    ranges: [PhysRange; N],
    len: usize,
}

impl<const N: usize> RangeList<N> {
    pub const fn new() -> Self {
        RangeList { ranges: [PhysRange { start: 0, end: 0 }; N], len: 0 }
    }
    pub fn push(&mut self, r: PhysRange) -> Result<(), Full> {
        if r.is_empty() {
            return Ok(());
        }
        // i: first entry that touches or follows r; j: first entry strictly after r.
        let mut i = 0;
        while i < self.len && self.ranges[i].end < r.start {
            i += 1;
        }
        let mut j = i;
        let mut merged = r;
        while j < self.len && self.ranges[j].start <= r.end {
            merged.start = merged.start.min(self.ranges[j].start);
            merged.end = merged.end.max(self.ranges[j].end);
            j += 1;
        }
        let new_len = self.len - (j - i) + 1;
        if new_len > N {
            return Err(Full);
        }
        let old = self.ranges;
        let old_len = self.len;
        self.ranges[i] = merged;
        for (k, item) in old[j..old_len].iter().enumerate() {
            self.ranges[i + 1 + k] = *item;
        }
        self.len = new_len;
        Ok(())
    }
    pub fn iter(&self) -> impl Iterator<Item = PhysRange> + '_ {
        self.ranges[..self.len].iter().copied()
    }
    pub fn len(&self) -> usize {
        self.len
    }
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
    pub fn total_bytes(&self) -> u64 {
        self.iter().fold(0u64, |a, r| a.saturating_add(r.len()))
    }
    pub fn subtract(&self, excl: &[PhysRange]) -> Result<RangeList<N>, Full> {
        let mut out = RangeList::<N>::new();
        for r in self.iter() {
            let mut cur = r.start;
            while cur < r.end {
                // Next exclusion overlapping [cur, r.end), earliest start first.
                let mut next: Option<PhysRange> = None;
                for e in excl.iter().filter(|e| !e.is_empty() && e.end > cur && e.start < r.end) {
                    if next.map_or(true, |n| e.start < n.start) {
                        next = Some(*e);
                    }
                }
                match next {
                    None => {
                        out.push(PhysRange { start: cur, end: r.end })?;
                        break;
                    }
                    Some(e) => {
                        if e.start > cur {
                            out.push(PhysRange { start: cur, end: e.start })?;
                        }
                        cur = e.end;
                    }
                }
            }
        }
        Ok(out)
    }
    pub fn contains(&self, addr: u64) -> bool {
        self.iter().any(|r| r.start <= addr && addr < r.end)
    }
}

impl<const N: usize> Default for RangeList<N> {
    fn default() -> Self {
        Self::new()
    }
}

/// 4-KiB page addresses -> maximal runs (any order, duplicates allowed).
/// Precondition: every address is 4-KiB aligned (debug-asserted). Returns
/// `Err(Full)` if the list overflows or a page's end would overflow u64.
pub fn coalesce_pages<const N: usize>(pages: impl Iterator<Item = u64>) -> Result<RangeList<N>, Full> {
    let mut l = RangeList::<N>::new();
    for p in pages {
        debug_assert!(p % PAGE == 0, "coalesce_pages: unaligned page address");
        // A page whose end overflows u64 must not be dropped silently (it would
        // stay unwiped); report it as an error. `Full` keeps the brief's
        // signature: the list cannot represent this page.
        let end = p.checked_add(PAGE).ok_or(Full)?;
        l.push(PhysRange { start: p, end })?;
    }
    Ok(l)
}

/// Calls `f` with the frame number of every set bit (LSB first per word).
pub fn for_each_set_bit(words: usize, word: impl Fn(usize) -> u64, mut f: impl FnMut(u64)) {
    for w in 0..words {
        let mut bits = word(w);
        while bits != 0 {
            f(w as u64 * 64 + bits.trailing_zeros() as u64);
            bits &= bits - 1;
        }
    }
}

/// Reports maximal runs of unallocated frames inside `region` (page-granular:
/// start rounded up, end rounded down). Frame number = physical address / 4096.
/// Frames at or above `tracked_frames` always count as unallocated.
/// Unaligned region edges are rounded inward (partial pages are NOT reported);
/// callers must handle any partial head/tail pages themselves.
pub fn unallocated_runs(
    region: PhysRange,
    tracked_frames: u64,
    is_allocated: impl Fn(u64) -> bool,
    mut f: impl FnMut(PhysRange),
) {
    let first = region.start / PAGE + u64::from(region.start % PAGE != 0);
    let last = region.end / PAGE;
    let mut run: Option<u64> = None;
    let mut frame = first;
    while frame < last {
        let used = frame < tracked_frames && is_allocated(frame);
        match (used, run) {
            (false, None) => run = Some(frame),
            (true, Some(s)) => {
                f(PhysRange { start: s * PAGE, end: frame * PAGE });
                run = None;
            }
            _ => {}
        }
        frame += 1;
    }
    if let Some(s) = run {
        f(PhysRange { start: s * PAGE, end: last * PAGE });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    extern crate std;
    use std::vec::Vec;

    fn r(s: u64, e: u64) -> PhysRange {
        PhysRange { start: s, end: e }
    }
    fn v<const N: usize>(l: &RangeList<N>) -> Vec<(u64, u64)> {
        l.iter().map(|x| (x.start, x.end)).collect()
    }

    #[test]
    fn range_basics() {
        assert_eq!(r(10, 20).len(), 10);
        assert_eq!(r(20, 10).len(), 0);
        assert!(r(5, 5).is_empty());
        assert!(r(0x1000, 0x3000).page_aligned());
        assert!(!r(0x1001, 0x3000).page_aligned());
        assert!(!r(0x1000, 0x2fff).page_aligned());
    }

    #[test]
    fn push_merges_adjacent_and_overlapping() {
        let mut l = RangeList::<4>::new();
        l.push(r(10, 20)).unwrap();
        l.push(r(20, 30)).unwrap(); // adjacent
        assert_eq!(v(&l), [(10, 30)]);
        l.push(r(25, 40)).unwrap(); // overlapping
        assert_eq!(v(&l), [(10, 40)]);
        l.push(r(50, 60)).unwrap();
        l.push(r(0, 100)).unwrap(); // swallows all
        assert_eq!(v(&l), [(0, 100)]);
        l.push(r(7, 7)).unwrap(); // empty ignored
        assert_eq!(l.len(), 1);
    }

    #[test]
    fn push_keeps_sorted() {
        let mut l = RangeList::<4>::new();
        l.push(r(50, 60)).unwrap();
        l.push(r(10, 20)).unwrap();
        l.push(r(30, 40)).unwrap();
        assert_eq!(v(&l), [(10, 20), (30, 40), (50, 60)]);
        l.push(r(20, 30)).unwrap(); // bridges two
        assert_eq!(v(&l), [(10, 40), (50, 60)]);
    }

    #[test]
    fn push_full_is_error_not_panic() {
        let mut l = RangeList::<2>::new();
        l.push(r(0, 1)).unwrap();
        l.push(r(10, 11)).unwrap();
        assert_eq!(l.push(r(20, 21)), Err(Full));
        assert_eq!(v(&l), [(0, 1), (10, 11)]); // unchanged
        l.push(r(1, 5)).unwrap(); // merge still fits when full
        assert_eq!(v(&l), [(0, 5), (10, 11)]);
    }

    #[test]
    fn subtract_splits_range() {
        let mut l = RangeList::<4>::new();
        l.push(r(0, 100)).unwrap();
        let d = l.subtract(&[r(40, 60)]).unwrap();
        assert_eq!(v(&d), [(0, 40), (60, 100)]);
        let d = l.subtract(&[r(0, 10), r(90, 200)]).unwrap();
        assert_eq!(v(&d), [(10, 90)]);
        assert_eq!(v(&l), [(0, 100)]); // original untouched
    }

    #[test]
    fn subtract_non_contiguous_island() {
        let mut l = RangeList::<8>::new();
        l.push(r(0, 100)).unwrap();
        l.push(r(200, 300)).unwrap();
        let d = l.subtract(&[r(50, 250), r(10, 20)]).unwrap();
        assert_eq!(v(&d), [(0, 10), (20, 50), (250, 300)]);
        let tiny = RangeList::<1>::new();
        let mut one = tiny;
        one.push(r(0, 100)).unwrap();
        assert_eq!(one.subtract(&[r(40, 60)]).err(), Some(Full));
    }

    #[test]
    fn subtract_exclusion_outside_is_noop() {
        let mut l = RangeList::<4>::new();
        l.push(r(100, 200)).unwrap();
        let d = l.subtract(&[r(0, 100), r(200, 300), r(5, 5)]).unwrap();
        assert_eq!(v(&d), [(100, 200)]);
        assert_eq!(v(&l.subtract(&[]).unwrap()), [(100, 200)]);
    }

    #[test]
    fn total_bytes_after_subtract() {
        let mut l = RangeList::<4>::new();
        l.push(r(0, 100)).unwrap();
        l.push(r(200, 300)).unwrap();
        assert_eq!(l.total_bytes(), 200);
        let d = l.subtract(&[r(50, 250)]).unwrap();
        assert_eq!(d.total_bytes(), 100);
        assert!(d.contains(0) && d.contains(49) && !d.contains(50));
        assert!(!d.contains(250 - 1) && d.contains(250) && !d.contains(300));
    }

    #[test]
    fn coalesce_pages_runs() {
        let pages = [0x3000u64, 0x1000, 0x2000, 0x8000, 0x9000, 0x1000];
        let l: RangeList<4> = coalesce_pages(pages.into_iter()).unwrap();
        assert_eq!(v(&l), [(0x1000, 0x4000), (0x8000, 0xa000)]);
        let many = [0u64, 0x2000, 0x4000];
        assert_eq!(coalesce_pages::<2>(many.into_iter()).err(), Some(Full));
        let end = [u64::MAX & !0xfff];
        // last page of the address space: end overflows -> loud error, not a drop
        assert_eq!(coalesce_pages::<2>(end.into_iter()).err(), Some(Full));
        let ok = [(u64::MAX & !0xfff) - PAGE];
        assert!(coalesce_pages::<2>(ok.into_iter()).is_ok());
    }

    #[test]
    fn near_u64_max_edges() {
        let top = u64::MAX;
        let mut l = RangeList::<2>::new();
        l.push(r(top - 10, top)).unwrap();
        l.push(r(top - 20, top - 10)).unwrap();
        assert_eq!(v(&l), [(top - 20, top)]);
        assert_eq!(l.total_bytes(), 20);
        assert!(l.contains(top - 1) && !l.contains(top));
        let d = l.subtract(&[r(top - 5, top)]).unwrap();
        assert_eq!(v(&d), [(top - 20, top - 5)]);
        assert!(l.subtract(&[r(0, top)]).unwrap().is_empty());
        let mut got = Vec::new();
        let hi = (top / PAGE) * PAGE;
        unallocated_runs(r(hi - 2 * PAGE, top), u64::MAX, |_| false, |x| got.push((x.start, x.end)));
        assert_eq!(got, [(hi - 2 * PAGE, hi)]);
    }

    #[test]
    fn subtract_exactly_n_pieces_ok_n_plus_one_err() {
        let mut l = RangeList::<3>::new();
        l.push(r(0, 100)).unwrap();
        // two holes -> 3 pieces == N
        let d = l.subtract(&[r(10, 20), r(50, 60)]).unwrap();
        assert_eq!(v(&d), [(0, 10), (20, 50), (60, 100)]);
        // three holes -> 4 pieces > N
        assert_eq!(l.subtract(&[r(10, 20), r(30, 40), r(50, 60)]).err(), Some(Full));
    }

    #[test]
    fn for_each_set_bit_reports_frame_numbers() {
        let words = [0b101u64, 0, 1u64 << 63];
        let mut got = Vec::new();
        for_each_set_bit(3, |i| words[i], |f| got.push(f));
        assert_eq!(got, [0, 2, 191]);
        for_each_set_bit(0, |_| 1, |_| panic!());
    }

    #[test]
    fn unallocated_runs_skips_allocated_and_merges() {
        // frames 0..8; allocated: 2,3 and 6
        let alloc = |f: u64| matches!(f, 2 | 3 | 6);
        let mut got = Vec::new();
        unallocated_runs(r(0, 8 * PAGE), 8, alloc, |x| got.push((x.start, x.end)));
        assert_eq!(got, [(0, 2 * PAGE), (4 * PAGE, 6 * PAGE), (7 * PAGE, 8 * PAGE)]);
    }

    #[test]
    fn unallocated_runs_above_tracked_limit_are_included() {
        // everything tracked is allocated; frames >= 4 are untracked => free
        let mut got = Vec::new();
        unallocated_runs(r(2 * PAGE, 10 * PAGE), 4, |_| true, |x| got.push((x.start, x.end)));
        assert_eq!(got, [(4 * PAGE, 10 * PAGE)]);
        // is_allocated must not be consulted at/above the limit
        let mut got2 = Vec::new();
        unallocated_runs(r(0, 6 * PAGE), 4, |f| { assert!(f < 4); f < 2 }, |x| got2.push((x.start, x.end)));
        assert_eq!(got2, [(2 * PAGE, 6 * PAGE)]);
    }
}
