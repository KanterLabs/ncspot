use std::sync::{Arc, RwLock};
use std::time::Duration;

use cursive::Printer;
use cursive::event::{Event, EventResult, MouseButton, MouseEvent};
use cursive::theme::{ColorStyle, ColorType, Effect, PaletteColor};
use cursive::traits::View;
use cursive::vec::Vec2;
use unicode_width::UnicodeWidthStr;

use crate::events::EventManager;
use crate::library::Library;
use crate::model::playable::Playable;
use crate::queue::{Queue, RepeatSetting};
use crate::spotify::{PlayerEvent, Spotify, VOLUME_PERCENT};
use crate::ui::accent;
use crate::ui::anim::{Animator, DEFAULT_FPS, lift, marquee};
use crate::ui::spectrum::BLOCKS;
use crate::utils::ms_to_hms;
use crate::waveform;

/// Rows the statusbar takes: a hairline to set it off from the screen above, then
/// the two line mini player.
pub const HEIGHT: usize = 3;

/// How far the progress bar is lifted towards white on the hardest beat. Smaller
/// than the now playing card's, because this line is always on screen.
const BEAT_LIFT: f32 = 0.25;
/// How far the cover brightens on the beat. Gentler than the bar: it is a picture.
#[cfg(feature = "album_art")]
const ART_GLOW: f32 = 0.12;
/// Frames per second the statusbar animates at. Deliberately slower than the now
/// playing card: this line is on screen whatever else you are doing, so it should
/// cost as little as a moving thing can.
const STATUSBAR_FPS: u32 = 12;

/// Columns the cover takes. It is two rows tall, and half blocks make each cell two
/// pixels high, so four columns draw a square.
const ART_WIDTH: usize = 4;
/// Width of the transport buttons, and of the equalizer drawn underneath them.
const TRANSPORT_WIDTH: usize = 10;
/// Space between the columns of the bar.
const GAP: usize = 3;
/// The track name is never squeezed narrower than this; the equalizer and then the
/// cover give their room up first.
const MIN_TEXT_WIDTH: usize = 14;
/// Bounds on the width of the right hand block holding the seek bar.
const RIGHT_MIN_WIDTH: usize = 24;
const RIGHT_MAX_WIDTH: usize = 56;

/// The most of a cast device's name the bar shows.
const CAST_NAME_WIDTH: usize = 18;

/// Rows of the mini player within the statusbar.
const TOP: usize = 1;
const BOTTOM: usize = 2;

/// What a click on a given run of cells does.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Control {
    Seek,
    Previous,
    PlayPause,
    Next,
    Shuffle,
    Repeat,
    Volume,
    /// The cast device's name: a click opens the cast picker.
    Cast,
}

/// A run of cells on one row that responds to the mouse, rebuilt on every draw.
#[derive(Clone, Copy, Debug)]
struct Hitbox {
    row: usize,
    start: usize,
    width: usize,
    control: Control,
}

impl Hitbox {
    fn contains(&self, position: Vec2) -> bool {
        position.y == self.row
            && position.x >= self.start
            && position.x < self.start.saturating_add(self.width)
    }

    fn fraction(&self, position: Vec2) -> f64 {
        if self.width <= 1 {
            return 0.0;
        }
        (position.x - self.start) as f64 / (self.width - 1) as f64
    }
}

/// Where each column of the bar goes at a given width.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Geometry {
    /// Whether the cover is drawn, at column 1.
    art: bool,
    text_start: usize,
    text_width: usize,
    /// Where the transport and equalizer column starts, when there is room for it.
    transport: Option<usize>,
    right_start: usize,
    right_width: usize,
}

