//! Small time based helpers shared by the animated parts of the UI.
//!
//! Everything here is a pure function of a duration, so a view can animate without
//! keeping any state of its own: pass in how long the thing has been on screen and
//! get back what to draw this frame. Animations that are driven by playback
//! position (the elapsed time of the current track) also reset themselves at a
//! track change and freeze while paused, which is what you want in both cases.

use std::sync::{Arc, RwLock, Weak};
use std::thread;
use std::time::{Duration, Instant};

use cursive::theme::Color;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::events::EventManager;
use crate::spotify::{PlayerEvent, Spotify};

/// Frames per second the animated views run at, unless `visualizer_fps` says
/// otherwise. Fast enough to look fluid, slow enough to stay cheap.
pub const DEFAULT_FPS: u32 = 20;
/// Bounds for `visualizer_fps`, so a typo cannot melt a CPU or stall the animation.
pub const FPS_RANGE: std::ops::RangeInclusive<u32> = 1..=60;
/// How often the animation thread re-checks an idle view.
const IDLE_INTERVAL: Duration = Duration::from_millis(250);
/// A view drawn this recently is assumed to still be on screen.
const VISIBLE_FOR: Duration = Duration::from_millis(500);
/// How long frames keep being drawn after playback stops, so a view can settle.
const SETTLE_FOR: Duration = Duration::from_millis(900);

/// Drives the visualizer while it is on screen.
///
/// The rest of ncspot only redraws every few hundred milliseconds, which is far too
/// coarse for an animation, so this asks the event loop for extra frames — but only
/// while the view is actually being drawn and something is playing, so a background
/// tab never costs anything.
#[derive(Default)]
pub struct Animator {
    last_draw: RwLock<Option<Instant>>,
}

impl Animator {
    /// Start animating at `fps`. Zero frames per second means the user turned the
    /// animation off: the band still draws, it just never asks for a frame.
    pub fn spawn(events: EventManager, spotify: Spotify, fps: u32) -> Arc<Self> {
        let animator = Arc::new(Self::default());
        if fps == 0 {
            return animator;
        }

        let frame = Duration::from_secs_f64(
            1.0 / f64::from(fps.clamp(*FPS_RANGE.start(), *FPS_RANGE.end())),
        );
        let handle: Weak<Self> = Arc::downgrade(&animator);

        thread::spawn(move || {
            let mut last_playing: Option<Instant> = None;

            // Stop once the view is gone, or once the event loop has shut down.
            while let Some(animator) = handle.upgrade() {
                let visible = animator.is_visible();
                drop(animator);

                let playing = matches!(spotify.get_current_status(), PlayerEvent::Playing(_));
                if playing {
                    last_playing = Some(Instant::now());
                }
                // Keep drawing for a moment after playback stops, so the band eases
                // down to its idle shape instead of freezing mid fall.
                let settling = last_playing.is_some_and(|at| at.elapsed() < SETTLE_FOR);

                if !visible || !(playing || settling) {
                    thread::sleep(IDLE_INTERVAL);
                    continue;
                }
                if !events.try_trigger() {
                    break;
                }
                thread::sleep(frame);
            }
        });

        animator
    }

    pub fn mark_drawn(&self) {
        *self.last_draw.write().unwrap() = Some(Instant::now());
    }

    pub fn is_visible(&self) -> bool {
        self.last_draw
            .read()
            .unwrap()
            .is_some_and(|at| at.elapsed() < VISIBLE_FOR)
    }
}

/// How long a scrolling line rests at each end before it moves on, in seconds.
/// Long enough to read the start of a title before it slides away.
const MARQUEE_HOLD: f64 = 2.0;
/// How fast a scrolling line travels, in cells per second. Slow enough to read.
const MARQUEE_SPEED: f64 = 4.0;

/// `colour` moved `amount` of the way towards white, hue intact. Colours that are
/// not given as components are left alone, since there is nothing to brighten.
pub fn lift(colour: Color, amount: f32) -> Color {
    if amount <= 0.0 {
        return colour;
    }
    let towards = |channel: u8, ceiling: f32| {
        (channel as f32 + (ceiling - channel as f32) * amount).round() as u8
    };
    match colour {
        Color::Rgb(r, g, b) => Color::Rgb(towards(r, 255.0), towards(g, 255.0), towards(b, 255.0)),
        Color::RgbLowRes(r, g, b) => {
            Color::RgbLowRes(towards(r, 5.0), towards(g, 5.0), towards(b, 5.0))
        }
        other => other,
    }
}

/// `amount` of the way from `from` to `to`, used to fade text in out of the
/// background and to shade a bar along its length.
///
/// A terminal palette colour has no components to interpolate, so a blend that
/// involves one snaps to `to` rather than guessing: on those terminals the fade is
/// simply absent instead of wrong.
pub fn blend(from: Color, to: Color, amount: f32) -> Color {
    let amount = amount.clamp(0.0, 1.0);
    let mix = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * amount).round() as u8;
    match (from, to) {
        (Color::Rgb(r1, g1, b1), Color::Rgb(r2, g2, b2)) => {
            Color::Rgb(mix(r1, r2), mix(g1, g2), mix(b1, b2))
        }
        (Color::RgbLowRes(r1, g1, b1), Color::RgbLowRes(r2, g2, b2)) => {
            Color::RgbLowRes(mix(r1, r2), mix(g1, g2), mix(b1, b2))
        }
        _ => to,
    }
}

/// Whether a colour has components to interpolate. A terminal palette colour does
/// not, so callers that want a gradient need another way to shade on those
/// themes: the terminal's own dim attribute, usually.
pub fn blendable(colour: Color) -> bool {
    matches!(colour, Color::Rgb(..) | Color::RgbLowRes(..))
}

