//! The overlay that says what a keypress just did, and what went wrong.
//!
//! Volume and seek are the two commands whose effect is a number in the corner of
//! a two line statusbar, which is no use at all from across the room. A key that
//! changes one of them raises a panel in the middle of the screen with the new
//! value drawn big, and the panel fades out a second later.
//!
//! The same panel carries notices. Without one, a failure has nowhere to go but the
//! log, which is only written when `--debug` was passed, so an ordinary run would
//! fail silently: a search that returned nothing, a library that could not be
//! reached, a token that expired. Anything the user would otherwise be left
//! guessing about goes through [`notify`].

use std::sync::{OnceLock, RwLock};
use std::time::{Duration, Instant};

use cursive::Printer;
use cursive::align::HAlign;
use cursive::theme::{ColorStyle, ColorType, Effect, PaletteColor};
use unicode_width::UnicodeWidthStr;

use crate::events::EventManager;
use crate::ui::accent;
use crate::ui::anim::{blend, blendable};
use crate::utils::ms_to_hms;

/// How long the panel stays up, including the fade at the end of it.
const LIFETIME: Duration = Duration::from_millis(1400);
/// The last part of the lifetime is spent fading out.
const FADE_FROM: f32 = 0.6;
/// Redraws asked for while the panel is up, so it fades and clears itself even
/// when nothing else on screen is moving.
const FRAMES: u32 = 18;
/// The panel's inner width, in cells. Wide enough for a bar worth looking at,
/// narrow enough for a small terminal.
const WIDTH: usize = 34;

/// What the panel is showing.
#[derive(Clone)]
pub enum Flash {
    /// A volume, 0 to 100.
    Volume(u16),
    /// A position in a track, both in milliseconds.
    Seek { elapsed: u128, duration: u32 },
    /// Something the user needs told, which has no bar under it.
    Notice(String),
}

impl Flash {
    fn label(&self) -> String {
        match self {
            Self::Notice(message) => message.clone(),
            Self::Volume(percent) => format!("volume {percent}%"),
            Self::Seek { elapsed, duration } => format!(
                "{} / {}",
                ms_to_hms((*elapsed).try_into().unwrap_or(u32::MAX)),
                ms_to_hms(*duration)
            ),
        }
    }

    /// How full the bar is drawn, 0 to 1.
    fn fraction(&self) -> f32 {
        match self {
            Self::Notice(_) => 0.0,
            Self::Volume(percent) => (*percent as f32 / 100.0).clamp(0.0, 1.0),
            Self::Seek { elapsed, duration } => {
                if *duration == 0 {
                    return 0.0;
                }
                (*elapsed as f32 / *duration as f32).clamp(0.0, 1.0)
            }
        }
    }
}

fn state() -> &'static RwLock<Option<(Flash, Instant)>> {
    static STATE: OnceLock<RwLock<Option<(Flash, Instant)>>> = OnceLock::new();
    STATE.get_or_init(RwLock::default)
}

/// The event loop to wake when a notice is raised.
///
/// Notices come from places with no view and no event manager to hand: a library
/// worker, a failed API call several layers down. The panel's state is already
/// process wide, so the way to reach the screen from those places is too.
fn notifier() -> &'static RwLock<Option<EventManager>> {
    static NOTIFIER: OnceLock<RwLock<Option<EventManager>>> = OnceLock::new();
    NOTIFIER.get_or_init(RwLock::default)
}

/// Let [`notify`] reach the screen. Called once, as the interface comes up.
pub fn attach(events: &EventManager) {
    *notifier().write().unwrap() = Some(events.clone());
}

/// Tell the user something, on the panel, for as long as a flash lasts.
///
/// Silently does nothing before the interface exists, which is the right thing:
/// startup failures are reported on stdout, where they can still be read.
pub fn notify(message: impl Into<String>) {
    let events = notifier().read().unwrap().clone();
    if let Some(events) = events {
        flash(Flash::Notice(message.into()), &events);
    }
}

/// Raise the panel, and keep the frames coming until it has faded away.
pub fn flash(flash: Flash, events: &EventManager) {
    let already_up = state()
        .read()
        .unwrap()
        .as_ref()
        .is_some_and(|(_, since)| since.elapsed() < LIFETIME);
    *state().write().unwrap() = Some((flash, Instant::now()));

    // Holding a volume key repeats the command; one ticker is enough for all of
    // them, since each keypress pushes the deadline out again.
    if already_up {
        events.trigger();
        return;
    }
    let events = events.clone();
    std::thread::spawn(move || {
        for _ in 0..FRAMES {
            if !events.try_trigger() {
                return;
            }
            std::thread::sleep(LIFETIME / FRAMES);
        }
        // One last frame to clear it off the screen.
        *state().write().unwrap() = None;
        events.try_trigger();
    });
}