impl Geometry {
    /// Lay the bar out across `width` columns. The seek bar always stays; as the
    /// terminal narrows the transport goes first, since every button has a key, and
    /// the cover after it.
    fn new(width: usize, art_available: bool) -> Self {
        let right_width = (width * 2 / 5)
            .clamp(RIGHT_MIN_WIDTH, RIGHT_MAX_WIDTH)
            .min(width.saturating_sub(2));
        let right_start = width.saturating_sub(right_width + 1);

        let text_start = |art: bool| if art { 1 + ART_WIDTH + 2 } else { 1 };
        let candidates = [
            (art_available, true),
            (art_available, false),
            (false, false),
        ];
        let (art, transport) = candidates
            .into_iter()
            .find(|&(art, transport)| {
                let reserved = if transport { TRANSPORT_WIDTH + GAP } else { 0 };
                right_start.saturating_sub(GAP + reserved + text_start(art)) >= MIN_TEXT_WIDTH
            })
            .unwrap_or((false, false));

        // The buttons sit in the middle of the screen, where a player's controls are
        // looked for, unless the seek bar pushes them left of it.
        let transport_start = transport.then(|| {
            let earliest = text_start(art) + MIN_TEXT_WIDTH + GAP;
            let latest = right_start - GAP - TRANSPORT_WIDTH;
            (width / 2)
                .saturating_sub(TRANSPORT_WIDTH / 2)
                .clamp(earliest, latest)
        });
        let text_start = text_start(art);
        let text_end = transport_start.unwrap_or(right_start).saturating_sub(GAP);
        Self {
            art,
            text_start,
            text_width: text_end.saturating_sub(text_start),
            transport: transport_start,
            right_start,
            right_width,
        }
    }
}

/// The three colours the bar is drawn in, all on the statusbar's background.
#[derive(Clone, Copy)]
struct Inks {
    /// Text and controls.
    text: ColorStyle,
    /// The album tinted, beat lit colour of the progress and the equalizer.
    bar: ColorStyle,
    /// Secondary text, unplayed track and switched off toggles.
    quiet: ColorStyle,
}

pub struct StatusBar {
    queue: Arc<Queue>,
    spotify: Spotify,
    library: Arc<Library>,
    events: EventManager,
    hitboxes: RwLock<Vec<Hitbox>>,
    #[cfg(feature = "album_art")]
    art: crate::ui::album_art::AlbumArt,
    /// Keeps the frames coming while something is playing, so the equalizer moves
    /// and the bar creeps forward wherever you are in the app.
    _animator: Arc<Animator>,
}

impl StatusBar {
    pub fn new(queue: Arc<Queue>, library: Arc<Library>, events: EventManager) -> Self {
        let spotify = queue.get_spotify();
        let fps = library
            .cfg
            .values()
            .visualizer_fps
            .unwrap_or(DEFAULT_FPS)
            .min(STATUSBAR_FPS);
        let animator = Animator::spawn(events.clone(), spotify.clone(), fps);

        Self {
            queue,
            spotify,
            library,
            #[cfg(feature = "album_art")]
            art: crate::ui::album_art::AlbumArt::new(events.clone()),
            events,
            hitboxes: RwLock::default(),
            _animator: animator,
        }
    }

    /// Whether the moving parts of the statusbar are wanted at all.
    fn animated(&self) -> bool {
        self.library
            .cfg
            .values()
            .visualizer_fps
            .unwrap_or(DEFAULT_FPS)
            > 0
    }

    fn is_playing(&self) -> bool {
        matches!(self.spotify.get_current_status(), PlayerEvent::Playing(_))
    }

    /// The live spectrum in `count` bands, or `None` when there is no audio to draw
    /// one from and a flat line should stand in.
    fn equalizer(&self, count: usize) -> Option<String> {
        if !self.animated() || !self.is_playing() {
            return None;
        }
        let bands = self.spotify.audio_tap().bands(count)?;
        Some(
            bands
                .iter()
                .map(|level| {
                    let eighth = ((level * 8.0).round() as usize).clamp(1, 8);
                    BLOCKS[eighth - 1]
                })
                .collect(),
        )
    }

    fn use_nerdfont(&self) -> bool {
        self.library.cfg.values().use_nerdfont.unwrap_or(false)
    }

    fn volume_display(&self) -> String {
        format!(
            "{}%",
            (self.spotify.volume() as f64 / 65535_f64 * 100.0).round() as u16
        )
    }

    /// How hard the bar is pulsing right now: the beat, unless the user turned the
    /// pulse off or nothing is playing.
    fn pulse(&self) -> f32 {
        if !self.library.cfg.values().beat_pulse.unwrap_or(true) {
            return 0.0;
        }
        self.spotify.audio_tap().pulse()
    }

    /// The first line: the track as `statusbar_format` has it, or its title and
    /// artists when that is unset.
    fn format_track(&self, t: &Playable) -> String {
        if let Some(format) = self.library.cfg.values().statusbar_format.as_deref() {
            return Playable::format(t, format, &self.library);
        }
        let artists = Playable::format(t, "%artists", &self.library);
        let title = Playable::format(t, "%title", &self.library);
        if artists.is_empty() {
            title
        } else {
            format!("{title} · {artists}")
        }
    }

