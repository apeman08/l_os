//! Reusable character-cell widgets: rounded box, hint bar, status bar.

use crate::cell::{Cell, Grid, Rect, Rgb};
use crate::theme::Theme;
use alloc::string::String;
use alloc::vec::Vec;

/// Maximum number of hints a `HintBar` may hold.
const MAX_HINTS: usize = 3;

/// Returned by `HintBar::new` when more than three hints are supplied.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub struct TooManyHints;

fn cell(ch: char, fg: Rgb, bg: Rgb) -> Cell {
    Cell { ch, fg, bg, bold: false }
}

/// Draws a box with rounded corners (`╭╮╰╯`, edges `─│`) and a filled
/// interior. Rectangles smaller than 2x2 are ignored; anything outside the
/// grid is clipped.
pub fn rounded_box(grid: &mut Grid, r: Rect, border: Rgb, fill: Rgb) {
    if r.cols < 2 || r.rows < 2 {
        return;
    }
    let last_col = r.col.saturating_add(r.cols - 1);
    let last_row = r.row.saturating_add(r.rows - 1);
    let row_end = last_row.saturating_add(1).min(grid.rows());
    let col_end = last_col.saturating_add(1).min(grid.cols());
    for row in r.row..row_end {
        for col in r.col..col_end {
            let top = row == r.row;
            let bottom = row == last_row;
            let left = col == r.col;
            let right = col == last_col;
            let ch = match (top, bottom, left, right) {
                (true, _, true, _) => '╭',
                (true, _, _, true) => '╮',
                (_, true, true, _) => '╰',
                (_, true, _, true) => '╯',
                (true, _, _, _) | (_, true, _, _) => '─',
                (_, _, true, _) | (_, _, _, true) => '│',
                _ => ' ',
            };
            let fg = if ch == ' ' { fill } else { border };
            grid.set(col, row, cell(ch, fg, fill));
        }
    }
}

/// Up to three `key description` hints, owned by the bar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HintBar {
    hints: Vec<(String, String)>,
}

impl HintBar {
    pub fn new(hints: &[(&str, &str)]) -> Result<HintBar, TooManyHints> {
        if hints.len() > MAX_HINTS {
            return Err(TooManyHints);
        }
        Ok(HintBar {
            hints: hints.iter().map(|&(k, d)| (String::from(k), String::from(d))).collect(),
        })
    }

    /// Fills `row` with `theme.surface`, then writes the hints from column 1,
    /// separated by two spaces: key in `accent`, description in `muted`.
    pub fn draw(&self, grid: &mut Grid, row: usize, theme: &Theme) {
        if row >= grid.rows() {
            return;
        }
        for col in 0..grid.cols() {
            grid.set(col, row, cell(' ', theme.text, theme.surface));
        }
        let mut col = 1usize;
        for (key, desc) in &self.hints {
            col = col.saturating_add(grid.put_str(col, row, key, theme.accent, theme.surface));
            col = col.saturating_add(grid.put_str(col, row, " ", theme.muted, theme.surface));
            col = col.saturating_add(grid.put_str(col, row, desc, theme.muted, theme.surface));
            col = col.saturating_add(2);
        }
    }
}