/// How far into a fade of `length` `phase` is, as 0.0 to 1.0, eased so the fade
/// arrives gently rather than stopping dead.
pub fn fade(phase: Duration, length: Duration) -> f32 {
    if length.is_zero() {
        return 1.0;
    }
    let t = (phase.as_secs_f32() / length.as_secs_f32()).clamp(0.0, 1.0);
    // Smoothstep: no sudden start, no sudden stop.
    t * t * (3.0 - 2.0 * t)
}

/// The same fade, delayed by `after`, so a stack of lines can arrive in order.
pub fn staggered_fade(phase: Duration, length: Duration, after: Duration) -> f32 {
    fade(phase.saturating_sub(after), length)
}

/// A `width` wide window onto `text`, scrolling when the text is too long to fit.
///
/// The window rests at the start, travels to the end, rests there and travels
/// back, so a long title reads as a line that scans rather than one that jumps.
/// Text that already fits is returned untouched and never moves.
pub fn marquee(text: &str, width: usize, phase: Duration) -> String {
    if width == 0 {
        return String::new();
    }
    let overflow = text.width().saturating_sub(width);
    if overflow == 0 {
        return text.to_string();
    }

    let travel = overflow as f64 / MARQUEE_SPEED;
    let cycle = 2.0 * (MARQUEE_HOLD + travel);
    let t = phase.as_secs_f64() % cycle;
    let offset = if t < MARQUEE_HOLD {
        0.0
    } else if t < MARQUEE_HOLD + travel {
        (t - MARQUEE_HOLD) * MARQUEE_SPEED
    } else if t < 2.0 * MARQUEE_HOLD + travel {
        overflow as f64
    } else {
        overflow as f64 - (t - 2.0 * MARQUEE_HOLD - travel) * MARQUEE_SPEED
    };

    window(text, (offset.round() as usize).min(overflow), width)
}

/// The `width` cells of `text` starting at column `start`. A double width
/// character cut in half by either edge shows as a blank, so the window is always
/// exactly as wide as it claims to be.
fn window(text: &str, start: usize, width: usize) -> String {
    let mut out = String::new();
    let mut column = 0;
    let mut used = 0;

    for character in text.chars() {
        if used >= width {
            break;
        }
        let cell_width = character.width().unwrap_or(0);
        if column + cell_width <= start {
            column += cell_width;
            continue;
        }
        if column < start || used + cell_width > width {
            // Straddles an edge: show the visible half as a blank.
            let visible = if column < start {
                (column + cell_width) - start
            } else {
                width - used
            };
            for _ in 0..visible.min(width - used) {
                out.push(' ');
                used += 1;
            }
            column += cell_width;
            continue;
        }
        out.push(character);
        used += cell_width;
        column += cell_width;
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(seconds: f64) -> Duration {
        Duration::from_secs_f64(seconds)
    }

    #[test]
    fn text_that_fits_never_scrolls() {
        for seconds in [0.0, 1.0, 5.0, 60.0] {
            assert_eq!(marquee("short", 10, at(seconds)), "short");
        }
    }

    #[test]
    fn a_long_line_holds_scrolls_and_comes_back() {
        let text = "a very long track title indeed";
        let width = 10;
        let start = marquee(text, width, at(0.0));
        assert_eq!(start, "a very lon");
        // Still resting at the start.
        assert_eq!(marquee(text, width, at(MARQUEE_HOLD - 0.1)), start);

        let moved = marquee(text, width, at(MARQUEE_HOLD + 1.0));
        assert_ne!(moved, start);
        assert_eq!(moved.width(), width);

        // At the far end the last cells of the text are visible.
        let overflow = text.width() - width;
        let end = marquee(
            text,
            width,
            at(MARQUEE_HOLD + overflow as f64 / MARQUEE_SPEED),
        );
        assert_eq!(end, "itle indeed"[1..].to_string());

        // And the cycle returns to where it started.
        let cycle = 2.0 * (MARQUEE_HOLD + overflow as f64 / MARQUEE_SPEED);
        assert_eq!(marquee(text, width, at(cycle)), start);
    }

    #[test]
    fn every_window_is_exactly_as_wide_as_asked() {
        // Double width characters, so every window has an edge to cut through.
        let text = "日本語のタイトルはここにあります";
        for start in 0..=(text.width() - 7) {
            assert_eq!(window(text, start, 7).width(), 7);
        }
    }

    #[test]
    fn a_blend_moves_between_its_two_ends() {
        let black = Color::Rgb(0, 0, 0);
        let white = Color::Rgb(255, 255, 255);
        assert_eq!(blend(black, white, 0.0), black);
        assert_eq!(blend(black, white, 1.0), white);
        assert_eq!(blend(black, white, 0.5), Color::Rgb(128, 128, 128));
        // Out of range amounts are clamped rather than overshooting.
        assert_eq!(blend(black, white, 2.0), white);
        // A palette colour cannot be interpolated, so the target wins.
        assert_eq!(blend(Color::TerminalDefault, white, 0.5), white);
    }

    #[test]
    fn a_fade_starts_empty_ends_full_and_can_be_delayed() {
        let length = Duration::from_millis(500);
        assert_eq!(fade(Duration::ZERO, length), 0.0);
        assert_eq!(fade(length, length), 1.0);
        assert!(fade(Duration::from_millis(250), length) > 0.4);
        assert_eq!(
            staggered_fade(Duration::from_millis(100), length, length),
            0.0
        );
        assert_eq!(fade(Duration::ZERO, Duration::ZERO), 1.0);
    }
}
