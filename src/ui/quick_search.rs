use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::thread;
use std::time::Duration;

use cursive::event::{Event, EventResult, Key};
use cursive::theme::{ColorStyle, Effect};
use cursive::{Printer, Vec2, View};
use rspotify::model::{SearchResult, SearchType};
use unicode_width::UnicodeWidthStr;

use crate::events::EventManager;
use crate::library::Library;
use crate::model::playable::Playable;
use crate::model::track::Track;
use crate::queue::Queue;
use crate::spotify::Spotify;
use crate::traits::ListItem;

/// Results shown, and the number of digit keys that pick one.
const RESULTS: usize = 4;
/// How long typing has to pause before a query is sent, so a fast typist makes
/// one request rather than one per keystroke.
const DEBOUNCE: Duration = Duration::from_millis(220);
const WIDTH: usize = 62;

/// What the next keypress means. Digits pick a result, so the query can only take
/// them while it is being typed: pressing Enter hands the digits over to the list.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Stage {
    Typing,
    Picking,
    /// A result is chosen and is waiting for an action key.
    Acting(usize),
}

#[derive(Default)]
struct Results {
    /// The query these results belong to, shown while a newer one is in flight.
    query: String,
    tracks: Vec<Track>,
    searching: bool,
    failed: bool,
}

/// A search overlay for the now playing screen: type, pick one of the top results
/// with a digit, then choose what to do with it. Never leaves the screen.
pub struct QuickSearch {
    spotify: Spotify,
    queue: Arc<Queue>,
    library: Arc<Library>,
    events: EventManager,
    query: String,
    stage: Stage,
    results: Arc<RwLock<Results>>,
    /// Bumped on every keystroke; a search only lands if it is still the newest.
    generation: Arc<AtomicU64>,
}

