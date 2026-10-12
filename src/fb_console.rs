//! Text console on the UEFI framebuffer ("Aurora" terminal).
//!
//! The text lives in an [`l_core::cell::Grid`] (one cell per 8x16 glyph); the
//! pixels are drawn by the pure renderer [`l_core::render::render`], in the
//! colours of [`l_core::theme::AURORA`]. Geometry (width, height, stride,
//! bytes per pixel, Rgb/Bgr) always comes from the firmware's description.
//!
//! Terminal semantics: the cursor starts at the top left; `\n` moves to the
//! start of the next row; a full row wraps; at the bottom the screen scrolls up
//! by one row. Backspace erases the character left of the cursor (not past the
//! start of the row); [`clear_screen`] blanks everything and homes the cursor.
//! Control characters are never interpreted: they are shown as the replacement
//! glyph. Any other character is drawn with the font (which falls back to the
//! replacement glyph for characters it does not have).
//!
//! Cost: after each print call only the rows that changed are rendered. A
//! scroll moves the already drawn pixels up with one memory copy per print call
//! (however many rows it scrolled) and renders only the new rows, so long
//! outputs cost O(lines), not a full-screen redraw per line or per character.
//!
//! With the cargo feature `serial-console` (on by default) every console write
//! is also mirrored to COM1.

use core::fmt;
use core::ops::Range;
use l_core::cell::{Cell, Grid, Rgb};
use l_core::font::{GLYPH_H, GLYPH_W, glyph};
use l_core::render::{FbInfo, PixelFormat, grid_size_for, render};
use l_core::theme::AURORA;
use spin::{Mutex, Once};
use x86_64::instructions::interrupts;

/// Shown for control characters; the font has no glyph for it, so it draws
/// its replacement glyph.
const REPLACEMENT: char = char::REPLACEMENT_CHARACTER;

/// Where the framebuffer is. Only an address: whoever turns it into a slice
/// must hold the [`CONSOLE`] lock (the panic path in [`emergency_print`] is
/// the one documented exception).
#[derive(Clone, Copy)]
struct Screen {
    base: *mut u8,
    len: usize,
    info: FbInfo,
}

// SAFETY: `Screen` is a plain description of the framebuffer memory (which
// lives for the whole run and is not tied to any thread); the rules for
// dereferencing it are above and are followed by every user in this module.
unsafe impl Send for Screen {}
// SAFETY: as for `Send`; shared `&Screen` only gives out the address.
unsafe impl Sync for Screen {}

/// Set once by [`init`]; read by the lock-free panic path.
static SCREEN: Once<Screen> = Once::new();

/// The console, `None` until [`init`] (no framebuffer, or before the heap).
pub static CONSOLE: Mutex<Option<Console>> = Mutex::new(None);

pub struct Console {
    grid: Grid,
    screen: Screen,
    col: usize,
    row: usize,
    fg: Rgb,
    /// Rows (in current grid coordinates) not yet drawn.
    pending: Option<Range<usize>>,
    /// Rows scrolled since the last flush; the drawn pixels are moved up by
    /// this many text rows before the pending rows are drawn.
    scrolled: usize,
    rendered_rows: u64,
}

fn blank(fg: Rgb) -> Cell {
    Cell { ch: ' ', fg, bg: AURORA.bg, bold: false }
}

fn merge(a: Option<Range<usize>>, b: Option<Range<usize>>) -> Option<Range<usize>> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.start.min(b.start)..a.end.max(b.end)),
        (a, None) => a,
        (None, b) => b,
    }
}

impl Console {
    fn new(screen: Screen) -> Option<Console> {
        let (cols, rows) = grid_size_for(&screen.info);
        if cols == 0 || rows == 0 {
            return None;
        }
        let mut console = Console {
            grid: Grid::new(cols, rows, blank(AURORA.text)),
            screen,
            col: 0,
            row: 0,
            fg: AURORA.text,
            pending: Some(0..rows),
            scrolled: 0,
            rendered_rows: 0,
        };
        console.paint_margin();
        console.flush();
        Some(console)
    }

    /// The framebuffer as a slice.
    fn fb(&mut self) -> &mut [u8] {
        // SAFETY: `base`/`len` come from the `&'static mut [u8]` handed to
        // `init`, so the memory is valid for the whole run. The slice lives
        // only as long as `&mut self`, i.e. while the CONSOLE lock is held, so
        // no other slice of it exists (except on the panic path, see
        // `paint_locked`).
        unsafe { core::slice::from_raw_parts_mut(self.screen.base, self.screen.len) }
    }

