//! Pure grid-to-pixels renderer (no_std, no floats, no unsafe).

use crate::cell::{Grid, Rgb};
use crate::font::{glyph, GLYPH_H, GLYPH_W};
use core::ops::Range;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PixelFormat {
    Rgb,
    Bgr,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FbInfo {
    pub width: usize,
    pub height: usize,
    pub stride_px: usize,
    pub bytes_per_px: usize,
    pub format: PixelFormat,
}

pub fn grid_size_for(info: &FbInfo) -> (usize, usize) {
    (info.width / GLYPH_W, info.height / GLYPH_H)
}

/// Writes one pixel; silently skips it if it does not fit in `fb`.
/// For 4-byte pixels the 4th (padding/alpha) byte is always 0xFF, so the
/// output is opaque whether the firmware treats it as padding or as alpha.
fn put_px(fb: &mut [u8], idx: usize, c: Rgb, info: &FbInfo) {
    let (a, b, d) = match info.format {
        PixelFormat::Rgb => (c.0, c.1, c.2),
        PixelFormat::Bgr => (c.2, c.1, c.0),
    };
    if let Some(px) = fb.get_mut(idx..).and_then(|s| s.get_mut(..info.bytes_per_px)) {
        px[0] = a;
        px[1] = b;
        px[2] = d;
        if let Some(pad) = px.get_mut(3) {
            *pad = 0xFF;
        }
    }
}

/// Draws the given grid rows into `fb`. Only whole 8x16 cells inside both the
/// grid and the framebuffer are drawn; bytes outside them are never touched,
/// and pixels that do not fit in `fb` are skipped instead of panicking.
pub fn render(grid: &Grid, rows: Range<usize>, fb: &mut [u8], info: &FbInfo) {
    if info.bytes_per_px != 3 && info.bytes_per_px != 4 {
        return;
    }
    let (fb_cols, fb_rows) = grid_size_for(info);
    let cols = grid.cols().min(fb_cols);
    let row_end = rows.end.min(grid.rows()).min(fb_rows);
    for row in rows.start..row_end {
        for col in 0..cols {
            let Some(cell) = grid.get(col, row) else { continue };
            let g = glyph(cell.ch);
            for (gy, bits) in g.iter().enumerate() {
                // Row and column are bounded by width/height, so these cannot
                // overflow unless stride/bpp are absurd; checked anyway.
                let base = (row * GLYPH_H + gy)
                    .checked_mul(info.stride_px)
                    .and_then(|v| v.checked_add(col * GLYPH_W));
                let Some(base) = base else { continue };
                for gx in 0..GLYPH_W {
                    let set = bits & (0x80 >> gx) != 0;
                    let colour = if set { cell.fg } else { cell.bg };
                    let idx = base.checked_add(gx).and_then(|v| v.checked_mul(info.bytes_per_px));
                    let Some(idx) = idx else { continue };
                    put_px(fb, idx, colour, info);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cell::Cell;
    use crate::theme::AURORA;
    use alloc::vec;
    use alloc::vec::Vec;

    fn info(width: usize, height: usize, stride_px: usize, bpp: usize, format: PixelFormat) -> FbInfo {
        FbInfo { width, height, stride_px, bytes_per_px: bpp, format }
    }

    fn blank(fg: Rgb, bg: Rgb) -> Cell {
        Cell { ch: ' ', fg, bg, bold: false }
    }

    fn fnv1a(bytes: &[u8]) -> u64 {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for &b in bytes {
            h ^= b as u64;
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
        h
    }

    #[test]
    fn grid_size_for_1280x720() {
        let i = info(1280, 720, 1280, 4, PixelFormat::Bgr);
        assert_eq!(grid_size_for(&i), (160, 45));
    }

    #[test]
    fn rgb_and_bgr_swap_channels() {
        // '█' is not guaranteed in the font; use bg of a blank cell: every pixel is bg.
        let g = Grid::new(1, 1, blank(Rgb(9, 9, 9), Rgb(1, 2, 3)));
        let mut rgb = vec![0u8; 8 * 16 * 3];
        render(&g, 0..1, &mut rgb, &info(8, 16, 8, 3, PixelFormat::Rgb));
        assert_eq!(&rgb[..3], &[1, 2, 3]);
        let mut bgr = vec![0u8; 8 * 16 * 3];
        render(&g, 0..1, &mut bgr, &info(8, 16, 8, 3, PixelFormat::Bgr));
        assert_eq!(&bgr[..3], &[3, 2, 1]);
        // fg path too: find a glyph with a set pixel and check its colour.
        let g = Grid::new(1, 1, Cell { ch: 'H', fg: Rgb(1, 2, 3), bg: Rgb(200, 100, 50), bold: false });
        let mut bgr = vec![0u8; 8 * 16 * 3];
        render(&g, 0..1, &mut bgr, &info(8, 16, 8, 3, PixelFormat::Bgr));
        let px: Vec<&[u8]> = bgr.chunks(3).collect();
        assert!(px.contains(&&[3u8, 2, 1][..]), "fg pixel missing");
        assert!(px.contains(&&[50u8, 100, 200][..]), "bg pixel missing");
    }

    #[test]
    fn four_byte_pixels_set_pad_byte_to_0xff() {
        let g = Grid::new(1, 1, blank(Rgb(9, 9, 9), Rgb(1, 2, 3)));
        let mut fb = vec![0u8; 8 * 16 * 4];
        render(&g, 0..1, &mut fb, &info(8, 16, 8, 4, PixelFormat::Rgb));
        assert_eq!(&fb[..4], &[1, 2, 3, 0xFF]);
        assert_eq!(&fb[8 * 16 * 4 - 4..], &[1, 2, 3, 0xFF]);
    }

    #[test]
    fn stride_larger_than_width_does_not_shift_rows() {
        let g = Grid::new(1, 1, blank(Rgb(9, 9, 9), Rgb(1, 2, 3)));
        let stride = 12;
        let mut fb = vec![0xAAu8; stride * 16 * 4];
        render(&g, 0..1, &mut fb, &info(8, 16, stride, 4, PixelFormat::Rgb));
        let off = stride * 4;
        assert_eq!(&fb[off..off + 4], &[1, 2, 3, 0xFF]); // pixel (0,1)
        assert_eq!(&fb[7 * 4..8 * 4], &[1, 2, 3, 0xFF]); // pixel (7,0)
        assert_eq!(&fb[8 * 4..off], &vec![0xAAu8; off - 8 * 4][..]); // padding untouched
    }

    #[test]
    fn only_requested_rows_are_touched() {
        let g = Grid::new(2, 4, blank(Rgb(9, 9, 9), Rgb(1, 2, 3)));
        let i = info(16, 64, 16, 3, PixelFormat::Rgb);
        let mut fb = vec![0xAAu8; 16 * 64 * 3];
        render(&g, 2..3, &mut fb, &i);
        let row_bytes = 16 * 16 * 3;
        for (n, b) in fb.iter().enumerate() {
            if (2 * row_bytes..3 * row_bytes).contains(&n) {
                assert_ne!(*b, 0xAA, "byte {n} not drawn");
            } else {
                assert_eq!(*b, 0xAA, "byte {n} touched");
            }
        }
    }

    #[test]
    fn odd_width_leaves_remainder_pixels_untouched() {
        let g = Grid::new(2, 1, blank(Rgb(9, 9, 9), Rgb(1, 2, 3)));
        let i = info(19, 16, 19, 3, PixelFormat::Rgb); // 2 cells + 3 leftover px
        assert_eq!(grid_size_for(&i), (2, 1));
        let mut fb = vec![0xAAu8; 19 * 16 * 3];
        render(&g, 0..1, &mut fb, &i);
        for y in 0..16 {
            for x in 0..19 {
                let p = &fb[(y * 19 + x) * 3..][..3];
                if x < 16 {
                    assert_eq!(p, &[1, 2, 3]);
                } else {
                    assert_eq!(p, &[0xAA; 3]);
                }
            }
        }
    }

    #[test]
    fn buffer_too_small_does_not_panic() {
        let g = Grid::new(4, 2, Cell { ch: 'W', fg: Rgb(1, 2, 3), bg: Rgb(4, 5, 6), bold: false });
        let i = info(32, 32, 32, 4, PixelFormat::Bgr);
        for len in [0usize, 1, 3, 4, 5, 100, 1000, 32 * 32 * 4 - 1] {
            let mut fb = vec![0u8; len];
            render(&g, 0..2, &mut fb, &i);
        }
        // Out-of-range rows and absurd geometry must not panic either.
        let mut fb = vec![0u8; 64];
        render(&g, 0..usize::MAX, &mut fb, &i);
        render(&g, 5..9, &mut fb, &i);
        render(&g, 3..1, &mut fb, &i);
        let huge = info(usize::MAX, usize::MAX, usize::MAX, 4, PixelFormat::Rgb);
        render(&g, 0..2, &mut fb, &huge);
        let bad_bpp = info(32, 32, 32, 0, PixelFormat::Rgb);
        render(&g, 0..2, &mut fb, &bad_bpp);
        assert_eq!(grid_size_for(&huge), (usize::MAX / 8, usize::MAX / 16));
    }

    #[test]
    fn partial_buffer_still_draws_what_fits() {
        let g = Grid::new(1, 1, blank(Rgb(9, 9, 9), Rgb(1, 2, 3)));
        let mut fb = vec![0u8; 8 * 3 + 2]; // one pixel row plus a torn pixel
        render(&g, 0..1, &mut fb, &info(8, 16, 8, 3, PixelFormat::Rgb));
        assert_eq!(&fb[..3], &[1, 2, 3]);
        assert_eq!(&fb[8 * 3..], &[0, 0]);
    }

    fn hello_buffer() -> Vec<u8> {
        let mut g = Grid::new(5, 1, blank(AURORA.text, AURORA.bg));
        g.put_str(0, 0, "Hello", AURORA.text, AURORA.bg);
        let i = info(40, 16, 40, 4, PixelFormat::Bgr);
        let mut fb = vec![0u8; 40 * 16 * 4];
        render(&g, 0..1, &mut fb, &i);
        fb
    }

    #[test]
    fn golden_hash_of_hello_row() {
        // Verified visually (ASCII dump of the 40x16 buffer showed "Hello" in
        // text-on-bg colours, BGR, 4 bytes/px) before pinning.
        assert_eq!(fnv1a(&hello_buffer()), 0x5d59_e624_33ee_8205);
    }
}
