//! The colour the whole interface is currently tinted with.
//!
//! The now playing card already takes its highlight from the cover art; this
//! carries that one colour to everything else that wants it — the statusbar bar,
//! the playing row in a list — and crossfades it when the track changes, so the
//! app takes on the new album's colour rather than snapping to it.
//!
//! It is process wide on purpose: the alternative is threading a colour through
//! every view constructor for something there is only ever one of.

use std::sync::{OnceLock, RwLock};
use std::time::{Duration, Instant};

use cursive::theme::Color;

use crate::events::EventManager;
use crate::ui::anim::{blend, fade};

/// How long the tint takes to travel from the old album's colour to the new one.
const CROSSFADE: Duration = Duration::from_millis(700);
/// Redraws asked for while the crossfade runs, for the case where nothing else is
/// animating: a track changed while playback is paused, say.
const FADE_FRAMES: u32 = 14;

#[derive(Default)]
struct State {
    /// The cover being followed, so the same one is not fetched twice.
    url: Option<String>,
    /// Where the crossfade started, or `None` to start from the caller's theme.
    from: Option<Color>,
    /// Where it is heading, or `None` for a cover with no colour worth using.
    to: Option<Color>,
    changed_at: Option<Instant>,
}

fn state() -> &'static RwLock<State> {
    static STATE: OnceLock<RwLock<State>> = OnceLock::new();
    STATE.get_or_init(RwLock::default)
}

/// The tint right now, part way through whatever crossfade is running.
///
/// `fallback` is the colour to use where the cover has none of its own, which is
/// the caller's own theme colour, so a cover that yields nothing changes nothing.
pub fn current(fallback: Color) -> Color {
    let state = state().read().unwrap();
    let to = state.to.unwrap_or(fallback);
    let Some(changed_at) = state.changed_at else {
        return to;
    };
    let from = state.from.unwrap_or(fallback);
    blend(from, to, fade(changed_at.elapsed(), CROSSFADE))
}

/// Note the cover of whatever is playing now.
///
/// Cheap to call on every draw: it only does anything when the cover changes, and
/// the colour is worked out on a background thread.
pub fn follow(url: Option<&str>, events: &EventManager) {
    {
        let state = state().read().unwrap();
        if state.url.as_deref() == url {
            return;
        }
    }

    // Start the crossfade from whatever is on screen right now, so a change that
    // lands mid fade carries on from there instead of jumping back.
    let midway = {
        let state = state().read().unwrap();
        match (state.from, state.to, state.changed_at) {
            (from, to, Some(changed_at)) => from
                .zip(to)
                .map(|(from, to)| blend(from, to, fade(changed_at.elapsed(), CROSSFADE)))
                .or(to),
            (_, to, None) => to,
        }
    };

    {
        let mut state = state().write().unwrap();
        state.url = url.map(str::to_string);
        state.from = midway;
        state.to = None;
        state.changed_at = Some(Instant::now());
    }

    let Some(url) = url else {
        return;
    };
    resolve(url.to_string(), events.clone());
}

/// Work out the colour of a cover and, if it is still the one being followed,
/// start the crossfade towards it.
#[cfg(feature = "album_art")]
fn resolve(url: String, events: EventManager) {
    std::thread::spawn(move || {
        let colour = crate::ui::album_art::accent_for(&url);
        {
            let mut state = state().write().unwrap();
            // A quick skip through a few tracks can land these out of order.
            if state.url.as_deref() != Some(url.as_str()) {
                return;
            }
            state.to = colour;
            state.changed_at = Some(Instant::now());
        }
        for _ in 0..FADE_FRAMES {
            if !events.try_trigger() {
                break;
            }
            std::thread::sleep(CROSSFADE / FADE_FRAMES);
        }
    });
}

/// Without album art there is no cover to take a colour from, so everything keeps
/// the colours its theme gave it.
#[cfg(not(feature = "album_art"))]
fn resolve(_url: String, _events: EventManager) {}

#[cfg(test)]
mod tests {
    use super::*;

    fn reset(from: Option<Color>, to: Option<Color>, changed_at: Option<Instant>) {
        let mut state = state().write().unwrap();
        state.url = None;
        state.from = from;
        state.to = to;
        state.changed_at = changed_at;
    }

    #[test]
    fn a_cover_with_no_colour_leaves_the_theme_alone() {
        let theme = Color::Rgb(10, 20, 30);
        reset(None, None, None);
        assert_eq!(current(theme), theme);
    }

    #[test]
    fn the_tint_travels_from_the_old_colour_to_the_new_one() {
        let theme = Color::Rgb(0, 0, 0);
        let old = Color::Rgb(0, 0, 0);
        let new = Color::Rgb(255, 255, 255);

        // Just changed: still the old colour.
        reset(Some(old), Some(new), Some(Instant::now()));
        assert_eq!(current(theme), old);

        // Long finished: fully the new one.
        reset(
            Some(old),
            Some(new),
            Instant::now().checked_sub(CROSSFADE * 2),
        );
        assert_eq!(current(theme), new);
    }
}