    /// Paints the pixels outside the grid (right/bottom remainder when the
    /// resolution is not a multiple of 8x16) in the background colour.
    fn paint_margin(&mut self) {
        let info = self.screen.info;
        let (gw, gh) = (self.grid.cols() * GLYPH_W, self.grid.rows() * GLYPH_H);
        let fb = self.fb();
        for y in 0..info.height {
            let x0 = if y < gh { gw } else { 0 };
            for x in x0..info.width {
                put_px(fb, &info, x, y, AURORA.bg);
            }
        }
    }

    fn mark(&mut self, rows: Range<usize>) {
        self.pending = merge(self.pending.take(), Some(rows));
    }

    fn write_str(&mut self, s: &str) {
        for c in s.chars() {
            self.write_char(c);
        }
    }

    fn write_char(&mut self, c: char) {
        if c == '\n' {
            self.new_line();
            return;
        }
        if self.col >= self.grid.cols() {
            self.new_line();
        }
        let ch = if c.is_control() { REPLACEMENT } else { c };
        let cell = Cell { ch, fg: self.fg, bg: AURORA.bg, bold: false };
        self.grid.set(self.col, self.row, cell);
        self.col += 1;
    }

    fn new_line(&mut self) {
        self.col = 0;
        if self.row + 1 < self.grid.rows() {
            self.row += 1;
            return;
        }
        // Remember what was dirty before the scroll (Grid::scroll_up marks
        // every row dirty, which would mean a full redraw).
        let dirty = self.grid.take_dirty();
        self.mark_opt(dirty);
        self.grid.scroll_up(1, blank(AURORA.text));
        let _ = self.grid.take_dirty();
        self.scrolled += 1;
        let last = self.grid.rows() - 1;
        self.pending = self
            .pending
            .take()
            .map(|r| r.start.saturating_sub(1)..r.end.saturating_sub(1))
            .filter(|r| !r.is_empty());
        self.mark(last..last + 1);
    }

    fn mark_opt(&mut self, rows: Option<Range<usize>>) {
        self.pending = merge(self.pending.take(), rows);
    }

    fn backspace(&mut self) {
        if self.col == 0 {
            return;
        }
        self.col -= 1;
        self.grid.set(self.col, self.row, blank(self.fg));
    }

    fn clear(&mut self) {
        for row in 0..self.grid.rows() {
            for col in 0..self.grid.cols() {
                self.grid.set(col, row, blank(AURORA.text));
            }
        }
        self.col = 0;
        self.row = 0;
    }

    /// Brings the pixels up to date with the grid: moves the drawn rows up by
    /// the number of rows scrolled, then renders only the rows that changed.
    fn flush(&mut self) {
        let dirty = self.grid.take_dirty();
        self.mark_opt(dirty);
        let rows = self.grid.rows();
        let scrolled = core::mem::take(&mut self.scrolled);
        let everything = self.pending.as_ref().is_some_and(|r| r.start == 0 && r.end >= rows);
        if scrolled > 0 && !everything {
            let info = self.screen.info;
            let row_bytes = GLYPH_H
                .checked_mul(info.stride_px)
                .and_then(|v| v.checked_mul(info.bytes_per_px));
            let fits = row_bytes.and_then(|b| b.checked_mul(rows)).is_some_and(|end| end <= self.screen.len);
            match row_bytes {
                Some(row_bytes) if scrolled < rows && fits => {
                    // The rows that scrolled in at the bottom are pending
                    // (each scroll marked its new bottom row).
                    self.fb().copy_within(scrolled * row_bytes..rows * row_bytes, 0);
                }
                _ => self.mark(0..rows),
            }
        }
        if let Some(range) = self.pending.take() {
            let info = self.screen.info;
            let count = range.len() as u64;
            // SAFETY: as in `fb` (CONSOLE lock held through `&mut self`);
            // taken directly so that `self.grid` can be borrowed alongside.
            let fb = unsafe { core::slice::from_raw_parts_mut(self.screen.base, self.screen.len) };
            render(&self.grid, range, fb, &info);
            self.rendered_rows += count;
        }
    }
}

