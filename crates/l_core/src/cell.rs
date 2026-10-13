//! Character cell grid with dirty-row tracking (pure logic).

use alloc::vec::Vec;
use core::ops::Range;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rgb(pub u8, pub u8, pub u8);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cell {
    pub ch: char,
    pub fg: Rgb,
    pub bg: Rgb,
    pub bold: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub col: usize,
    pub row: usize,
    pub cols: usize,
    pub rows: usize,
}

/// Row-major cell grid. Tracks the min/max touched row since the last
/// `take_dirty` so a renderer only repaints what changed.
pub struct Grid {
    cols: usize,
    rows: usize,
    cells: Vec<Cell>,
    /// Half-open dirty row range, `lo >= hi` means clean.
    dirty_lo: usize,
    dirty_hi: usize,
}

impl Grid {
    pub fn new(cols: usize, rows: usize, fill: Cell) -> Grid {
        Grid {
            cols,
            rows,
            cells: alloc::vec![fill; cols * rows],
            dirty_lo: 0,
            dirty_hi: 0,
        }
    }

    pub fn cols(&self) -> usize {
        self.cols
    }

    pub fn rows(&self) -> usize {
        self.rows
    }

    pub fn get(&self, col: usize, row: usize) -> Option<&Cell> {
        if col < self.cols && row < self.rows {
            self.cells.get(row * self.cols + col)
        } else {
            None
        }
    }

    fn mark_dirty(&mut self, row: usize) {
        if self.dirty_lo >= self.dirty_hi {
            self.dirty_lo = row;
            self.dirty_hi = row + 1;
        } else {
            self.dirty_lo = self.dirty_lo.min(row);
            self.dirty_hi = self.dirty_hi.max(row + 1);
        }
    }

    /// Out-of-range coordinates are ignored.
    pub fn set(&mut self, col: usize, row: usize, cell: Cell) {
        if col < self.cols && row < self.rows {
            self.cells[row * self.cols + col] = cell;
            self.mark_dirty(row);
        }
    }

    /// Writes `s` from (`col`, `row`), clipping at the right edge. Returns
    /// the number of columns written.
    pub fn put_str(&mut self, col: usize, row: usize, s: &str, fg: Rgb, bg: Rgb) -> usize {
        let mut written = 0;
        for ch in s.chars() {
            if col.saturating_add(written) >= self.cols || row >= self.rows {
                break;
            }
            self.set(col + written, row, Cell { ch, fg, bg, bold: false });
            written += 1;
        }
        written
    }

    /// Shifts all rows up by `lines` and fills the vacated bottom rows.
    pub fn scroll_up(&mut self, lines: usize, fill: Cell) {
        if self.rows == 0 || lines == 0 {
            return;
        }
        let lines = lines.min(self.rows);
        self.cells.copy_within(lines * self.cols.., 0);
        let keep = (self.rows - lines) * self.cols;
        for c in &mut self.cells[keep..] {
            *c = fill;
        }
        self.dirty_lo = 0;
        self.dirty_hi = self.rows;
    }

    /// Like [`Grid::scroll_up`], but only within `rows` (clamped to the
    /// grid): those rows move up by `lines`, the vacated ones at the end of
    /// the range are filled, and only that range is marked dirty. Rows outside
    /// the range are untouched.
    pub fn scroll_rows_up(&mut self, rows: Range<usize>, lines: usize, fill: Cell) {
        let end = rows.end.min(self.rows);
        if rows.start >= end || lines == 0 {
            return;
        }
        let height = end - rows.start;
        let lines = lines.min(height);
        let (first, stop) = (rows.start * self.cols, end * self.cols);
        self.cells.copy_within(first + lines * self.cols..stop, first);
        for c in &mut self.cells[stop - lines * self.cols..stop] {
            *c = fill;
        }
        self.mark_dirty(rows.start);
        self.mark_dirty(end - 1);
    }