    /// The second line: where the track comes from.
    fn subtitle(t: &Playable) -> String {
        match t {
            Playable::Track(track) => track.album.clone().unwrap_or_default(),
            Playable::Episode(episode) => episode.release_date.clone(),
        }
    }

    /// The item queued after the current one, for the `next` preview.
    fn next_up(&self) -> Option<String> {
        let index = self.queue.next_index()?;
        // Release the queue lock before anything else is drawn.
        let next = self.queue.queue.read().unwrap().get(index).cloned()?;
        Some(match next {
            Playable::Track(track) => track.title,
            Playable::Episode(episode) => episode.name,
        })
    }

    /// The glyphs for previous, play or pause, and next. The middle one names what a
    /// click does, like the now playing card, unless `flip_status_indicators` asks
    /// for the current state instead.
    fn transport_glyphs(&self) -> [&'static str; 3] {
        let flipped = self
            .library
            .cfg
            .values()
            .flip_status_indicators
            .unwrap_or(false);
        let pause = self.is_playing() != flipped;
        if self.use_nerdfont() {
            let middle = if pause { "\u{f03e4}" } else { "\u{f040a}" };
            ["\u{f04ab}", middle, "\u{f04ad}"]
        } else {
            ["◀◀", if pause { "❚❚" } else { "▶" }, "▶▶"]
        }
    }

    /// The toggles at the end of the second line, each with whether it is on.
    fn toggles(&self) -> [(&'static str, bool, Control); 2] {
        let nerdfont = self.use_nerdfont();
        let shuffle = (
            if nerdfont { "\u{f049d}" } else { "⇄" },
            self.queue.get_shuffle(),
            Control::Shuffle,
        );
        let repeat = match (nerdfont, self.queue.get_repeat()) {
            (true, RepeatSetting::RepeatTrack) => ("\u{f0458}", true, Control::Repeat),
            (true, mode) => ("\u{f0456}", mode != RepeatSetting::None, Control::Repeat),
            (false, RepeatSetting::RepeatTrack) => ("↻1", true, Control::Repeat),
            (false, mode) => ("↻", mode != RepeatSetting::None, Control::Repeat),
        };
        [shuffle, repeat]
    }

    fn updating_indicator(&self) -> &'static str {
        if *self.library.is_done.read().unwrap() {
            ""
        } else if self.use_nerdfont() {
            "\u{f04e6}"
        } else {
            "[U]"
        }
    }

    #[cfg(feature = "album_art")]
    fn draw_art(&self, printer: &Printer<'_, '_>, playable: &Playable) {
        let offset = Vec2::new(1, TOP);
        let size = Vec2::new(ART_WIDTH, 2);
        let drawn = playable.cover_url().is_some_and(|url| {
            self.art
                .draw(printer, offset, size, &url, ART_GLOW * self.pulse())
        });
        if !drawn {
            crate::ui::album_art::draw_placeholder(printer, offset, size);
        }
    }

    #[cfg(not(feature = "album_art"))]
    fn draw_art(&self, _printer: &Printer<'_, '_>, _playable: &Playable) {}

    fn draw_transport(
        &self,
        printer: &Printer<'_, '_>,
        start: usize,
        inks: Inks,
        hitboxes: &mut Vec<Hitbox>,
    ) {
        // Three slots two cells wide with a two cell gap, so the buttons stay put
        // whichever glyph is showing.
        let controls = [Control::Previous, Control::PlayPause, Control::Next];
        for (slot, (glyph, control)) in self
            .transport_glyphs()
            .into_iter()
            .zip(controls)
            .enumerate()
        {
            let x = start + slot * 4;
            let ink = if control == Control::PlayPause {
                inks.bar
            } else {
                inks.text
            };
            printer.with_color(ink, |printer| printer.print((x, TOP), glyph));
            hitboxes.push(Hitbox {
                row: TOP,
                start: x,
                width: 2,
                control,
            });
        }

        match self.equalizer(TRANSPORT_WIDTH) {
            Some(bands) => {
                printer.with_color(inks.bar, |printer| printer.print((start, BOTTOM), &bands))
            }
            None => printer.with_color(inks.quiet, |printer| {
                printer.print((start, BOTTOM), &"▁".repeat(TRANSPORT_WIDTH));
            }),
        }
    }