impl fmt::Write for Console {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        Console::write_str(self, s);
        Ok(())
    }
}

/// Writes one pixel at (`x`, `y`) in the screen's pixel format; skipped if it
/// is outside the buffer. Same byte layout as `l_core::render`.
fn put_px(fb: &mut [u8], info: &FbInfo, x: usize, y: usize, c: Rgb) {
    let (a, b, d) = match info.format {
        PixelFormat::Rgb => (c.0, c.1, c.2),
        PixelFormat::Bgr => (c.2, c.1, c.0),
    };
    if info.bytes_per_px != 3 && info.bytes_per_px != 4 {
        return;
    }
    let Some(idx) = y
        .checked_mul(info.stride_px)
        .and_then(|v| v.checked_add(x))
        .and_then(|v| v.checked_mul(info.bytes_per_px))
    else {
        return;
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

/// Take over the framebuffer. Called once from [`crate::init_memory`], after
/// the heap exists (the grid is heap-allocated); later calls are ignored.
/// Clears the screen to the theme background.
pub fn init(info: FbInfo, buffer: &'static mut [u8]) {
    let screen = Screen { base: buffer.as_mut_ptr(), len: buffer.len(), info };
    interrupts::without_interrupts(|| {
        let mut console = CONSOLE.lock();
        if console.is_some() || SCREEN.r#try().is_some() {
            return;
        }
        *console = Console::new(screen);
        if console.is_some() {
            SCREEN.call_once(|| screen);
        }
    });
}

#[macro_export]
macro_rules! print {
    ($($arg:tt)*) => ($crate::fb_console::_print(format_args!($($arg)*)));
}

#[macro_export]
macro_rules! println {
    () => ($crate::print!("\n"));
    ($($arg:tt)*) => ($crate::print!("{}\n", format_args!($($arg)*)));
}

#[doc(hidden)]
pub fn _print(args: fmt::Arguments) {
    use core::fmt::Write;
    interrupts::without_interrupts(|| {
        if let Some(console) = CONSOLE.lock().as_mut() {
            // Console::write_str never fails.
            let _ = console.write_fmt(args);
            console.flush();
        }
    });
    // Lock order: console, then serial; never nested the other way round.
    #[cfg(feature = "serial-console")]
    crate::serial::_print(args);
}

pub fn backspace() {
    interrupts::without_interrupts(|| {
        if let Some(console) = CONSOLE.lock().as_mut() {
            console.backspace();
            console.flush();
        }
    });
    // Terminal-style erase on the mirror.
    #[cfg(feature = "serial-console")]
    crate::serial::_print(format_args!("\x08 \x08"));
}

pub fn clear_screen() {
    interrupts::without_interrupts(|| {
        if let Some(console) = CONSOLE.lock().as_mut() {
            console.clear();
            console.flush();
        }
    });
    // ANSI "erase display, cursor home" on the mirror.
    #[cfg(feature = "serial-console")]
    crate::serial::_print(format_args!("\x1b[2J\x1b[H"));
}

/// Print from a panic path, on the screen and on COM1 (always, whatever the
/// `serial-console` feature says). Never blocks; see [`emergency_print_screen`].
pub fn emergency_print(args: fmt::Arguments) {
    emergency_print_screen(args);
    let were_enabled = interrupts::are_enabled();
    crate::serial::emergency_print(args);
    // serial::emergency_print turns interrupts off for good; a panic handler
    // wants that anyway, other callers get their state back.
    if were_enabled {
        interrupts::enable();
    }
}

/// Print from a panic path on the screen only, in the error colour. Never
/// blocks: if the console is free, the text goes through it as usual; if its
/// lock is held (the panic interrupted a console user), the text is painted
/// directly onto the bottom rows of the framebuffer without touching the
/// console's state, the heap or any lock.
pub fn emergency_print_screen(args: fmt::Arguments) {
    use core::fmt::Write;
    interrupts::without_interrupts(|| match CONSOLE.try_lock() {
        Some(mut guard) => {
            if let Some(console) = guard.as_mut() {
                let fg = core::mem::replace(&mut console.fg, AURORA.err);
                let _ = console.write_fmt(args);
                console.fg = fg;
                console.flush();
            }
        }
        None => paint_locked(args),
    });
}

/// A fixed-size text buffer for the panic path (no heap); excess is dropped.
struct PanicText {
    buf: [u8; 512],
    len: usize,
}

impl fmt::Write for PanicText {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for c in s.chars() {
            let mut tmp = [0u8; 4];
            let bytes = c.encode_utf8(&mut tmp).as_bytes();
            let Some(dest) = self.buf.get_mut(self.len..self.len + bytes.len()) else { break };
            dest.copy_from_slice(bytes);
            self.len += bytes.len();
        }
        Ok(())
    }
}

