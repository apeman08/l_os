//! The Aurora chrome around the console: what the status bar and the hint bar
//! say, and the boot fade-in. The drawing itself is in [`crate::fb_console`]
//! (`enable_chrome`, `fade_in_chrome`) with the `l_core` widgets.
//!
//! Turned on only by the real kernel entry (`src/main.rs`); test kernels keep
//! the plain full-screen console unless they call [`show_chrome`].

use l_core::widgets::HintBar;

/// Status bar, left: the system's name.
pub const STATUS_LEFT: &str = "Larp OS";
/// Status bar, right: the system state, "amnesisch: ja" only when every
/// condition of spec E7 holds ([`crate::wipe::amnesia`], `Amnesia::label`).
/// Read when the chrome is turned on, i.e. after the boot stick was
/// identified and sealed.
pub fn status_right() -> &'static str {
    crate::wipe::amnesia().label()
}

/// Hint bar: only keys the shell really handles today (`user/src/bin/shell.rs`
/// with the `l_core::shell` line editor): Enter submits the line, Backspace
/// erases the last character, `help` lists the commands. Tab completion and a
/// command palette do not exist, so they are not advertised.
pub const HINTS: [(&str, &str); 3] = [("Enter", "ausführen"), ("Backspace", "löschen"), ("help", "Befehle")];

/// Length of the boot fade-in of the chrome.
pub const BOOT_FADE_MS: u64 = 400;

/// Skip animations and show their end state at once. Off by default; there
/// is no runtime setting yet.
pub const REDUCE_MOTION: bool = false;

/// The hint bar; `HintBar::new` enforces the limit of three hints.
pub fn hint_bar() -> HintBar {
    HintBar::new(&HINTS).expect("at most three hints")
}

/// Turn the chrome on and fade it in (blocking, `hlt` between frames, for
/// [`BOOT_FADE_MS`] unless [`REDUCE_MOTION`]). Returns the number of frames
/// drawn, or `None` without a console (or one too small for the bars).
pub fn show_chrome() -> Option<u32> {
    if !crate::fb_console::enable_chrome(STATUS_LEFT, status_right(), hint_bar()) {
        return None;
    }
    Some(crate::fb_console::fade_in_chrome(crate::time::ticks_from_ms(BOOT_FADE_MS), REDUCE_MOTION))
}