    /// Elapsed time, the seek bar and the length of the track.
    fn draw_seek(
        &self,
        printer: &Printer<'_, '_>,
        geometry: &Geometry,
        playable: Option<&Playable>,
        inks: Inks,
        hitboxes: &mut Vec<Hitbox>,
    ) {
        let elapsed = self.spotify.get_current_progress();
        let (left, right) = match playable {
            Some(t) => (
                ms_to_hms(elapsed.as_millis().try_into().unwrap_or(0)),
                t.duration_str(),
            ),
            None => ("-:--".to_string(), "-:--".to_string()),
        };
        let start = geometry.right_start;
        let bar_start = start + left.width() + 1;
        let bar_width = geometry
            .right_width
            .saturating_sub(left.width() + right.width() + 2);

        printer.with_color(inks.text, |printer| {
            printer.print((start, TOP), &left);
            printer.print((bar_start + bar_width + 1, TOP), &right);
        });
        printer.with_color(inks.quiet, |printer| {
            printer.print((bar_start, TOP), &"┄".repeat(bar_width));
        });
        if bar_width == 0 {
            return;
        }
        if let Some(t) = playable {
            let filled = (elapsed.as_millis() as u64)
                .saturating_mul(bar_width as u64)
                .checked_div(u64::from(t.duration()))
                .unwrap_or(0)
                .min(bar_width as u64 - 1) as usize;
            printer.with_color(inks.bar, |printer| {
                printer.print((bar_start, TOP), &"━".repeat(filled));
                printer.print((bar_start + filled, TOP), "●");
            });
        }
        hitboxes.push(Hitbox {
            row: TOP,
            start: bar_start,
            width: bar_width,
            control: Control::Seek,
        });
    }

    /// `next  <title>` on the left of the second line, the toggles and volume on
    /// the right.
    fn draw_footer(
        &self,
        printer: &Printer<'_, '_>,
        geometry: &Geometry,
        inks: Inks,
        hitboxes: &mut Vec<Hitbox>,
    ) {
        let end = geometry.right_start + geometry.right_width;
        let volume = self.volume_display();
        let mut x = end.saturating_sub(volume.width());
        printer.with_color(inks.text, |printer| printer.print((x, BOTTOM), &volume));
        hitboxes.push(Hitbox {
            row: BOTTOM,
            start: x,
            width: volume.width(),
            control: Control::Volume,
        });

        for (glyph, on, control) in self.toggles().into_iter().rev() {
            x = x.saturating_sub(glyph.width() + 1);
            let ink = if on { inks.text } else { inks.quiet };
            printer.with_color(ink, |printer| printer.print((x, BOTTOM), glyph));
            hitboxes.push(Hitbox {
                row: BOTTOM,
                start: x,
                width: glyph.width(),
                control,
            });
        }

        let updating = self.updating_indicator();
        if !updating.is_empty() {
            x = x.saturating_sub(updating.width() + 1);
            printer.with_color(inks.text, |printer| printer.print((x, BOTTOM), updating));
        }

        // Where the sound is coming from, while it is not this machine.
        if let Some(device) = self.spotify.cast_target() {
            let glyph = if self.use_nerdfont() {
                "\u{f0118}"
            } else {
                "⇢"
            };
            let label = format!("{glyph} {}", truncate_name(&device, CAST_NAME_WIDTH));
            x = x.saturating_sub(label.width() + 2);
            printer.with_color(inks.bar, |printer| printer.print((x, BOTTOM), &label));
            hitboxes.push(Hitbox {
                row: BOTTOM,
                start: x,
                width: label.width(),
                control: Control::Cast,
            });
        }

        let room = x.saturating_sub(geometry.right_start + 1);
        const LABEL: &str = "next  ";
        if let Some(next) = self.next_up()
            && room > LABEL.width() + 3
        {
            let start = geometry.right_start;
            printer.with_color(inks.quiet, |printer| printer.print((start, BOTTOM), LABEL));
            let phase = self.spotify.get_current_progress();
            let name = marquee(&next, room - LABEL.width(), phase);
            printer.with_color(inks.text, |printer| {
                printer.print((start + LABEL.width(), BOTTOM), &name);
            });
        }
    }