/// Draw the panel over `printer`, if one is up. Cheap to call on every frame.
pub fn draw(printer: &Printer<'_, '_>) {
    let Some((flash, since)) = state().read().unwrap().clone() else {
        return;
    };
    let age = since.elapsed();
    if age >= LIFETIME {
        return;
    }
    let label = flash.label();
    // A notice is as wide as its message needs, up to what the terminal allows; the
    // bars keep their fixed width so a volume panel doesn't change size as it counts.
    let wanted = match flash {
        Flash::Notice(_) => WIDTH.max(label.width() + 2),
        _ => WIDTH,
    };
    let width = wanted.min(printer.size.x.saturating_sub(4));
    if width < 12 || printer.size.y < 7 {
        return;
    }

    // Fully lit for most of its life, then fading out.
    let life = age.as_secs_f32() / LIFETIME.as_secs_f32();
    let strength = if life < FADE_FROM {
        1.0
    } else {
        1.0 - (life - FADE_FROM) / (1.0 - FADE_FROM)
    };

    let background = printer.theme.palette[PaletteColor::Background];
    let theme = *printer.theme.palette.custom("statusbar_progress").unwrap();
    let accent = accent::current(theme);
    let primary = printer.theme.palette[PaletteColor::Primary];
    let faded = |colour| blend(background, colour, strength);
    // A theme with no background colour cannot be faded towards, so on those the
    // panel dims for the last stretch instead of dissolving.
    let dim = !blendable(background) && strength < 0.5;

    let left = (printer.size.x - width) / 2;
    let top = printer.size.y / 2 - 2;
    // A message too wide even for the whole terminal is cut rather than wrapped, so
    // the panel keeps its shape.
    let label = crate::utils::truncate_string(&label, width);
    let filled = (flash.fraction() * width as f32).round() as usize;

    let ink = |colour| ColorStyle::new(ColorType::Color(colour), ColorType::Color(background));
    let write = |printer: &Printer<'_, '_>, x: usize, y: usize, text: &str, style: ColorStyle| {
        printer.with_color(style, |printer| {
            if dim {
                printer.with_effect(Effect::Dim, |printer| printer.print((x, y), text));
            } else {
                printer.print((x, y), text);
            }
        });
    };

    // A box, so the panel reads as something on top of the screen rather than as
    // text that has landed in the middle of it.
    let border = ink(faded(accent));
    write(
        printer,
        left - 1,
        top - 1,
        &format!("\u{256d}{}\u{256e}", "\u{2500}".repeat(width)),
        border,
    );
    for row in 0..3 {
        write(printer, left - 1, top + row, "\u{2502}", border);
        write(printer, left + width, top + row, "\u{2502}", border);
        printer.with_color(ink(background), |printer| {
            printer.print_hline((left, top + row), width, " ");
        });
    }
    write(
        printer,
        left - 1,
        top + 3,
        &format!("\u{2570}{}\u{256f}", "\u{2500}".repeat(width)),
        border,
    );

    let x = left + HAlign::Center.get_offset(label.width(), width);
    if matches!(flash, Flash::Notice(_)) {
        // No bar under it, so the message sits in the middle of the panel rather
        // than at the top of it with two empty rows underneath.
        write(printer, x, top + 1, &label, ink(faded(primary)));
        return;
    }
    write(printer, x, top, &label, ink(faded(primary)));
    write(
        printer,
        left,
        top + 2,
        &"\u{2504}".repeat(width),
        ink(faded(primary)),
    );
    if filled > 0 {
        write(
            printer,
            left,
            top + 2,
            &"\u{2501}".repeat(filled.min(width)),
            ink(faded(accent)),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_volume_reads_as_a_percentage_and_fills_its_bar() {
        let flash = Flash::Volume(50);
        assert_eq!(flash.label(), "volume 50%");
        assert!((flash.fraction() - 0.5).abs() < f32::EPSILON);
        // Nothing sensible can overflow the bar.
        assert!((Flash::Volume(400).fraction() - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn a_notice_is_its_message_and_has_no_bar() {
        let flash = Flash::Notice("couldn't reach Spotify".to_string());
        assert_eq!(flash.label(), "couldn't reach Spotify");
        // Nothing is being measured, so nothing is filled in: a bar at zero would
        // read as a volume of nought rather than as no bar at all.
        assert_eq!(flash.fraction(), 0.0);
    }

    #[test]
    fn a_seek_reads_as_a_position_in_the_track() {
        let flash = Flash::Seek {
            elapsed: 62_000,
            duration: 124_000,
        };
        assert_eq!(flash.label(), "1:02 / 2:04");
        assert!((flash.fraction() - 0.5).abs() < f32::EPSILON);
        // A track with no length yet cannot say how far through it is.
        assert_eq!(
            Flash::Seek {
                elapsed: 1000,
                duration: 0
            }
            .fraction(),
            0.0
        );
    }
}
