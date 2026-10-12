//! The framebuffer console (`src/fb_console.rs`) on the real UEFI framebuffer.
//!
//! Expected pixels are always computed with `l_core::render` (the same pure
//! renderer the host tests check), never hard-coded, so the comparison holds
//! for any width, stride, bytes per pixel and Rgb/Bgr order the firmware picks.
#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(l_os::test_runner)]
#![reexport_test_harness_main = "test_main"]

extern crate alloc;

use alloc::vec;
use alloc::vec::Vec;
use bootloader_api::{BootInfo, entry_point};
use core::panic::PanicInfo;
use l_core::cell::{Cell, Grid, Rgb};
use l_core::font::{GLYPH_H, GLYPH_W};
use l_core::render::{FbInfo, render};
use l_core::theme::AURORA;
use l_os::{fb_console, print, println, serial_println};
use x86_64::instructions::interrupts;

entry_point!(main, config = &l_os::BOOTLOADER_CONFIG);

fn main(boot_info: &'static mut BootInfo) -> ! {
    l_os::init();
    // Initializes the console on the framebuffer (after the heap).
    l_os::init_memory(boot_info);
    test_main();
    l_os::hlt_loop();
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    l_os::test_panic_handler(info)
}

/// One text row as the console should draw it: `text` in `fg` on the theme
/// background, the rest of the row blank. Rendered by `l_core::render` into a
/// buffer with the screen's stride, bytes per pixel and pixel format.
fn expected_row(info: &FbInfo, cols: usize, text: &str, fg: Rgb) -> Vec<u8> {
    let blank = Cell { ch: ' ', fg: AURORA.text, bg: AURORA.bg, bold: false };
    let mut grid = Grid::new(cols, 1, blank);
    grid.put_str(0, 0, text, fg, AURORA.bg);
    let row_info = FbInfo { height: GLYPH_H, ..*info };
    let mut buf = vec![0u8; info.stride_px * GLYPH_H * info.bytes_per_px];
    render(&grid, 0..1, &mut buf, &row_info);
    buf
}

/// Compares the first `cells` cells of text row `row` on the screen with
/// row 0 of `expected` (as produced by [`expected_row`]).
fn cells_match(info: &FbInfo, fb: &[u8], row: usize, cells: usize, expected: &[u8]) -> bool {
    let line = info.stride_px * info.bytes_per_px;
    let used = cells * GLYPH_W * info.bytes_per_px;
    (0..GLYPH_H).all(|y| {
        let start = (row * GLYPH_H + y) * line;
        fb[start..start + used] == expected[y * line..y * line + used]
    })
}

fn screen_row_is(row: usize, text: &str, fg: Rgb) -> bool {
    let (cols, _) = fb_console::grid_size().expect("console not initialized");
    fb_console::with_framebuffer(|info, fb| {
        let expected = expected_row(info, cols, text, fg);
        cells_match(info, fb, row, cols, &expected)
    })
    .expect("console not initialized")
}

// Must stay the first test (the custom test framework runs them in name
// order): it checks the state right after `init`.
#[test_case]
fn console_writes_pixels() {
    print!("A");
    let (cols, _) = fb_console::grid_size().expect("console not initialized");
    fb_console::with_framebuffer(|info, fb| {
        let expected = expected_row(info, cols, "A", AURORA.text);
        // Cell (0,0) exactly as l_core::render draws it ...
        assert!(cells_match(info, fb, 0, 1, &expected), "cell (0,0) differs from l_core::render");
        // ... and not trivially: the glyph has foreground and background pixels.
        let blank = expected_row(info, cols, "", AURORA.text);
        assert!(!cells_match(info, fb, 0, 1, &blank), "cell (0,0) shows no glyph");
        // The rest of the row is blank.
        assert!(cells_match(info, fb, 0, cols, &expected), "row 0 differs from l_core::render");
    })
    .expect("console not initialized");
    // `init` cleared the whole screen to the theme background.
    let (_, rows) = fb_console::grid_size().unwrap();
    for row in 1..rows {
        assert!(screen_row_is(row, "", AURORA.text), "row {} not blank after init", row);
    }
    assert_eq!(fb_console::cursor(), Some((1, 0)));
}

#[test_case]
fn emergency_print_works_while_locked() {
    let (_, rows) = fb_console::grid_size().expect("console not initialized");
    interrupts::without_interrupts(|| {
        // Simulate a panic that interrupted code holding the console lock.
        let guard = fb_console::CONSOLE.lock();
        // Must return (a blocking lock() would hang until the runner's timeout).
        fb_console::emergency_print(format_args!("emergency while locked"));
        drop(guard);
    });
    assert!(
        screen_row_is(rows - 1, "emergency while locked", AURORA.err),
        "emergency text missing from the bottom row"
    );
    // The console is usable afterwards.
    println!();
    assert!(fb_console::CONSOLE.try_lock().is_some(), "console lock left held");
}