    fn activate(&self, control: Control, hitbox: Hitbox, position: Vec2) {
        match control {
            Control::Seek => {
                if let Some(playable) = self.queue.get_current() {
                    let target = playable.duration() as f64 * hitbox.fraction(position);
                    self.spotify.seek(target.round().max(0.0) as u32);
                }
            }
            Control::Previous => {
                // Matches `Command::Previous`: restart the track unless it just started.
                if self.spotify.get_current_progress() < Duration::from_secs(5) {
                    self.queue.previous();
                } else {
                    self.spotify.seek(0);
                }
            }
            Control::PlayPause => self.queue.toggleplayback(),
            Control::Next => self.queue.next(true),
            Control::Shuffle => self.queue.set_shuffle(!self.queue.get_shuffle()),
            Control::Repeat => {
                let mode = match self.queue.get_repeat() {
                    RepeatSetting::None => RepeatSetting::RepeatPlaylist,
                    RepeatSetting::RepeatPlaylist => RepeatSetting::RepeatTrack,
                    RepeatSetting::RepeatTrack => RepeatSetting::None,
                };
                self.queue.set_repeat(mode);
            }
            // The number is too small a target to click a level into; it scrolls.
            Control::Volume => {}
            // Needs the Cursive root, so `on_event` opens the picker itself.
            Control::Cast => {}
        }
    }

    fn scroll(&self, control: Control, up: bool) {
        match control {
            Control::Seek => self.spotify.seek_relative(if up { -500 } else { 500 }),
            Control::Volume => {
                let volume = if up {
                    self.spotify.volume().saturating_add(VOLUME_PERCENT)
                } else {
                    self.spotify.volume().saturating_sub(VOLUME_PERCENT)
                };
                self.spotify.set_volume(volume, true);
            }
            _ => {}
        }
    }
}

impl View for StatusBar {
    fn draw(&self, printer: &Printer<'_, '_>) {
        if printer.size.x == 0 {
            return;
        }
        self._animator.mark_drawn();
        // The statusbar is the one view that is always on screen, which makes it
        // the right place to keep the shared tint and the learned waveform current.
        let playing = self.queue.get_current();
        accent::follow(
            playing.as_ref().and_then(Playable::cover_url).as_deref(),
            &self.events,
        );
        waveform::observe(&self.spotify, &self.queue);

        // The bar takes the playing album's colour, crossfading as tracks change,
        // and brightens on the beat.
        let palette = &printer.theme.palette;
        let theme_bar = *palette.custom("statusbar_progress").unwrap();
        let tinted = if self.library.cfg.values().cover_accent.unwrap_or(true) {
            accent::current(theme_bar)
        } else {
            theme_bar
        };
        let back = ColorType::Color(*palette.custom("statusbar_bg").unwrap());
        let bar = ColorStyle::new(
            ColorType::Color(lift(tinted, BEAT_LIFT * self.pulse())),
            back,
        );
        let quiet_colour = *palette.custom("statusbar_progress_bg").unwrap();
        let quiet = ColorStyle::new(ColorType::Color(quiet_colour), back);
        let style = ColorStyle::new(
            ColorType::Color(*palette.custom("statusbar").unwrap()),
            back,
        );
        let inks = Inks {
            text: style,
            bar,
            quiet,
        };

        printer.with_color(
            ColorStyle::new(
                ColorType::Color(quiet_colour),
                ColorType::Palette(PaletteColor::Background),
            ),
            |printer| printer.print_hline((0, 0), printer.size.x, "─"),
        );
        printer.with_color(style, |printer| {
            for row in [TOP, BOTTOM] {
                printer.print_hline((0, row), printer.size.x, " ");
            }
        });

        let geometry = Geometry::new(printer.size.x, cfg!(feature = "album_art"));
        let mut hitboxes = Vec::new();

        if let Some(t) = playing.as_ref() {
            if geometry.art {
                self.draw_art(printer, t);
            }
            let phase = self.spotify.get_current_progress();
            let width = geometry.text_width;
            printer.with_color(style, |printer| {
                printer.with_effect(Effect::Bold, |printer| {
                    printer.print(
                        (geometry.text_start, TOP),
                        &marquee(&self.format_track(t), width, phase),
                    );
                });
            });
            printer.with_color(quiet, |printer| {
                printer.print(
                    (geometry.text_start, BOTTOM),
                    &marquee(&Self::subtitle(t), width, phase),
                );
            });
        } else {
            printer.with_color(quiet, |printer| {
                printer.print((geometry.text_start, TOP), "Nothing playing");
            });
        }

        if let Some(start) = geometry.transport {
            self.draw_transport(printer, start, inks, &mut hitboxes);
        }
        self.draw_seek(printer, &geometry, playing.as_ref(), inks, &mut hitboxes);
        self.draw_footer(printer, &geometry, inks, &mut hitboxes);

        *self.hitboxes.write().unwrap() = hitboxes;
    }