impl QuickSearch {
    pub fn new(
        spotify: Spotify,
        queue: Arc<Queue>,
        library: Arc<Library>,
        events: EventManager,
    ) -> Self {
        Self {
            spotify,
            queue,
            library,
            events,
            query: String::new(),
            stage: Stage::Typing,
            results: Arc::default(),
            generation: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Queue a search for the current query, replacing any pending one.
    fn schedule_search(&self) {
        let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        let query = self.query.trim().to_string();

        if query.is_empty() {
            *self.results.write().unwrap() = Results::default();
            return;
        }
        self.results.write().unwrap().searching = true;

        let spotify = self.spotify.clone();
        let results = self.results.clone();
        let events = self.events.clone();
        let current = self.generation.clone();
        thread::spawn(move || {
            thread::sleep(DEBOUNCE);
            // Another keystroke landed while we waited: that search supersedes this one.
            if current.load(Ordering::SeqCst) != generation {
                return;
            }

            let found = spotify
                .api
                .search(SearchType::Track, &query, RESULTS as u32, 0);
            if current.load(Ordering::SeqCst) != generation {
                return;
            }

            let mut results = results.write().unwrap();
            results.searching = false;
            results.query = query;
            match found {
                Ok(SearchResult::Tracks(page)) => {
                    results.failed = false;
                    results.tracks = page.items.iter().map(Track::from).collect();
                }
                Ok(_) => {}
                Err(()) => {
                    results.failed = true;
                    results.tracks.clear();
                }
            }
            drop(results);
            events.trigger();
        });
    }

    fn tracks(&self) -> Vec<Track> {
        self.results.read().unwrap().tracks.clone()
    }

    /// Act on the chosen result. `immediately` plays it now; otherwise it goes in
    /// right after the current item.
    fn act(&self, index: usize, immediately: bool) {
        let Some(track) = self.tracks().get(index).cloned() else {
            return;
        };
        let mut playable = Playable::Track(track);
        if immediately {
            playable.play(&self.queue);
        } else {
            playable.play_next(&self.queue);
        }
    }

    fn draw_frame(&self, printer: &Printer<'_, '_>) {
        let (width, height) = (printer.size.x, printer.size.y);
        if width < 4 || height < 3 {
            return;
        }

        for row in 0..height {
            printer.print_hline((0, row), width, " ");
        }
        let border = ColorStyle::secondary();
        printer.with_color(border, |printer| {
            printer.print((0, 0), "╭");
            printer.print((1, 0), &"─".repeat(width - 2));
            printer.print((width - 1, 0), "╮");
            for row in 1..height - 1 {
                printer.print((0, row), "│");
                printer.print((width - 1, row), "│");
            }
            printer.print((0, height - 1), "╰");
            printer.print((1, height - 1), &"─".repeat(width - 2));
            printer.print((width - 1, height - 1), "╯");
        });
        printer.with_color(ColorStyle::title_primary(), |printer| {
            printer.print((3, 0), " quick search ");
        });
    }

    fn draw_query(&self, printer: &Printer<'_, '_>, row: usize, width: usize) {
        let caret = if self.stage == Stage::Typing {
            "▏"
        } else {
            ""
        };
        let query = format!("{}{caret}", self.query);
        printer.with_color(ColorStyle::secondary(), |printer| {
            printer.print((2, row), "❯");
        });
        printer.with_color(ColorStyle::primary(), |printer| {
            printer.print((4, row), &truncate(&query, width));
        });
    }

    fn draw_results(&self, printer: &Printer<'_, '_>, top: usize, width: usize) {
        let results = self.results.read().unwrap();
        if results.tracks.is_empty() {
            let message = if results.failed {
                "search failed"
            } else if results.searching {
                "searching…"
            } else if self.query.trim().is_empty() {
                "type to search Spotify"
            } else {
                "no results"
            };
            printer.with_color(ColorStyle::secondary(), |printer| {
                printer.print((4, top), message);
            });
            return;
        }

        for (index, track) in results.tracks.iter().take(RESULTS).enumerate() {
            let row = top + index;
            if row >= printer.size.y {
                break;
            }
            let chosen = self.stage == Stage::Acting(index);
            let key_style = if chosen {
                ColorStyle::highlight()
            } else {
                ColorStyle::secondary()
            };
            printer.with_color(key_style, |printer| {
                printer.print((2, row), &format!(" {} ", index + 1));
            });

            // Mark what is already in the library, so a result you own is obvious.
            let saved = if self.library.is_saved_track(&Playable::Track(track.clone())) {
                if self.library.cfg.values().use_nerdfont.unwrap_or(false) {
                    "\u{f012c} "
                } else {
                    "♥ "
                }
            } else {
                ""
            };
            let artists = track.artists.join(", ");
            let title = truncate(&format!("{saved}{}", track.title), width / 2);
            printer.with_color(ColorStyle::primary(), |printer| {
                if chosen {
                    printer.with_effect(Effect::Bold, |printer| printer.print((6, row), &title));
                } else {
                    printer.print((6, row), &title);
                }
            });
            let used = 6 + title.width() + 2;
            if used < width {
                printer.with_color(ColorStyle::secondary(), |printer| {
                    printer.print((used, row), &truncate(&artists, width - used));
                });
            }
        }
    }

    fn draw_hint(&self, printer: &Printer<'_, '_>, row: usize) {
        let hint = match self.stage {
            Stage::Typing if self.tracks().is_empty() => "Esc  close",
            Stage::Typing => "Enter  pick a result     Esc  close",
            Stage::Picking => "1-4  choose     type to edit     Esc  back",
            Stage::Acting(_) => "1  play next     2  play now     Esc  back",
        };
        printer.with_color(ColorStyle::secondary(), |printer| {
            printer.print((2, row), hint);
        });
    }
}

impl View for QuickSearch {
    fn draw(&self, printer: &Printer<'_, '_>) {
        self.draw_frame(printer);
        if printer.size.x < 12 || printer.size.y < 8 {
            return;
        }

        let width = printer.size.x.saturating_sub(6);
        self.draw_query(printer, 2, width);
        self.draw_results(printer, 4, width);
        self.draw_hint(printer, printer.size.y - 2);
    }

    fn required_size(&mut self, constraint: Vec2) -> Vec2 {
        Vec2::new(
            std::cmp::min(WIDTH, constraint.x),
            std::cmp::min(4 + RESULTS + 3, constraint.y),
        )
    }

    fn take_focus(
        &mut self,
        _source: cursive::direction::Direction,
    ) -> Result<EventResult, cursive::view::CannotFocus> {
        Ok(EventResult::consumed())
    }

    fn on_event(&mut self, event: Event) -> EventResult {
        let close = || {
            EventResult::with_cb(|s| {
                s.pop_layer();
            })
        };

        match (self.stage, event) {
            (_, Event::Key(Key::Esc)) => match self.stage {
                Stage::Typing => close(),
                Stage::Picking => {
                    self.stage = Stage::Typing;
                    EventResult::consumed()
                }
                Stage::Acting(_) => {
                    self.stage = Stage::Picking;
                    EventResult::consumed()
                }
            },

            (Stage::Typing, Event::Key(Key::Backspace)) => {
                self.query.pop();
                self.schedule_search();
                EventResult::consumed()
            }
            (Stage::Typing, Event::Key(Key::Enter)) => {
                if !self.tracks().is_empty() {
                    self.stage = Stage::Picking;
                }
                EventResult::consumed()
            }
            (Stage::Typing, Event::Char(character)) => {
                self.query.push(character);
                self.schedule_search();
                EventResult::consumed()
            }

            (Stage::Picking, Event::Char(digit @ '1'..='4')) => {
                let index = digit as usize - '1' as usize;
                if index < self.tracks().len() {
                    self.stage = Stage::Acting(index);
                }
                EventResult::consumed()
            }
            // Anything else typed goes back to editing, so a mistyped digit does not
            // strand you in the list.
            (Stage::Picking, Event::Char(character)) => {
                self.query.push(character);
                self.stage = Stage::Typing;
                self.schedule_search();
                EventResult::consumed()
            }

            (Stage::Acting(index), Event::Char(action @ ('1' | '2'))) => {
                self.act(index, action == '2');
                close()
            }

            _ => EventResult::Ignored,
        }
    }
}

fn truncate(text: &str, max_width: usize) -> String {
    if text.width() <= max_width {
        return text.to_string();
    }
    let mut result = String::new();
    let mut width = 0;
    for character in text.chars() {
        let character_width = unicode_width::UnicodeWidthChar::width(character).unwrap_or(0);
        if width + character_width > max_width.saturating_sub(1) {
            break;
        }
        result.push(character);
        width += character_width;
    }
    result.push('…');
    result
}

#[cfg(test)]
mod tests {
    use super::{QuickSearch, RESULTS, Stage};
    use crate::config::Config;
    use crate::events::EventManager;
    use crate::library::Library;
    use crate::model::track::Track;
    use crate::queue::Queue;
    use crate::spotify::Spotify;
    use cursive::View;
    use cursive::event::{Event, EventResult, Key};
    use std::sync::Arc;

    fn track(title: &str) -> Track {
        Track {
            id: Some(title.to_string()),
            uri: format!("spotify:track:{title}"),
            title: title.to_string(),
            track_number: 1,
            disc_number: 1,
            duration: 1000,
            artists: vec!["Artist".to_string()],
            artist_ids: vec![],
            album: Some("Album".to_string()),
            album_id: None,
            album_artists: vec![],
            cover_url: None,
            url: String::new(),
            added_at: None,
            list_index: 0,
            is_local: false,
            is_playable: Some(true),
        }
    }

    /// A search overlay holding `found` results, over a queue with one item playing.
    fn overlay(found: usize) -> (QuickSearch, Arc<Queue>) {
        let cfg = Config::new_for_test();
        let ev = EventManager::new_for_test();
        let spotify = Spotify::new_for_test(cfg.clone(), ev.clone());
        let library = Library::new_for_test(ev.clone(), spotify.clone(), cfg.clone());
        let queue = Arc::new(Queue::new_for_test(
            vec![crate::model::playable::Playable::Track(track("playing"))],
            Some(0),
            spotify.clone(),
            cfg,
            library.clone(),
        ));
        let search = QuickSearch::new(spotify, queue.clone(), library, ev);
        search.results.write().unwrap().tracks =
            (0..found).map(|i| track(&format!("result {i}"))).collect();
        (search, queue)
    }

    fn press(search: &mut QuickSearch, event: Event) {
        assert!(
            matches!(search.on_event(event.clone()), EventResult::Consumed(_)),
            "{event:?} was ignored"
        );
    }

    #[test]
    fn typing_builds_the_query() {
        let (mut search, _) = overlay(0);
        press(&mut search, Event::Char('a'));
        press(&mut search, Event::Char('b'));
        press(&mut search, Event::Key(Key::Backspace));
        press(&mut search, Event::Char('c'));
        assert_eq!(search.query, "ac");
        assert_eq!(search.stage, Stage::Typing);
    }

    #[test]
    fn digits_stay_in_the_query_until_a_pick_is_asked_for() {
        // "blink 182" has to be typeable, so digits only select once Enter is pressed.
        let (mut search, _) = overlay(RESULTS);
        for character in "blink 182".chars() {
            press(&mut search, Event::Char(character));
        }
        assert_eq!(search.query, "blink 182");

        press(&mut search, Event::Key(Key::Enter));
        assert_eq!(search.stage, Stage::Picking);
        press(&mut search, Event::Char('2'));
        assert_eq!(search.stage, Stage::Acting(1));
    }

    #[test]
    fn enter_does_nothing_without_results() {
        let (mut search, _) = overlay(0);
        press(&mut search, Event::Key(Key::Enter));
        assert_eq!(search.stage, Stage::Typing);
    }

    #[test]
    fn typing_in_pick_mode_returns_to_the_query() {
        let (mut search, _) = overlay(RESULTS);
        press(&mut search, Event::Key(Key::Enter));
        press(&mut search, Event::Char('x'));
        assert_eq!(search.stage, Stage::Typing);
        assert_eq!(search.query, "x");
    }

    #[test]
    fn escape_walks_back_one_stage_at_a_time() {
        let (mut search, _) = overlay(RESULTS);
        press(&mut search, Event::Key(Key::Enter));
        press(&mut search, Event::Char('1'));
        assert_eq!(search.stage, Stage::Acting(0));
        press(&mut search, Event::Key(Key::Esc));
        assert_eq!(search.stage, Stage::Picking);
        press(&mut search, Event::Key(Key::Esc));
        assert_eq!(search.stage, Stage::Typing);
    }

    #[test]
    fn play_next_inserts_after_the_current_item() {
        let (mut search, queue) = overlay(RESULTS);
        press(&mut search, Event::Key(Key::Enter));
        press(&mut search, Event::Char('3'));
        press(&mut search, Event::Char('1'));

        let items = queue.queue.read().unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[1].id(), Some("result 2".to_string()));
        // Playback did not move off the current item.
        assert_eq!(queue.get_current_index(), Some(0));
    }

    #[test]
    fn play_now_starts_the_chosen_result() {
        let (mut search, queue) = overlay(RESULTS);
        press(&mut search, Event::Key(Key::Enter));
        press(&mut search, Event::Char('1'));
        press(&mut search, Event::Char('2'));

        assert_eq!(
            queue.get_current().and_then(|item| item.id()),
            Some("result 0".to_string())
        );
    }
}