#[test_case]
fn emergency_print_without_lock_goes_through_the_console() {
    fb_console::clear_screen();
    fb_console::emergency_print(format_args!("panic text\n"));
    assert_eq!(fb_console::cell_at(0, 0), Some('p'));
    assert_eq!(fb_console::cursor(), Some((0, 1)));
    assert!(screen_row_is(0, "panic text", AURORA.err));
}

#[test_case]
fn screen_clear_homes_the_cursor_and_blanks_it() {
    println!("something");
    fb_console::clear_screen();
    assert_eq!(fb_console::cursor(), Some((0, 0)));
    let (_, rows) = fb_console::grid_size().unwrap();
    for row in 0..rows {
        assert!(screen_row_is(row, "", AURORA.text), "row {} not blank", row);
    }
}

#[test_case]
fn erase_with_backspace_stops_at_column_zero() {
    fb_console::clear_screen();
    print!("abc");
    fb_console::backspace();
    assert_eq!(fb_console::cell_at(1, 0), Some('b'));
    assert_eq!(fb_console::cell_at(2, 0), Some(' '));
    assert_eq!(fb_console::cursor(), Some((2, 0)));
    fb_console::backspace();
    fb_console::backspace();
    fb_console::backspace(); // at column 0: must not underflow
    assert_eq!(fb_console::cursor(), Some((0, 0)));
    assert!(screen_row_is(0, "", AURORA.text));
}

#[test_case]
fn long_lines_wrap_and_non_ascii_uses_the_font() {
    fb_console::clear_screen();
    let (cols, _) = fb_console::grid_size().unwrap();
    for _ in 0..cols + 2 {
        print!("w");
    }
    assert_eq!(fb_console::cursor(), Some((2, 1)));
    // A character the font has is drawn as itself; a control character is
    // never interpreted, it becomes the replacement glyph.
    print!("ä\u{7}\r");
    assert_eq!(fb_console::cell_at(2, 1), Some('ä'));
    assert_eq!(fb_console::cell_at(3, 1), Some(char::REPLACEMENT_CHARACTER));
    assert_eq!(fb_console::cell_at(4, 1), Some(char::REPLACEMENT_CHARACTER));
    assert!(screen_row_is(1, "wwä\u{FFFD}\u{FFFD}", AURORA.text));
}

/// 2000 lines through `println!`, each one a separate print call: only the
/// changed rows may be rendered (a full redraw per line would be 2000 x rows),
/// and the result after all the scrolling must still be pixel-exact.
#[test_case]
fn two_thousand_lines_render_only_changed_rows() {
    const LINES: usize = 2000;
    fb_console::clear_screen();
    let (_, rows) = fb_console::grid_size().unwrap();
    let rendered_before = fb_console::rendered_rows();
    let rtc_before = l_os::rtc::unix_time();
    let start = l_os::time::ticks();
    for i in 0..LINES {
        println!("line {:4} of the scroll test, padded to look like real output", i);
    }
    let ticks = l_os::time::ticks() - start;
    let rtc_after = l_os::rtc::unix_time();
    let rendered = fb_console::rendered_rows() - rendered_before;
    serial_println!(
        "  {} lines: {} ticks ({} Hz PIT), RTC {:?} -> {:?}, {} text rows rendered",
        LINES, ticks, l_os::time::TICK_HZ, rtc_before, rtc_after, rendered
    );
    // Each line dirties its own row and the new blank bottom row.
    assert!(rendered <= 2 * LINES as u64 + rows as u64, "{} rows rendered: full redraws?", rendered);
    // Measured: 450-582 ticks at 100 Hz (~4.5-5.8 s); before at 18.2 Hz 78-89 ticks
    // (~4-5 s), and 1267 ticks (~70 s) with an unoptimized l_core. Bound: 20 s.
    assert!(ticks < l_os::time::ticks_from_seconds(20), "2000 lines took {} ticks", ticks);
    // After the scrolling, the screen shows the last rows-1 lines, then the empty cursor row.
    let mut text = alloc::string::String::new();
    for row in 0..rows - 1 {
        use core::fmt::Write;
        text.clear();
        let line = LINES - (rows - 1) + row;
        write!(text, "line {:4} of the scroll test, padded to look like real output", line).unwrap();
        assert!(screen_row_is(row, &text, AURORA.text), "row {} is not line {}", row, line);
    }
    assert!(screen_row_is(rows - 1, "", AURORA.text), "cursor row not blank");
}
