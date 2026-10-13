//! The Aurora chrome of the framebuffer console: a status bar on the first
//! text row, a hint bar on the last one, and the text region in between
//! (`fb_console::enable_chrome`, started by `l_os::aurora::show_chrome` as in
//! the real kernel entry).
//!
//! Expected pixels are computed with the `l_core` widgets and renderer in the
//! final `AURORA` colours, never hard-coded, so the comparison is independent
//! of how the console draws them.
#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(l_os::test_runner)]
#![reexport_test_harness_main = "test_main"]

extern crate alloc;

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use bootloader_api::{BootInfo, entry_point};
use core::fmt::Write;
use core::panic::PanicInfo;
use l_core::anim::fade_theme;
use l_core::cell::{Cell, Grid};
use l_core::font::{GLYPH_H, GLYPH_W};
use l_core::render::{FbInfo, render};
use l_core::theme::{AURORA, Theme};
use l_core::widgets::status_bar;
use l_os::{aurora, fb_console, print, println, serial_println, time};

entry_point!(main, config = &l_os::BOOTLOADER_CONFIG);

fn main(boot_info: &'static mut BootInfo) -> ! {
    l_os::init();
    l_os::init_memory(boot_info);
    test_main();
    l_os::hlt_loop();
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    l_os::test_panic_handler(info)
}

#[derive(Clone, Copy)]
enum Bar {
    Status,
    Hints,
}

/// One screen row in the screen's stride, bytes per pixel and pixel format,
/// drawn by `l_core::render` from a one-row grid.
fn row_pixels(info: &FbInfo, grid: &Grid) -> Vec<u8> {
    let row_info = FbInfo { height: GLYPH_H, ..*info };
    let mut buf = vec![0u8; info.stride_px * GLYPH_H * info.bytes_per_px];
    render(grid, 0..1, &mut buf, &row_info);
    buf
}

fn blank_grid(cols: usize) -> Grid {
    Grid::new(cols, 1, Cell { ch: ' ', fg: AURORA.text, bg: AURORA.bg, bold: false })
}

/// A bar as the l_core widgets draw it with `theme`.
fn expected_bar(info: &FbInfo, cols: usize, bar: Bar, theme: &Theme) -> Vec<u8> {
    let mut grid = blank_grid(cols);
    match bar {
        Bar::Status => status_bar(&mut grid, 0, aurora::STATUS_LEFT, aurora::STATUS_RIGHT, theme),
        Bar::Hints => aurora::hint_bar().draw(&mut grid, 0, theme),
    }
    row_pixels(info, &grid)
}

fn expected_text(info: &FbInfo, cols: usize, text: &str) -> Vec<u8> {
    let mut grid = blank_grid(cols);
    grid.put_str(0, 0, text, AURORA.text, AURORA.bg);
    row_pixels(info, &grid)
}

fn row_matches(info: &FbInfo, fb: &[u8], row: usize, cols: usize, expected: &[u8]) -> bool {
    let line = info.stride_px * info.bytes_per_px;
    let used = cols * GLYPH_W * info.bytes_per_px;
    (0..GLYPH_H).all(|y| {
        let start = (row * GLYPH_H + y) * line;
        fb[start..start + used] == expected[y * line..y * line + used]
    })
}

fn size() -> (usize, usize) {
    fb_console::grid_size().expect("console not initialized")
}

fn bar_is(row: usize, bar: Bar, theme: &Theme) -> bool {
    let (cols, _) = size();
    fb_console::with_framebuffer(|info, fb| row_matches(info, fb, row, cols, &expected_bar(info, cols, bar, theme)))
        .expect("console not initialized")
}

fn text_row_is(row: usize, text: &str) -> bool {
    let (cols, _) = size();
    fb_console::with_framebuffer(|info, fb| row_matches(info, fb, row, cols, &expected_text(info, cols, text)))
        .expect("console not initialized")
}

/// Both bars in the final Aurora colours, pixel-exact.
fn assert_bars_final(context: &str) {
    let (_, rows) = size();
    assert!(bar_is(0, Bar::Status, &AURORA), "{}: status bar (row 0) is not the final Aurora status bar", context);
    assert!(bar_is(rows - 1, Bar::Hints, &AURORA), "{}: hint bar (last row) is not the final Aurora hint bar", context);
}

fn row_chars(row: usize) -> String {
    let (cols, _) = size();
    (0..cols).map(|c| fb_console::cell_at(c, row).unwrap()).collect()
}