/// Full-width bar: `left` in `text` from column 0, `right` in `muted`
/// right-aligned. When space is short `left` is truncated (one gap column is
/// kept before `right`); `right` stays visible.
pub fn status_bar(grid: &mut Grid, row: usize, left: &str, right: &str, theme: &Theme) {
    if row >= grid.rows() {
        return;
    }
    let cols = grid.cols();
    for col in 0..cols {
        grid.set(col, row, cell(' ', theme.text, theme.surface));
    }
    let right_len = right.chars().count().min(cols);
    let right_col = cols - right_len;
    let avail = if right_len == 0 { cols } else { right_col.saturating_sub(1) };
    let left_cut: String = left.chars().take(avail).collect();
    grid.put_str(0, row, &left_cut, theme.text, theme.surface);
    let right_cut: String = right.chars().take(right_len).collect();
    grid.put_str(right_col, row, &right_cut, theme.muted, theme.surface);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::AURORA;

    const BLANK: Cell = Cell { ch: '.', fg: Rgb(1, 1, 1), bg: Rgb(2, 2, 2), bold: false };
    const BORDER: Rgb = Rgb(9, 9, 9);
    const FILL: Rgb = Rgb(5, 5, 5);

    fn ch(g: &Grid, c: usize, r: usize) -> char {
        g.get(c, r).unwrap().ch
    }

    #[test]
    fn rounded_box_has_rounded_corners_and_filled_interior() {
        let mut g = Grid::new(10, 6, BLANK);
        rounded_box(&mut g, Rect { col: 1, row: 1, cols: 5, rows: 4 }, BORDER, FILL);
        assert_eq!(ch(&g, 1, 1), '╭');
        assert_eq!(ch(&g, 5, 1), '╮');
        assert_eq!(ch(&g, 1, 4), '╰');
        assert_eq!(ch(&g, 5, 4), '╯');
        assert_eq!(ch(&g, 3, 1), '─');
        assert_eq!(ch(&g, 3, 4), '─');
        assert_eq!(ch(&g, 1, 2), '│');
        assert_eq!(ch(&g, 5, 3), '│');
        assert_eq!(g.get(1, 1).unwrap().fg, BORDER);
        for (c, r) in [(2, 2), (3, 3), (4, 2)] {
            assert_eq!(ch(&g, c, r), ' ');
            assert_eq!(g.get(c, r).unwrap().bg, FILL);
        }
        assert_eq!(ch(&g, 0, 0), '.');
        assert_eq!(ch(&g, 6, 2), '.');
    }

    #[test]
    fn rounded_box_smaller_than_2x2_is_ignored() {
        let mut g = Grid::new(6, 6, BLANK);
        rounded_box(&mut g, Rect { col: 1, row: 1, cols: 1, rows: 3 }, BORDER, FILL);
        rounded_box(&mut g, Rect { col: 1, row: 1, cols: 3, rows: 1 }, BORDER, FILL);
        rounded_box(&mut g, Rect { col: 1, row: 1, cols: 0, rows: 0 }, BORDER, FILL);
        assert_eq!(g.take_dirty(), None);
        rounded_box(&mut g, Rect { col: 1, row: 1, cols: 2, rows: 2 }, BORDER, FILL);
        assert_eq!(ch(&g, 1, 1), '╭');
        assert_eq!(ch(&g, 2, 2), '╯');
    }

    #[test]
    fn rounded_box_clipped_by_grid_edge_does_not_panic() {
        let mut g = Grid::new(6, 4, BLANK);
        rounded_box(&mut g, Rect { col: 4, row: 2, cols: 10, rows: 10 }, BORDER, FILL);
        assert_eq!(ch(&g, 4, 2), '╭');
        assert_eq!(ch(&g, 5, 2), '─');
        assert_eq!(ch(&g, 4, 3), '│');
        rounded_box(&mut g, Rect { col: 100, row: 100, cols: 3, rows: 3 }, BORDER, FILL);
        rounded_box(&mut g, Rect { col: usize::MAX, row: usize::MAX, cols: usize::MAX, rows: usize::MAX }, BORDER, FILL);
        rounded_box(&mut g, Rect { col: 0, row: 0, cols: usize::MAX, rows: usize::MAX }, BORDER, FILL);
        assert_eq!(ch(&g, 0, 0), '╭');
    }

    #[test]
    fn hint_bar_rejects_four_hints() {
        let h = [("a", "1"), ("b", "2"), ("c", "3"), ("d", "4")];
        assert_eq!(HintBar::new(&h).unwrap_err(), TooManyHints);
    }

    #[test]
    fn hint_bar_accepts_zero_to_three() {
        let h = [("a", "1"), ("b", "2"), ("c", "3")];
        for n in 0..=3 {
            assert!(HintBar::new(&h[..n]).is_ok(), "{n}");
        }
        let bar = HintBar::new(&[("Enter", "open")]).unwrap();
        let mut g = Grid::new(20, 2, BLANK);
        bar.draw(&mut g, 1, &AURORA);
        assert_eq!(ch(&g, 1, 1), 'E');
        assert_eq!(g.get(1, 1).unwrap().fg, AURORA.accent);
        assert_eq!(ch(&g, 7, 1), 'o');
        assert_eq!(g.get(7, 1).unwrap().fg, AURORA.muted);
        assert_eq!(g.get(19, 1).unwrap().bg, AURORA.surface);
        bar.draw(&mut g, 2, &AURORA); // beyond grid: no panic
    }

    #[test]
    fn status_bar_keeps_right_text_when_left_too_long() {
        let mut g = Grid::new(10, 1, BLANK);
        status_bar(&mut g, 0, "a very long left text", "RIGHT", &AURORA);
        let row: String = (0..10).map(|c| ch(&g, c, 0)).collect();
        assert_eq!(row, "a ve RIGHT");
        assert_eq!(g.get(0, 0).unwrap().fg, AURORA.text);
        assert_eq!(g.get(5, 0).unwrap().fg, AURORA.muted);
        let mut g = Grid::new(10, 1, BLANK);
        status_bar(&mut g, 0, "ab", "xyz", &AURORA);
        let row: String = (0..10).map(|c| ch(&g, c, 0)).collect();
        assert_eq!(row, "ab     xyz");
        status_bar(&mut g, 3, "x", "y", &AURORA); // beyond grid: no panic
        let mut g = Grid::new(4, 1, BLANK);
        status_bar(&mut g, 0, "left", "toolongright", &AURORA);
        assert_eq!(ch(&g, 0, 0), 't');
    }

    #[test]
    fn status_bar_empty_texts_draw_only_background() {
        let mut g = Grid::new(8, 2, BLANK);
        status_bar(&mut g, 1, "", "", &AURORA);
        for c in 0..8 {
            let cl = g.get(c, 1).unwrap();
            assert_eq!(cl.ch, ' ');
            assert_eq!(cl.bg, AURORA.surface);
        }
        assert_eq!(ch(&g, 0, 0), '.');
    }
}
