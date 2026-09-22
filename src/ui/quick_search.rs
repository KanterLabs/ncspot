use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::thread;
use std::time::Duration;

use cursive::event::{Event, EventResult, Key};
use cursive::theme::{ColorStyle, Effect};
use cursive::{Printer, Vec2, View};
use rspotify::model::{SearchResult, SearchType};
use unicode_width::UnicodeWidthStr;

use log::debug;

use crate::events::EventManager;
use crate::library::Library;
use crate::model::playable::Playable;
use crate::model::track::Track;
use crate::queue::Queue;
use crate::search_cache::{self, SearchCache};
use crate::spotify::Spotify;
use crate::traits::ListItem;

/// Results shown, and the number of digit keys that pick one.
const RESULTS: usize = 4;
/// How long typing has to pause before a query is sent, so a fast typist makes
/// one request rather than one per keystroke. Short, because the list is filled
/// from the library and the cache while the request is in flight.
const DEBOUNCE: Duration = Duration::from_millis(120);
/// Results asked of Spotify. More than are shown, so the cache is worth reusing
/// for the next keystroke and costs no extra round trip.
const FETCH: u32 = 20;
/// How many library hits can take the top of the list before catalogue results.
const LOCAL_SLOTS: usize = 2;
const WIDTH: usize = 62;

/// What the next keypress means. Digits always pick a result the moment there is
/// one to pick, so choosing and playing is two keys and never more; a digit that
/// belongs in the query goes in with Alt held.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Stage {
    Typing,
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

/// What a chosen result can be turned into.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Action {
    PlayNext,
    PlayNow,
    /// Play it now, then fill the queue behind it with tracks like it.
    Radio,
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
    cache: Arc<SearchCache>,
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
            cache: search_cache::shared(),
        }
    }

    /// Tracks in the saved library that match `query`, best match first.
    ///
    /// This needs no network at all: the library is already in memory, restored
    /// from its own disk cache at startup, so these land on the very keystroke.
    fn local_matches(&self, query: &str) -> Vec<Track> {
        let needle = query.trim().to_lowercase();
        if needle.is_empty() {
            return vec![];
        }
        let tracks = self.library.tracks.read().unwrap();
        let mut scored: Vec<(u8, &Track)> = tracks
            .iter()
            .filter_map(|track| local_score(track, &needle).map(|score| (score, track)))
            .collect();
        scored.sort_by_key(|(score, _)| *score);
        scored
            .into_iter()
            .take(LOCAL_SLOTS)
            .map(|(_, track)| track.clone())
            .collect()
    }

    /// Fill the list from what is already known, so it is never empty while a
    /// request is out: library hits first, then the results of this query or of
    /// the longest prefix of it that was searched before.
    fn show_known(&self, query: &str) -> bool {
        let local = self.local_matches(query);
        let remembered = self.cache.fresh(query);
        let known = merge(
            local,
            remembered
                .clone()
                .unwrap_or_else(|| self.cache.best_effort(query)),
        );

        let mut results = self.results.write().unwrap();
        results.query = query.to_string();
        results.failed = false;
        results.tracks = known;
        remembered.is_some()
    }

    /// Queue a search for the current query, replacing any pending one.
    fn schedule_search(&self) {
        let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        let query = self.query.trim().to_string();

        if query.is_empty() {
            *self.results.write().unwrap() = Results::default();
            return;
        }

        // Everything already on hand goes up immediately; a fresh cache hit means
        // there is nothing left to ask for.
        if self.show_known(&query) {
            self.results.write().unwrap().searching = false;
            prefetch_covers(&self.tracks());
            return;
        }
        self.results.write().unwrap().searching = true;

        let spotify = self.spotify.clone();
        let results = self.results.clone();
        let events = self.events.clone();
        let current = self.generation.clone();
        let cache = self.cache.clone();
        let local = self.local_matches(&query);
        thread::spawn(move || {
            thread::sleep(DEBOUNCE);
            // Another keystroke landed while we waited: that search supersedes this one.
            if current.load(Ordering::SeqCst) != generation {
                return;
            }

            let found = spotify.api.search(SearchType::Track, &query, FETCH, 0);
            if current.load(Ordering::SeqCst) != generation {
                return;
            }

            let mut results = results.write().unwrap();
            results.searching = false;
            results.query = query.clone();
            match found {
                Ok(SearchResult::Tracks(page)) => {
                    let fetched: Vec<Track> = page.items.iter().map(Track::from).collect();
                    results.failed = false;
                    results.tracks = merge(local, fetched.clone());
                    let shown = results.tracks.clone();
                    drop(results);

                    // Remember the whole page, not just what is shown, so the next
                    // keystroke has something to fall back on.
                    cache.store(&query, fetched);
                    cache.save();
                    prefetch_covers(&shown);
                }
                Ok(_) => {}
                Err(()) => {
                    // Keep whatever was already on screen rather than blanking it.
                    results.failed = results.tracks.is_empty();
                }
            }
            events.trigger();
        });
    }

    fn tracks(&self) -> Vec<Track> {
        self.results.read().unwrap().tracks.clone()
    }

    /// Act on the chosen result.
    fn act(&self, index: usize, action: Action) {
        let Some(track) = self.tracks().get(index).cloned() else {
            return;
        };
        match action {
            Action::PlayNext => Playable::Track(track).play_next(&self.queue),
            Action::PlayNow => Playable::Track(track).play(&self.queue),
            Action::Radio => self.start_radio(track),
        }
    }

    /// Play `track` now and queue tracks like it behind it.
    ///
    /// The seed plays straight away so the key press is felt at once; the rest of
    /// the station arrives behind it when Spotify answers.
    fn start_radio(&self, track: Track) {
        let seed = track.uri.clone();
        let id = track.id.clone();
        Playable::Track(track).play(&self.queue);

        let Some(id) = id else {
            return;
        };
        let spotify = self.spotify.clone();
        let queue = self.queue.clone();
        let events = self.events.clone();
        thread::spawn(move || {
            let Ok(found) = spotify.api.recommendations(None, None, Some(vec![&id])) else {
                debug!("no recommendations for {id}");
                return;
            };
            let station: Vec<Playable> = found
                .tracks
                .iter()
                .map(Track::from)
                // The seed is already playing, so it does not want queueing again.
                .filter(|track| track.uri != seed)
                .map(Playable::Track)
                .collect();
            if !station.is_empty() {
                queue.append_next(&station);
            }
            events.trigger();
        });
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
            Stage::Typing => "1-4  pick     Alt+digit  type it     Esc  close",
            Stage::Acting(_) => "1  play next     2  play now     3  radio     Esc  back",
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
                Stage::Acting(_) => {
                    self.stage = Stage::Typing;
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
                    self.stage = Stage::Acting(0);
                }
                EventResult::consumed()
            }
            // A digit picks the result it numbers. With nothing to pick yet it is
            // just a character, so a query that starts with a number still types.
            (Stage::Typing, Event::Char(digit @ '1'..='4'))
                if (digit as usize - '1' as usize) < self.tracks().len() =>
            {
                self.stage = Stage::Acting(digit as usize - '1' as usize);
                EventResult::consumed()
            }
            // The way to put a digit in the query once results are up.
            (Stage::Typing, Event::AltChar(character)) => {
                self.query.push(character);
                self.schedule_search();
                EventResult::consumed()
            }
            (Stage::Typing, Event::Char(character)) => {
                self.query.push(character);
                self.schedule_search();
                EventResult::consumed()
            }

            (Stage::Acting(index), Event::Char(key @ ('1' | '2' | '3'))) => {
                let action = match key {
                    '1' => Action::PlayNext,
                    '2' => Action::PlayNow,
                    _ => Action::Radio,
                };
                self.act(index, action);
                close()
            }
            // Anything else typed goes back to editing, so a mistyped key does not
            // strand you on a result.
            (Stage::Acting(_), Event::Char(character)) => {
                self.query.push(character);
                self.stage = Stage::Typing;
                self.schedule_search();
                EventResult::consumed()
            }

            _ => EventResult::Ignored,
        }
    }
}

