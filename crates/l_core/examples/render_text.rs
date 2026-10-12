//! Host helper for scripts/qemu_shell_smoke.py: renders text lines exactly as
//! the kernel's framebuffer console draws them, so the smoke test can compare
//! them pixel by pixel with QEMU's `screendump`.
//!
//! Usage: `render_text COLS < lines.txt > lines.ppm`
//! Each input line (UTF-8, without the newline) becomes one text row of COLS
//! cells: the characters in AURORA `text` on AURORA `bg`, the rest blank, as
//! `l_core::render` draws them. The output is one binary PPM (P6, RGB, 8 bits
//! per channel), COLS*8 pixels wide and 16 pixels high per input line, which is
//! the format of QEMU's `screendump`.
//!
//! Only std and l_core; no unsafe.

use l_core::cell::{Cell, Grid};
use l_core::font::{GLYPH_H, GLYPH_W};
use l_core::render::{FbInfo, PixelFormat, render};
use l_core::theme::AURORA;
use std::io::{Read, Write};
use std::process::ExitCode;

fn main() -> ExitCode {
    let cols = match std::env::args().nth(1).map(|a| a.parse::<usize>()) {
        Some(Ok(cols)) if cols > 0 => cols,
        _ => {
            eprintln!("usage: render_text COLS < lines.txt > lines.ppm");
            return ExitCode::from(2);
        }
    };
    let mut input = String::new();
    if let Err(e) = std::io::stdin().read_to_string(&mut input) {
        eprintln!("render_text: cannot read stdin: {e}");
        return ExitCode::from(1);
    }
    let lines: Vec<&str> = input.lines().collect();
    let rows = lines.len();

    // The console's blank cell and text colours (src/fb_console.rs).
    let blank = Cell { ch: ' ', fg: AURORA.text, bg: AURORA.bg, bold: false };
    let mut grid = Grid::new(cols, rows, blank);
    for (row, line) in lines.iter().enumerate() {
        grid.put_str(0, row, line, AURORA.text, AURORA.bg);
    }
    let (width, height) = (cols * GLYPH_W, rows * GLYPH_H);
    let info = FbInfo { width, height, stride_px: width, bytes_per_px: 3, format: PixelFormat::Rgb };
    let mut pixels = vec![0u8; width * height * 3];
    render(&grid, 0..rows, &mut pixels, &info);

    let mut out = std::io::stdout().lock();
    let result = write!(out, "P6\n{width} {height}\n255\n")
        .and_then(|()| out.write_all(&pixels))
        .and_then(|()| out.flush());
    if let Err(e) = result {
        eprintln!("render_text: cannot write output: {e}");
        return ExitCode::from(1);
    }
    ExitCode::SUCCESS
}
