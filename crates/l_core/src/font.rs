//! Embedded 8x16 bitmap font (Terminus 16n, SIL OFL 1.1; see `LICENSE-font`).
//!
//! Each glyph is 16 rows of one byte; bit 7 is the leftmost pixel. The table
//! lives in the generated `font_data.rs`, sorted by codepoint.

pub const GLYPH_W: usize = 8;
pub const GLYPH_H: usize = 16;

/// Hollow box shown for every codepoint the font does not cover.
static REPLACEMENT: [u8; GLYPH_H] = [
    0x00, 0x00, 0x00, 0x7E, 0x42, 0x42, 0x42, 0x42, 0x42, 0x42, 0x42, 0x42, 0x7E, 0x00, 0x00, 0x00,
];

/// Glyph bitmap for `ch`, or the replacement glyph if it is not embedded.
/// Never panics.
pub fn glyph(ch: char) -> &'static [u8; GLYPH_H] {
    match crate::font_data::GLYPHS.binary_search_by_key(&ch, |&(c, _)| c) {
        Ok(i) => &crate::font_data::GLYPHS[i].1,
        Err(_) => &REPLACEMENT,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BLANK: [u8; GLYPH_H] = [0; GLYPH_H];

    #[test]
    fn ascii_printable_all_have_distinct_nonblank_glyphs() {
        for c in 0x21u8..=0x7E {
            assert_ne!(*glyph(c as char), BLANK, "blank glyph for {:?}", c as char);
        }
        for a in 0x21u8..=0x7E {
            for b in (a + 1)..=0x7E {
                assert_ne!(glyph(a as char), glyph(b as char), "{:?} == {:?}", a as char, b as char);
            }
        }
    }

    #[test]
    fn space_is_blank() {
        assert_eq!(*glyph(' '), BLANK);
    }

    #[test]
    fn german_umlauts_and_eszett_present() {
        for c in "äöüÄÖÜß".chars() {
            assert_ne!(glyph(c), &REPLACEMENT, "missing {c:?}");
            assert_ne!(*glyph(c), BLANK, "blank {c:?}");
        }
    }

    #[test]
    fn box_drawing_and_rounded_corners_present() {
        for c in "─│╭╮╰╯●█▀▄░▒▓".chars() {
            assert_ne!(glyph(c), &REPLACEMENT, "missing {c:?}");
            assert_ne!(*glyph(c), BLANK, "blank {c:?}");
        }
    }

    #[test]
    fn unknown_codepoints_return_replacement() {
        for c in ['\u{1F600}', '中', '\u{0301}'] {
            assert_eq!(glyph(c), &REPLACEMENT);
        }
        assert_ne!(REPLACEMENT, BLANK);
    }

    #[test]
    fn table_is_strictly_sorted() {
        let t = crate::font_data::GLYPHS;
        assert!(t.windows(2).all(|w| w[0].0 < w[1].0), "GLYPHS must be strictly sorted by char");
    }
}