/// How well a saved track matches, lower being better. `None` means it does not.
fn local_score(track: &Track, needle: &str) -> Option<u8> {
    let title = track.title.to_lowercase();
    if title.starts_with(needle) {
        return Some(0);
    }
    let artists = track.artists.join(", ").to_lowercase();
    if artists.starts_with(needle) {
        return Some(1);
    }
    if title.contains(needle) {
        return Some(2);
    }
    if artists.contains(needle) {
        return Some(3);
    }
    let album = track.album.as_deref().unwrap_or_default().to_lowercase();
    if album.contains(needle) {
        return Some(4);
    }
    None
}

/// Library hits ahead of catalogue results, without repeating a track that is in
/// both, trimmed to what the overlay can show.
fn merge(local: Vec<Track>, remote: Vec<Track>) -> Vec<Track> {
    let mut merged: Vec<Track> = Vec::with_capacity(RESULTS);
    for track in local.into_iter().chain(remote) {
        if merged.len() == RESULTS {
            break;
        }
        if !merged.iter().any(|kept| kept.uri == track.uri) {
            merged.push(track);
        }
    }
    merged
}

/// Warm the cover cache for what is on screen, so art for a track that gets played
/// is already on disk.
fn prefetch_covers(tracks: &[Track]) {
    #[cfg(feature = "album_art")]
    crate::ui::album_art::prefetch_covers(
        tracks
            .iter()
            .filter_map(|track| track.cover_url.clone())
            .collect(),
    );
    #[cfg(not(feature = "album_art"))]
    let _ = tracks;
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
        seed(&search, found);
        (search, queue)
    }

    /// Put results on screen, as a landed search would. Typing clears them, so a
    /// test that types first has to seed again afterwards.
    fn seed(search: &QuickSearch, found: usize) {
        search.results.write().unwrap().tracks =
            (0..found).map(|i| track(&format!("result {i}"))).collect();
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
    fn a_digit_picks_the_result_it_numbers() {
        let (mut search, _) = overlay(RESULTS);
        press(&mut search, Event::Char('2'));
        assert_eq!(search.stage, Stage::Acting(1));
        // The digit chose a result rather than being typed.
        assert!(search.query.is_empty());
    }

    #[test]
    fn a_digit_types_when_there_is_nothing_to_pick() {
        // A query can still start with a number, because nothing is numbered yet.
        let (mut search, _) = overlay(0);
        press(&mut search, Event::Char('9'));
        press(&mut search, Event::Char('9'));
        assert_eq!(search.query, "99");
        assert_eq!(search.stage, Stage::Typing);
    }

    #[test]
    fn alt_types_a_digit_that_would_otherwise_pick() {
        let (mut search, _) = overlay(RESULTS);
        for character in "blink ".chars() {
            press(&mut search, Event::Char(character));
        }
        seed(&search, RESULTS);
        for character in "182".chars() {
            press(&mut search, Event::AltChar(character));
        }
        assert_eq!(search.query, "blink 182");
        assert_eq!(search.stage, Stage::Typing);
    }

    #[test]
    fn enter_does_nothing_without_results() {
        let (mut search, _) = overlay(0);
        press(&mut search, Event::Key(Key::Enter));
        assert_eq!(search.stage, Stage::Typing);
    }

    #[test]
    fn typing_on_a_chosen_result_returns_to_the_query() {
        let (mut search, _) = overlay(RESULTS);
        press(&mut search, Event::Key(Key::Enter));
        press(&mut search, Event::Char('x'));
        assert_eq!(search.stage, Stage::Typing);
        assert_eq!(search.query, "x");
    }

    #[test]
    fn escape_walks_back_to_the_query_before_closing() {
        let (mut search, _) = overlay(RESULTS);
        press(&mut search, Event::Char('1'));
        assert_eq!(search.stage, Stage::Acting(0));
        press(&mut search, Event::Key(Key::Esc));
        assert_eq!(search.stage, Stage::Typing);
    }

    #[test]
    fn library_hits_land_on_the_keystroke() {
        let (mut search, _) = overlay(0);
        search
            .library
            .tracks
            .write()
            .unwrap()
            .push(track("Solaris"));

        for character in "sol".chars() {
            press(&mut search, Event::Char(character));
        }
        // No request has had time to land, so this can only have come from the
        // library that was already in memory.
        assert_eq!(
            search.tracks().first().map(|found| found.title.clone()),
            Some("Solaris".to_string())
        );
    }

    #[test]
    fn a_remembered_query_is_served_without_searching() {
        let (mut search, _) = overlay(0);
        search.cache.store("sol", vec![track("Cached")]);

        for character in "sol".chars() {
            press(&mut search, Event::Char(character));
        }
        assert_eq!(
            search.tracks().first().map(|found| found.title.clone()),
            Some("Cached".to_string())
        );
        assert!(!search.results.read().unwrap().searching);
    }

    #[test]
    fn a_prefix_fills_the_list_while_the_next_query_is_out() {
        let (mut search, _) = overlay(0);
        search.cache.store("sol", vec![track("Cached")]);

        for character in "solar".chars() {
            press(&mut search, Event::Char(character));
        }
        // The longer query is not cached, so its results are still coming, but the
        // prefix's keep the list from going blank.
        assert_eq!(
            search.tracks().first().map(|found| found.title.clone()),
            Some("Cached".to_string())
        );
        assert!(search.results.read().unwrap().searching);
    }

    #[test]
    fn merging_keeps_library_hits_first_and_drops_repeats() {
        let merged = super::merge(
            vec![track("Owned")],
            vec![
                track("Owned"),
                track("Found"),
                track("Other"),
                track("More"),
                track("Extra"),
            ],
        );
        let titles: Vec<String> = merged.iter().map(|found| found.title.clone()).collect();
        assert_eq!(titles, ["Owned", "Found", "Other", "More"]);
    }

    #[test]
    fn play_next_inserts_after_the_current_item() {
        let (mut search, queue) = overlay(RESULTS);
        press(&mut search, Event::Char('3'));
        press(&mut search, Event::Char('1'));

        let items = queue.queue.read().unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[1].id(), Some("result 2".to_string()));
        // Playback did not move off the current item.
        assert_eq!(queue.get_current_index(), Some(0));
    }

    #[test]
    fn radio_starts_the_chosen_result_straight_away() {
        let (mut search, queue) = overlay(RESULTS);
        press(&mut search, Event::Char('2'));
        press(&mut search, Event::Char('3'));

        // The station is fetched in the background, but the seed plays at once.
        assert_eq!(
            queue.get_current().and_then(|item| item.id()),
            Some("result 1".to_string())
        );
    }

    #[test]
    fn play_now_starts_the_chosen_result() {
        let (mut search, queue) = overlay(RESULTS);
        press(&mut search, Event::Char('1'));
        press(&mut search, Event::Char('2'));

        assert_eq!(
            queue.get_current().and_then(|item| item.id()),
            Some("result 0".to_string())
        );
    }
}
