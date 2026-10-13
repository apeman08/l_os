//! Animation primitives (pure math): easing, colour blending, fades.
//!
//! Times are timer ticks (`crate::time::TICK_HZ` per second). Only `+ - * /`
//! and comparisons on `f32` are used, so this also builds for the soft-float
//! kernel target without libm.

use crate::cell::Rgb;
use crate::theme::Theme;

/// Cubic ease-out: fast start, gentle stop. `t` is clamped to `0.0..=1.0`
/// (NaN counts as 0.0); `ease_out_cubic(0.0) == 0.0`, `ease_out_cubic(1.0) == 1.0`.
pub fn ease_out_cubic(t: f32) -> f32 {
    // `!(t > 0.0)` also catches NaN.
    if !(t > 0.0) {
        return 0.0;
    }
    if t >= 1.0 {
        return 1.0;
    }
    let u = 1.0 - t;
    1.0 - u * u * u
}

/// Mix two colours: `alpha == 0` gives exactly `a`, `alpha == 255` exactly `b`.
pub fn blend(a: Rgb, b: Rgb, alpha: u8) -> Rgb {
    let mix = |x: u8, y: u8| -> u8 {
        let w = alpha as u32;
        // Rounded weighted mean; at most 255 * 255 + 127, so it fits.
        ((x as u32 * (255 - w) + y as u32 * w + 127) / 255) as u8
    };
    Rgb(mix(a.0, b.0), mix(a.1, b.1), mix(a.2, b.2))
}

/// Every colour of `theme` blended from `theme.bg` towards itself with
/// [`blend`]: `alpha == 0` turns everything into the background (invisible),
/// `alpha == 255` gives exactly `theme`. Used to fade UI chrome in.
pub fn fade_theme(theme: &Theme, alpha: u8) -> Theme {
    let f = |c: Rgb| blend(theme.bg, c, alpha);
    Theme {
        bg: theme.bg,
        surface: f(theme.surface),
        text: f(theme.text),
        muted: f(theme.muted),
        accent: f(theme.accent),
        ok: f(theme.ok),
        warn: f(theme.warn),
        err: f(theme.err),
    }
}

/// A fade-in starting at tick `start` and lasting `duration` ticks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fade {
    pub start: u64,
    pub duration: u64,
}

impl Fade {
    /// Opacity at tick `now`: 0 up to `start`, eased up to 255 at
    /// `start + duration`, 255 afterwards. With `reduce_motion` (or a zero
    /// duration) the target is shown at once: always 255.
    pub fn alpha(&self, now: u64, reduce_motion: bool) -> u8 {
        if reduce_motion || self.duration == 0 {
            return 255;
        }
        if now <= self.start {
            return 0;
        }
        let elapsed = now - self.start;
        if elapsed >= self.duration {
            return 255;
        }
        let eased = ease_out_cubic(elapsed as f32 / self.duration as f32);
        // Round to nearest; `eased` is within 0.0..=1.0, so this stays <= 255.
        let a = eased * 255.0 + 0.5;
        if a >= 255.0 { 255 } else { a as u8 }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ease_out_cubic_endpoints_and_monotonic() {
        assert_eq!(ease_out_cubic(0.0), 0.0);
        assert_eq!(ease_out_cubic(1.0), 1.0);
        // Clamped outside the unit interval; NaN is treated as the start.
        assert_eq!(ease_out_cubic(-0.5), 0.0);
        assert_eq!(ease_out_cubic(2.0), 1.0);
        assert_eq!(ease_out_cubic(f32::NAN), 0.0);
        // Ease-out: ahead of linear in the middle (0.5 -> 0.875).
        assert!((ease_out_cubic(0.5) - 0.875).abs() < 1e-6);
        let mut prev = ease_out_cubic(0.0);
        for i in 1..=1000 {
            let v = ease_out_cubic(i as f32 / 1000.0);
            assert!(v >= prev, "not monotonic at step {}: {} < {}", i, v, prev);
            assert!((0.0..=1.0).contains(&v));
            prev = v;
        }
    }

    const A: Rgb = Rgb(10, 200, 33);
    const B: Rgb = Rgb(250, 0, 128);

    #[test]
    fn blend_zero_is_a() {
        assert_eq!(blend(A, B, 0), A);
        assert_eq!(blend(Rgb(255, 255, 255), Rgb(0, 0, 0), 0), Rgb(255, 255, 255));
    }

    #[test]
    fn blend_255_is_b() {
        assert_eq!(blend(A, B, 255), B);
        assert_eq!(blend(Rgb(0, 0, 0), Rgb(255, 255, 255), 255), Rgb(255, 255, 255));
        // Halfway lands between the two, rounded.
        assert_eq!(blend(Rgb(0, 0, 0), Rgb(255, 255, 255), 128), Rgb(128, 128, 128));
    }

    #[test]
    fn fade_alpha_ramps_and_clamps() {
        let f = Fade { start: 100, duration: 50 };
        assert_eq!(f.alpha(0, false), 0);
        assert_eq!(f.alpha(100, false), 0);
        assert_eq!(f.alpha(150, false), 255);
        assert_eq!(f.alpha(10_000, false), 255);
        assert_eq!(f.alpha(u64::MAX, false), 255);
        let mid = f.alpha(125, false);
        assert!(mid > 128 && mid < 255, "eased midpoint {}", mid);
        let mut prev = 0;
        for now in 100..=150 {
            let a = f.alpha(now, false);
            assert!(a >= prev, "fade went backwards at tick {}", now);
            prev = a;
        }
        // A zero-length fade is already complete; no division by zero.
        assert_eq!(Fade { start: 5, duration: 0 }.alpha(5, false), 255);
        // start + duration overflowing u64 must not panic.
        assert_eq!(Fade { start: u64::MAX - 1, duration: u64::MAX }.alpha(0, false), 0);
    }

    #[test]
    fn fade_theme_255_is_the_theme_and_0_is_the_background() {
        use crate::theme::AURORA;
        assert_eq!(fade_theme(&AURORA, 255), AURORA);
        let t = fade_theme(&AURORA, 0);
        for c in [t.bg, t.surface, t.text, t.muted, t.accent, t.ok, t.warn, t.err] {
            assert_eq!(c, AURORA.bg);
        }
        // In between every colour is the blend from bg, field by field.
        let m = fade_theme(&AURORA, 100);
        assert_eq!(m.text, blend(AURORA.bg, AURORA.text, 100));
        assert_eq!(m.surface, blend(AURORA.bg, AURORA.surface, 100));
        assert_eq!(m.accent, blend(AURORA.bg, AURORA.accent, 100));
        assert_eq!(m.muted, blend(AURORA.bg, AURORA.muted, 100));
        assert_ne!(m.text, AURORA.text);
        assert_ne!(m.text, AURORA.bg);
    }

    #[test]
    fn fade_reduce_motion_is_immediate() {
        let f = Fade { start: 100, duration: 50 };
        for now in [0, 99, 100, 120, 150, 1000] {
            assert_eq!(f.alpha(now, true), 255);
        }
    }
}