    fn required_size(&mut self, constraint: Vec2) -> Vec2 {
        Vec2::new(constraint.x, HEIGHT)
    }

    fn on_event(&mut self, event: Event) -> EventResult {
        let Event::Mouse {
            offset,
            position,
            event,
        } = event
        else {
            return EventResult::Ignored;
        };
        let position = position.saturating_sub(offset);
        let hit = self
            .hitboxes
            .read()
            .unwrap()
            .iter()
            .copied()
            .find(|hitbox| hitbox.contains(position));

        if let (MouseEvent::Press(MouseButton::Left), Some(hitbox)) = (event, hit)
            && hitbox.control == Control::Cast
        {
            let spotify = self.spotify.clone();
            let queue = self.queue.clone();
            let hosts = self
                .library
                .cfg
                .values()
                .roku_hosts
                .clone()
                .unwrap_or_default();
            return EventResult::with_cb(move |s| {
                crate::ui::cast::open(s, spotify.clone(), queue.clone(), hosts.clone());
            });
        }
        match (event, hit) {
            (MouseEvent::Press(MouseButton::Left), Some(hitbox)) => {
                self.activate(hitbox.control, hitbox, position);
            }
            // Anywhere else on the player still pauses and resumes, as the bar
            // always has.
            (MouseEvent::Press(MouseButton::Left), None) if position.y > 0 => {
                self.queue.toggleplayback();
            }
            (MouseEvent::WheelUp, Some(hitbox)) => self.scroll(hitbox.control, true),
            (MouseEvent::WheelDown, Some(hitbox)) => self.scroll(hitbox.control, false),
            _ => {}
        }
        EventResult::Consumed(None)
    }
}