/// Paints `args` onto the last rows of the screen (wrapped at the grid width,
/// the last lines at the bottom), bypassing the locked console.
fn paint_locked(args: fmt::Arguments) {
    use core::fmt::Write;
    let Some(screen) = SCREEN.r#try() else { return };
    let mut text = PanicText { buf: [0; 512], len: 0 };
    let _ = text.write_fmt(args);
    let text = core::str::from_utf8(&text.buf[..text.len]).unwrap_or("");
    let text = text.strip_suffix('\n').unwrap_or(text);
    let info = screen.info;
    let (cols, rows) = grid_size_for(&info);
    if cols == 0 || rows == 0 {
        return;
    }
    // Count the (wrapped) lines first, then paint the last `rows` of them.
    let line_count = |text: &str| -> usize {
        text.split('\n').map(|l| l.chars().count().max(1).div_ceil(cols)).sum()
    };
    let total = line_count(text);
    let skip = total.saturating_sub(rows);
    let first_row = rows - (total - skip);
    // SAFETY: the framebuffer memory is valid for the whole run (see `fb`).
    // The CONSOLE lock is held by code that was interrupted by the panic and
    // never resumes (this runs with interrupts off on the only CPU), so no
    // console code writes to the buffer concurrently; if the holder was in
    // the middle of a flush, its half-drawn row is simply overdrawn here.
    let fb = unsafe { core::slice::from_raw_parts_mut(screen.base, screen.len) };
    let mut line = 0;
    for part in text.split('\n') {
        let mut chars = part.chars().peekable();
        loop {
            // One screen row: up to `cols` characters, padded with blanks.
            for col in 0..cols {
                let ch = match chars.next() {
                    Some(c) if c.is_control() => REPLACEMENT,
                    Some(c) => c,
                    None => ' ',
                };
                if line >= skip {
                    paint_cell(fb, &info, col, first_row + line - skip, ch, AURORA.err);
                }
            }
            line += 1;
            if chars.peek().is_none() {
                break;
            }
        }
    }
}

fn paint_cell(fb: &mut [u8], info: &FbInfo, col: usize, row: usize, ch: char, fg: Rgb) {
    for (gy, bits) in glyph(ch).iter().enumerate() {
        for gx in 0..GLYPH_W {
            let colour = if bits & (0x80 >> gx) != 0 { fg } else { AURORA.bg };
            put_px(fb, info, col * GLYPH_W + gx, row * GLYPH_H + gy, colour);
        }
    }
}

// ---- Introspection (used by tests/framebuffer.rs) -------------------------

/// Grid size in cells (columns, rows), or `None` without a console.
pub fn grid_size() -> Option<(usize, usize)> {
    with_console(|c| (c.grid.cols(), c.grid.rows()))
}

/// Cursor position (column, row).
pub fn cursor() -> Option<(usize, usize)> {
    with_console(|c| (c.col, c.row))
}

/// The character in a cell.
pub fn cell_at(col: usize, row: usize) -> Option<char> {
    with_console(|c| c.grid.get(col, row).map(|cell| cell.ch)).flatten()
}

/// Text rows rendered since boot (each row is GLYPH_H pixel lines).
pub fn rendered_rows() -> u64 {
    with_console(|c| c.rendered_rows).unwrap_or(0)
}

/// Read access to the framebuffer pixels, under the console lock.
pub fn with_framebuffer<R>(f: impl FnOnce(&FbInfo, &[u8]) -> R) -> Option<R> {
    interrupts::without_interrupts(|| {
        CONSOLE.lock().as_mut().map(|c| {
            let info = c.screen.info;
            f(&info, c.fb())
        })
    })
}

fn with_console<R>(f: impl FnOnce(&Console) -> R) -> Option<R> {
    interrupts::without_interrupts(|| CONSOLE.lock().as_ref().map(f))
}