    /// Row range touched since the previous call, or `None` if clean.
    pub fn take_dirty(&mut self) -> Option<Range<usize>> {
        let r = (self.dirty_lo < self.dirty_hi).then(|| self.dirty_lo..self.dirty_hi);
        self.dirty_lo = 0;
        self.dirty_hi = 0;
        r
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::String;

    const BLANK: Cell = Cell { ch: ' ', fg: Rgb(1, 1, 1), bg: Rgb(0, 0, 0), bold: false };
    const FG: Rgb = Rgb(200, 200, 200);
    const BG: Rgb = Rgb(0, 0, 0);

    #[test]
    fn put_str_clips_at_right_edge() {
        let mut g = Grid::new(10, 2, BLANK);
        assert_eq!(g.put_str(7, 0, "abcdef", FG, BG), 3);
        assert_eq!(g.get(9, 0).unwrap().ch, 'c');
        assert_eq!(g.get(0, 1).unwrap().ch, ' ');
    }

    #[test]
    fn set_out_of_bounds_is_ignored() {
        let mut g = Grid::new(4, 4, BLANK);
        let x = Cell { ch: 'x', ..BLANK };
        g.set(4, 0, x);
        g.set(0, 4, x);
        g.set(usize::MAX, usize::MAX, x);
        assert_eq!(g.take_dirty(), None);
        assert!(g.get(4, 0).is_none());
    }

    #[test]
    fn scroll_up_moves_rows_and_fills_last() {
        let mut g = Grid::new(2, 3, BLANK);
        g.put_str(0, 1, "ab", FG, BG);
        g.put_str(0, 2, "cd", FG, BG);
        let fill = Cell { ch: '.', ..BLANK };
        g.scroll_up(1, fill);
        assert_eq!(g.get(0, 0).unwrap().ch, 'a');
        assert_eq!(g.get(1, 1).unwrap().ch, 'd');
        assert_eq!(g.get(0, 2).unwrap().ch, '.');
        g.scroll_up(100, fill);
        assert_eq!(g.get(0, 0).unwrap().ch, '.');
    }

    #[test]
    fn scroll_rows_up_moves_only_the_region() {
        let mut g = Grid::new(2, 5, BLANK);
        for (row, s) in ["aa", "bb", "cc", "dd", "ee"].iter().enumerate() {
            g.put_str(0, row, s, FG, BG);
        }
        let _ = g.take_dirty();
        let fill = Cell { ch: '.', ..BLANK };
        g.scroll_rows_up(1..4, 1, fill);
        let col0: String = (0..5).map(|r| g.get(0, r).unwrap().ch).collect();
        assert_eq!(col0, "acd.e");
        // Only the region is dirty, not the rows around it.
        assert_eq!(g.take_dirty(), Some(1..4));
        g.scroll_rows_up(1..4, 10, fill);
        let col1: String = (0..5).map(|r| g.get(1, r).unwrap().ch).collect();
        assert_eq!(col1, "a...e");
        // Empty, reversed or out-of-range regions change nothing and do not panic.
        let _ = g.take_dirty();
        g.scroll_rows_up(3..3, 1, fill);
        g.scroll_rows_up(5..9, 1, fill);
        g.scroll_rows_up(2..1, 1, fill);
        g.scroll_rows_up(1..4, 0, fill);
        assert_eq!(g.take_dirty(), None);
        g.scroll_rows_up(3..usize::MAX, 1, fill);
        assert_eq!(g.get(0, 3).unwrap().ch, 'e');
        assert_eq!(g.get(0, 4).unwrap().ch, '.');
    }

    #[test]
    fn dirty_range_covers_only_touched_rows() {
        let mut g = Grid::new(4, 8, BLANK);
        assert_eq!(g.take_dirty(), None);
        g.set(0, 3, Cell { ch: 'x', ..BLANK });
        g.set(1, 5, Cell { ch: 'y', ..BLANK });
        assert_eq!(g.take_dirty(), Some(3..6));
        assert_eq!(g.take_dirty(), None);
    }

    #[test]
    fn put_str_in_row_beyond_grid_returns_zero() {
        let mut g = Grid::new(4, 2, BLANK);
        assert_eq!(g.put_str(0, 2, "abc", FG, BG), 0);
        assert_eq!(g.put_str(9, 0, "abc", FG, BG), 0);
        assert_eq!(g.take_dirty(), None);
    }
}