/// `name` cut to `width` columns with an ellipsis.
fn truncate_name(name: &str, width: usize) -> String {
    if name.width() <= width {
        return name.to_string();
    }
    let mut cut = String::new();
    for character in name.chars() {
        if cut.width() + 1 >= width {
            break;
        }
        cut.push(character);
    }
    cut.push('…');
    cut
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use cursive::Vec2;
    use cursive::backends::puppet::Backend as PuppetBackend;

    use crate::config::Config;
    use crate::events::EventManager;
    use crate::library::Library;
    use crate::model::playable::Playable;
    use crate::model::track::Track;
    use crate::queue::Queue;
    use crate::spotify::{PlayerEvent, Spotify};

    use super::{
        ART_WIDTH, GAP, Geometry, MIN_TEXT_WIDTH, RIGHT_MAX_WIDTH, StatusBar, TRANSPORT_WIDTH,
    };

    fn track(title: &str, album: &str) -> Playable {
        Playable::Track(Track {
            id: Some(title.to_string()),
            uri: format!("spotify:track:{title}"),
            title: title.to_string(),
            track_number: 1,
            disc_number: 1,
            duration: 143_000,
            artists: vec!["Zach Bryan".to_string()],
            artist_ids: vec!["zach".to_string()],
            album: Some(album.to_string()),
            album_id: None,
            album_artists: vec![],
            cover_url: None,
            url: String::new(),
            added_at: None,
            list_index: 0,
            is_local: false,
            is_playable: Some(true),
        })
    }

    fn statusbar(tracks: Vec<Playable>, current: Option<usize>, status: PlayerEvent) -> StatusBar {
        let cfg = Config::new_for_test();
        let ev = EventManager::new_for_test();
        let spotify = Spotify::new_for_test(cfg.clone(), ev.clone());
        let library = Library::new_for_test(ev.clone(), spotify.clone(), cfg.clone());
        *library.is_done.write().unwrap() = true;
        spotify.update_status(status);
        let queue = Arc::new(Queue::new_for_test(
            tracks,
            current,
            spotify,
            cfg,
            library.clone(),
        ));
        StatusBar::new(queue, library, ev)
    }

    fn render(bar: StatusBar, width: usize) -> Vec<String> {
        let size = Vec2::new(width, super::HEIGHT);
        let backend = PuppetBackend::init(Some(size));
        let screens = backend.stream();
        let mut siv = cursive::Cursive::new();
        siv.set_theme(crate::theme::load(&Config::new_for_test().values().theme));
        siv.add_fullscreen_layer(bar);
        let mut runner = siv.into_runner(backend);
        runner.refresh();
        let screen = screens.try_iter().last().expect("a frame was captured");
        (0..size.y)
            .map(|y| {
                (0..size.x)
                    .map(|x| match &screen[Vec2::new(x, y)] {
                        Some(cell) if !cell.letter.is_continuation() => cell.letter.unwrap(),
                        _ => String::new(),
                    })
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect()
    }

    #[test]
    fn a_wide_bar_has_room_for_every_column() {
        let geometry = Geometry::new(160, true);
        assert!(geometry.art);
        assert_eq!(geometry.text_start, 1 + ART_WIDTH + 2);
        assert_eq!(geometry.right_width, RIGHT_MAX_WIDTH);
        let transport = geometry.transport.expect("the transport fits");
        assert_eq!(transport, 80 - TRANSPORT_WIDTH / 2);
        assert!(transport + TRANSPORT_WIDTH + GAP <= geometry.right_start);
        assert_eq!(geometry.text_start + geometry.text_width + GAP, transport);
    }

    #[test]
    fn the_transport_then_the_cover_give_way_as_the_bar_narrows() {
        let mut previous = Geometry::new(160, true);
        for width in (20..160).rev() {
            let geometry = Geometry::new(width, true);
            // Once a column has gone it does not come back at a narrower width.
            assert!(previous.transport.is_some() || geometry.transport.is_none());
            assert!(previous.art || !geometry.art);
            // The seek bar never leaves, and nothing runs off the right edge.
            assert!(geometry.right_start + geometry.right_width < width);
            if geometry.art || geometry.transport.is_some() {
                assert!(geometry.text_width >= MIN_TEXT_WIDTH, "at {width}");
            }
            previous = geometry;
        }
        assert!(!Geometry::new(40, true).art);
        assert!(Geometry::new(40, true).transport.is_none());
    }

    #[test]
    fn the_player_shows_the_track_its_album_and_what_is_next() {
        let bar = statusbar(
            vec![
                track("Say Why", "With Heaven On Top"),
                track("Wait for Me", "Can We Please Have Fun"),
            ],
            Some(0),
            PlayerEvent::Paused(Duration::from_secs(76)),
        );
        let rows = render(bar, 120);
        assert!(rows[0].chars().all(|c| c == '─'), "{:?}", rows[0]);
        assert!(rows[1].contains("Say Why · Zach Bryan"), "{:?}", rows[1]);
        assert!(rows[1].contains("◀◀  ▶"), "{:?}", rows[1]);
        assert!(rows[1].contains("1:16"), "{:?}", rows[1]);
        assert!(rows[1].trim_end().ends_with("2:23"), "{:?}", rows[1]);
        assert!(rows[2].contains("With Heaven On Top"), "{:?}", rows[2]);
        assert!(rows[2].contains("next  Wait for Me"), "{:?}", rows[2]);
        assert!(rows[2].contains("⇄ ↻"), "{:?}", rows[2]);
    }

    #[test]
    fn nothing_playing_still_draws_a_steady_bar() {
        let bar = statusbar(Vec::new(), None, PlayerEvent::Stopped);
        let rows = render(bar, 100);
        assert!(rows[1].contains("Nothing playing"), "{:?}", rows[1]);
        assert!(rows[1].contains("-:--"), "{:?}", rows[1]);
    }

    #[test]
    fn the_equalizer_lies_flat_when_there_is_no_audio() {
        // Paused, so there is nothing to show a level for.
        let paused = statusbar(
            Vec::new(),
            None,
            PlayerEvent::Paused(Duration::from_secs(1)),
        );
        assert!(paused.equalizer(TRANSPORT_WIDTH).is_none());

        // Playing, but no packet has ever reached the tap: a flat line stands in
        // rather than the bars sitting at zero pretending to be a level.
        let playing = statusbar(
            Vec::new(),
            None,
            PlayerEvent::Playing(std::time::SystemTime::now()),
        );
        assert!(playing.equalizer(TRANSPORT_WIDTH).is_none());
    }
}