/// Must stay the first test (tests run in name order): it turns the chrome on
/// exactly as the kernel entry does and checks the boot fade.
#[test_case]
fn a_boot_fade_ends_in_the_final_aurora_colours() {
    let (_, rows) = size();
    // Opt-in: without the call the console uses every row (tests/framebuffer.rs).
    assert_eq!(fb_console::region(), Some(0..rows));
    assert_eq!(aurora::STATUS_LEFT, "Larp OS");
    assert_eq!(aurora::STATUS_RIGHT, "amnesisch: nein");
    print!("old text");
    let rendered_before = fb_console::rendered_rows();
    let start = time::ticks();
    let frames = aurora::show_chrome().expect("chrome not shown");
    let elapsed = time::ticks() - start;
    let rendered = fb_console::rendered_rows() - rendered_before;
    let fade_ticks = time::ticks_from_ms(aurora::BOOT_FADE_MS);
    serial_println!("  fade: {} frames in {} ticks (fade {} ticks), {} text rows rendered", frames, elapsed, fade_ticks, rendered);

    // The region lies between the bars; the cursor starts at its first row.
    assert_eq!(fb_console::region(), Some(1..rows - 1));
    assert_eq!(fb_console::cursor(), Some((0, 1)));
    // After the fade the bars are exactly the final Aurora bars (alpha 255).
    assert_eq!(fb_console::chrome_alpha(), Some(255));
    assert_bars_final("after the boot fade");
    assert!(row_chars(0).starts_with("Larp OS"));
    assert!(row_chars(0).trim_end().ends_with("amnesisch: nein"));
    // The old text is gone: the region was cleared.
    for row in 1..rows - 1 {
        assert!(text_row_is(row, ""), "region row {} not blank after enable_chrome", row);
    }
    if !aurora::REDUCE_MOTION {
        // The fade really waited on the timer, frame by frame, until the eased
        // opacity rounds to 255 (ease-out gets there a little before the end) ...
        let fade = l_core::anim::Fade { start: 0, duration: fade_ticks };
        let full = (0..=fade_ticks).find(|&t| fade.alpha(t, false) == 255).unwrap();
        assert!(full > fade_ticks / 2, "{}", full);
        assert!(elapsed >= full, "fade over after {} ticks, expected >= {}", elapsed, full);
        assert!(elapsed < fade_ticks + time::ticks_from_seconds(5), "fade took {} ticks", elapsed);
        assert!(frames >= 3, "only {} frames", frames);
        assert!(frames as u64 <= fade_ticks + 1, "{} frames for a {}-tick fade", frames, fade_ticks);
    }
    // ... and every frame re-rendered only the two chrome rows: enabling drew
    // the whole screen once (cleared region + bars), each frame two rows.
    assert_eq!(rendered, rows as u64 + 2 * frames as u64, "rows rendered during the fade");
}

#[test_case]
fn backspace_at_column_zero_stays_in_the_region() {
    let (_, rows) = size();
    assert_eq!(fb_console::region(), Some(1..rows - 1), "chrome not enabled");
    fb_console::clear_screen();
    for _ in 0..3 {
        fb_console::backspace();
    }
    assert_eq!(fb_console::cursor(), Some((0, 1)));
    print!("ab");
    for _ in 0..5 {
        fb_console::backspace();
    }
    assert_eq!(fb_console::cursor(), Some((0, 1)));
    assert!(text_row_is(1, ""));
    assert_bars_final("after backspace at column 0");
}

#[test_case]
fn form_feed_clear_keeps_the_bars() {
    let (_, rows) = size();
    assert_eq!(fb_console::region(), Some(1..rows - 1), "chrome not enabled");
    for i in 0..rows + 3 {
        println!("filler {}", i);
    }
    fb_console::clear_screen();
    assert_eq!(fb_console::cursor(), Some((0, 1)));
    for row in 1..rows - 1 {
        assert!(text_row_is(row, ""), "region row {} not blank after clear", row);
    }
    assert_bars_final("after clear");
    print!("x");
    assert_eq!(fb_console::cell_at(0, 1), Some('x'));
    assert!(text_row_is(1, "x"));
}

#[test_case]
fn printing_200_lines_never_touches_the_bars() {
    const LINES: usize = 200;
    let (_, rows) = size();
    assert_eq!(fb_console::region(), Some(1..rows - 1), "chrome not enabled");
    fb_console::clear_screen();
    let (status, hints) = (row_chars(0), row_chars(rows - 1));
    for i in 0..LINES {
        println!("chrome line {}", i);
        assert_eq!(row_chars(0), status, "status bar cells changed after line {}", i);
        assert_eq!(row_chars(rows - 1), hints, "hint bar cells changed after line {}", i);
    }
    assert_bars_final("after 200 lines");
    // The region shows the last region-height-1 lines, then the empty cursor row
    // directly above the hint bar.
    let last = rows - 2;
    assert_eq!(fb_console::cursor(), Some((0, last)));
    let mut text = String::new();
    for row in 1..last {
        let line = LINES - (last - row);
        text.clear();
        write!(text, "chrome line {}", line).unwrap();
        assert!(text_row_is(row, &text), "row {} is not line {}", row, line);
    }
    assert!(text_row_is(last, ""), "cursor row not blank");
}

#[test_case]
fn reduce_motion_shows_the_final_colours_at_once() {
    let (_, rows) = size();
    fb_console::set_chrome_alpha(0);
    // Alpha 0: the bars are drawn in the background colour (invisible).
    assert!(bar_is(0, Bar::Status, &fade_theme(&AURORA, 0)));
    assert!(bar_is(rows - 1, Bar::Hints, &fade_theme(&AURORA, 0)));
    assert!(!bar_is(0, Bar::Status, &AURORA));
    let start = time::ticks();
    let frames = fb_console::fade_in_chrome(time::ticks_from_ms(aurora::BOOT_FADE_MS), true);
    let elapsed = time::ticks() - start;
    assert_eq!(frames, 1, "reduce_motion must draw the final frame only");
    assert!(elapsed <= 1, "reduce_motion waited {} ticks", elapsed);
    assert_eq!(fb_console::chrome_alpha(), Some(255));
    assert_bars_final("with reduce_motion");
}
