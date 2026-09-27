//! The queue beside the now playing card: the track that just played, the one
//! playing, and what comes after it, in the order it will actually play.
//!
//! Positions here are places in the play order, not indices into the raw queue, so
//! a shuffled queue reads the way it will sound.

use cursive::Printer;
use cursive::theme::{ColorStyle, Effect};
use unicode_width::UnicodeWidthStr;

use crate::model::playable::Playable;

/// Space between the columns of a row.
const GAP: usize = 2;

/// The colours a panel is drawn in.
pub struct Styles {
    pub text: ColorStyle,
    pub quiet: ColorStyle,
    /// The playing row.
    pub current: ColorStyle,
    /// The row the keyboard cursor is on.
    pub selected: ColorStyle,
}

/// A row of the panel, as the queue handed it over.
pub struct Entry {
    /// Index into the raw queue, for playing or removing the item.
    pub index: usize,
    /// Place in the play order.
    pub position: usize,
    pub playable: Playable,
}

/// A row on screen and the queue index a click on it plays.
pub struct Hit {
    pub row: usize,
    pub index: usize,
}

/// The first play order position to show in `rows` rows of a `len` long queue.
///
/// The item before the playing one stays in view as context, and the list scrolls
/// only as far as it must to keep the cursor on screen.
pub fn window_start(
    current: Option<usize>,
    selected: Option<usize>,
    len: usize,
    rows: usize,
) -> usize {
    if rows == 0 || len <= rows {
        return 0;
    }
    let mut start = current.map_or(0, |current| current.saturating_sub(1));
    if let Some(selected) = selected {
        if selected < start {
            start = selected;
        } else if selected >= start + rows {
            start = selected + 1 - rows;
        }
    }
    start.min(len - rows)
}

/// Move a cursor `delta` places through a `len` long queue. A cursor that is not
/// out yet starts from the playing item.
pub fn step(
    selected: Option<usize>,
    current: Option<usize>,
    delta: i32,
    len: usize,
) -> Option<usize> {
    if len == 0 {
        return None;
    }
    let from = selected.or(current).unwrap_or(0) as i64;
    Some((from + i64::from(delta)).clamp(0, len as i64 - 1) as usize)
}

/// Draw `entries` one per row from (`left`, `top`), `width` cells wide.
#[allow(clippy::too_many_arguments)]
pub fn draw(
    printer: &Printer<'_, '_>,
    left: usize,
    top: usize,
    width: usize,
    entries: &[Entry],
    current: Option<usize>,
    selected: Option<usize>,
    styles: &Styles,
) -> Vec<Hit> {
    let number_width = entries
        .last()
        .map_or(1, |entry| (entry.position + 1).to_string().len());
    let mut hits = Vec::with_capacity(entries.len());

    for (offset, entry) in entries.iter().enumerate() {
        let row = top + offset;
        let playing = Some(entry.position) == current;
        let played = current.is_some_and(|current| entry.position < current);
        let marker = if playing { "▸ " } else { "  " };
        let number = format!("{:>number_width$}", entry.position + 1);
        let duration = entry.playable.duration_str();
        let (title, byline) = describe(&entry.playable);

        let name_start = left + marker.width() + number.width() + GAP;
        let duration_start = (left + width).saturating_sub(duration.width());
        let room = duration_start.saturating_sub(name_start + GAP);

        let (ink, number_ink) = if Some(entry.position) == selected {
            (styles.selected, styles.selected)
        } else if playing {
            (styles.current, styles.current)
        } else if played {
            (styles.quiet, styles.quiet)
        } else {
            (styles.text, styles.quiet)
        };

        if Some(entry.position) == selected {
            printer.with_color(styles.selected, |printer| {
                printer.print_hline((left, row), width, " ");
            });
        }
        printer.with_color(number_ink, |printer| {
            printer.print((left, row), marker);
            printer.print((left + marker.width(), row), &number);
            printer.print((duration_start, row), &duration);
        });

        // The title always gets the room; the artist follows it only when there is
        // some left over.
        let title = truncate(&title, room);
        let bold = if playing {
            Effect::Bold
        } else {
            Effect::Simple
        };
        printer.with_color(ink, |printer| {
            printer.with_effect(bold, |printer| printer.print((name_start, row), &title));
        });
        let spare = room.saturating_sub(title.width() + 3);
        if !byline.is_empty() && spare >= 4 {
            let quiet = if Some(entry.position) == selected {
                styles.selected
            } else {
                styles.quiet
            };
            printer.with_color(quiet, |printer| {
                printer.print(
                    (name_start + title.width(), row),
                    &format!(" · {}", truncate(&byline, spare)),
                );
            });
        }

        hits.push(Hit {
            row,
            index: entry.index,
        });
    }
    hits
}

/// A line of help under the list, cut to fit.
pub fn draw_hint(
    printer: &Printer<'_, '_>,
    left: usize,
    row: usize,
    width: usize,
    text: &str,
    style: ColorStyle,
) {
    let hint = truncate(text, width);
    let offset = left + width.saturating_sub(hint.width()) / 2;
    printer.with_color(style, |printer| printer.print((offset, row), &hint));
}

fn describe(playable: &Playable) -> (String, String) {
    match playable {
        Playable::Track(track) => (track.title.clone(), track.artists.join(", ")),
        Playable::Episode(episode) => (episode.name.clone(), String::new()),
    }
}

fn truncate(text: &str, width: usize) -> String {
    if text.width() <= width {
        return text.to_string();
    }
    let mut result = String::new();
    for character in text.chars() {
        if result.width() + 1 >= width {
            break;
        }
        result.push(character);
    }
    if width > 0 {
        result.push('…');
    }
    result
}

#[cfg(test)]
mod tests {
    use super::{step, truncate, window_start};

    #[test]
    fn the_track_before_the_playing_one_stays_in_view() {
        assert_eq!(window_start(Some(10), None, 50, 8), 9);
        assert_eq!(window_start(Some(0), None, 50, 8), 0);
        // Near the end the list fills the panel rather than trailing off.
        assert_eq!(window_start(Some(48), None, 50, 8), 42);
        // A short queue is shown whole.
        assert_eq!(window_start(Some(3), None, 5, 8), 0);
    }

    #[test]
    fn the_list_scrolls_just_far_enough_to_follow_the_cursor() {
        assert_eq!(window_start(Some(10), Some(16), 50, 8), 9);
        assert_eq!(window_start(Some(10), Some(17), 50, 8), 10);
        assert_eq!(window_start(Some(10), Some(4), 50, 8), 4);
    }

    #[test]
    fn the_cursor_starts_at_the_playing_track_and_stops_at_the_ends() {
        assert_eq!(step(None, Some(4), 1, 10), Some(5));
        assert_eq!(step(Some(0), Some(4), -1, 10), Some(0));
        assert_eq!(step(Some(8), Some(4), 5, 10), Some(9));
        assert_eq!(step(None, None, 1, 0), None);
    }

    #[test]
    fn long_names_are_cut_with_an_ellipsis() {
        assert_eq!(truncate("Oklahoma Smokeshow", 8), "Oklahom…");
        assert_eq!(truncate("Dawns", 8), "Dawns");
    }
}
