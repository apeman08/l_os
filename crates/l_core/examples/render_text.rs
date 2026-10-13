//! Host helper for scripts/qemu_shell_smoke.py: renders text lines exactly as
//! the kernel's framebuffer console draws them, so the smoke test can compare
//! them pixel by pixel with QEMU's `screendump`.
//!
//! Usage: `render_text COLS [--status LEFT RIGHT] [--hint KEY DESC]... < lines.txt > lines.ppm`
//! Each input line (UTF-8, without the newline) becomes one text row of COLS
//! cells: the characters in AURORA `text` on AURORA `bg`, the rest blank, as
//! `l_core::render` draws them. With `--status`, a status bar
//! (`l_core::widgets::status_bar`) is drawn above the lines; with one or more
//! `--hint` (at most three, enforced by `HintBar::new`), a hint bar below
//! them; both in the final AURORA colours, like the kernel console's chrome. The output is one binary PPM (P6, RGB, 8 bits
//! per channel), COLS*8 pixels wide and 16 pixels high per input line, which is
//! the format of QEMU's `screendump`.
//!
//! Only std and l_core; no unsafe.

use l_core::cell::{Cell, Grid};
use l_core::font::{GLYPH_H, GLYPH_W};
use l_core::render::{FbInfo, PixelFormat, render};
use l_core::theme::AURORA;
use l_core::widgets::{HintBar, status_bar};
use std::io::{Read, Write};
use std::process::ExitCode;

const USAGE: &str = "usage: render_text COLS [--status LEFT RIGHT] [--hint KEY DESC]... < lines.txt > lines.ppm";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cols = match args.first().map(|a| a.parse::<usize>()) {
        Some(Ok(cols)) if cols > 0 => cols,
        _ => {
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        }
    };
    let mut status: Option<(&str, &str)> = None;
    let mut hints: Vec<(&str, &str)> = Vec::new();
    let mut rest = &args[1..];
    while let Some(flag) = rest.first() {
        match (flag.as_str(), rest.get(1), rest.get(2)) {
            ("--status", Some(left), Some(right)) => status = Some((left, right)),
            ("--hint", Some(key), Some(desc)) => hints.push((key, desc)),
            _ => {
                eprintln!("{USAGE}");
                return ExitCode::from(2);
            }
        }
        rest = &rest[3..];
    }
    let hint_bar = match HintBar::new(&hints) {
        Ok(bar) => bar,
        Err(_) => {
            eprintln!("render_text: more than three hints");
            return ExitCode::from(2);
        }
    };
    let mut input = String::new();
    if let Err(e) = std::io::stdin().read_to_string(&mut input) {
        eprintln!("render_text: cannot read stdin: {e}");
        return ExitCode::from(1);
    }
    let lines: Vec<&str> = input.lines().collect();
    let first = usize::from(status.is_some());
    let rows = first + lines.len() + usize::from(!hints.is_empty());

    // The console's blank cell and text colours (src/fb_console.rs).
    let blank = Cell { ch: ' ', fg: AURORA.text, bg: AURORA.bg, bold: false };
    let mut grid = Grid::new(cols, rows, blank);
    if let Some((left, right)) = status {
        status_bar(&mut grid, 0, left, right, &AURORA);
    }
    for (i, line) in lines.iter().enumerate() {
        grid.put_str(0, first + i, line, AURORA.text, AURORA.bg);
    }
    if !hints.is_empty() {
        hint_bar.draw(&mut grid, rows - 1, &AURORA);
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
