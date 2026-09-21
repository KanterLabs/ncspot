use std::cmp::min;
use std::sync::{Arc, RwLock};

use cursive::align::HAlign;
use cursive::event::{Event, EventResult, MouseButton, MouseEvent};
use cursive::theme::{ColorStyle, ColorType, PaletteColor};
use cursive::{Cursive, Printer, Vec2, View};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::command::{Command, GotoMode};
use crate::commands::CommandResult;
use crate::library::Library;
use crate::model::playable::Playable;
use crate::queue::{Queue, RepeatSetting};
use crate::spotify::{PlayerEvent, Spotify};
use crate::traits::{IntoBoxedViewExt, ListItem, ViewExt};
use crate::ui::album::AlbumView;
use crate::ui::artist::ArtistView;
use crate::utils::ms_to_hms;

#[derive(Clone, Copy)]
struct ProgressHitbox {
    row: usize,
    start: usize,
    width: usize,
}

/// A responsive dashboard for the item currently playing.
pub struct NowPlayingView {
    queue: Arc<Queue>,
    spotify: Spotify,
    library: Arc<Library>,
    progress_hitbox: RwLock<Option<ProgressHitbox>>,
}

impl NowPlayingView {
    pub fn new(queue: Arc<Queue>, library: Arc<Library>) -> Self {
        let spotify = queue.get_spotify();
        Self {
            queue,
            spotify,
            library,
            progress_hitbox: RwLock::new(None),
        }
    }

    fn playing_style(printer: &Printer<'_, '_>) -> ColorStyle {
        ColorStyle::new(
            ColorType::Color(*printer.theme.palette.custom("playing").unwrap()),
            ColorType::Palette(PaletteColor::Background),
        )
    }

    fn accent_style(printer: &Printer<'_, '_>) -> ColorStyle {
        ColorStyle::new(
            ColorType::Color(*printer.theme.palette.custom("statusbar_progress").unwrap()),
            ColorType::Palette(PaletteColor::Background),
        )
    }

    fn draw_centered(
        printer: &Printer<'_, '_>,
        y: usize,
        text: &str,
        max_width: usize,
        style: ColorStyle,
    ) {
        if y >= printer.size.y || printer.size.x == 0 {
            return;
        }

        let text = truncate(text, min(max_width, printer.size.x));
        let offset = HAlign::Center.get_offset(text.width(), printer.size.x);
        printer.with_color(style, |printer| printer.print((offset, y), &text));
    }

    fn draw_empty(&self, printer: &Printer<'_, '_>) {
        let middle = printer.size.y / 2;
        let accent = Self::accent_style(printer);
        Self::draw_centered(printer, middle.saturating_sub(1), "◇", 1, accent);
        Self::draw_centered(
            printer,
            middle.saturating_add(1),
            "Nothing playing yet",
            printer.size.x.saturating_sub(2),
            ColorStyle::primary(),
        );
        Self::draw_centered(
            printer,
            middle.saturating_add(2),
            "Choose something from your library or queue",
            printer.size.x.saturating_sub(2),
            ColorStyle::secondary(),
        );
    }

    fn metadata(playable: &Playable) -> (String, String, String, String) {
        match playable {
            Playable::Track(track) => (
                track.title.clone(),
                track.artists.join(", "),
                track.album.clone().unwrap_or_default(),
                if track.disc_number > 1 {
                    format!(
                        "DISC {}  •  TRACK {}",
                        track.disc_number, track.track_number
                    )
                } else {
                    format!("TRACK {}", track.track_number)
                },
            ),
            Playable::Episode(episode) => (
                episode.name.clone(),
                "PODCAST EPISODE".to_string(),
                episode.release_date.clone(),
                "EPISODE".to_string(),
            ),
        }
    }

    fn status(&self) -> (&'static str, &'static str) {
        let use_nerdfont = self.library.cfg.values().use_nerdfont.unwrap_or(false);
        match self.spotify.get_current_status() {
            PlayerEvent::Playing(_) => {
                if use_nerdfont {
                    ("󰐊", "PLAYING")
                } else {
                    ("▶", "PLAYING")
                }
            }
            PlayerEvent::Paused(_) => {
                if use_nerdfont {
                    ("󰏤", "PAUSED")
                } else {
                    ("Ⅱ", "PAUSED")
                }
            }
            PlayerEvent::Stopped | PlayerEvent::FinishedTrack => ("■", "STOPPED"),
        }
    }

    fn equalizer(&self, width: usize, elapsed_ms: u128) -> String {
        const BARS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
        let playing = matches!(self.spotify.get_current_status(), PlayerEvent::Playing(_));
        (0..width)
            .map(|column| {
                if playing {
                    let phase = (elapsed_ms / 240) as usize;
                    let wave = (column * 5 + phase * 3) % 14;
                    BARS[min(wave, 14 - wave)]
                } else {
                    BARS[1]
                }
            })
            .collect()
    }

    fn draw_compact(&self, printer: &Printer<'_, '_>, playable: &Playable) {
        let (title, byline, album, _) = Self::metadata(playable);
        let elapsed = self.spotify.get_current_progress().as_millis();
        let (icon, state) = self.status();
        let width = printer.size.x.saturating_sub(2);
        let start_y = printer.size.y.saturating_sub(6) / 2;

        Self::draw_centered(
            printer,
            start_y,
            &format!("{icon}  {state}"),
            width,
            Self::playing_style(printer),
        );
        Self::draw_centered(
            printer,
            start_y.saturating_add(2),
            &title,
            width,
            ColorStyle::title_primary(),
        );
        Self::draw_centered(
            printer,
            start_y.saturating_add(3),
            &byline,
            width,
            ColorStyle::primary(),
        );
        if !album.is_empty() {
            Self::draw_centered(
                printer,
                start_y.saturating_add(4),
                &album,
                width,
                ColorStyle::secondary(),
            );
        }
        self.draw_progress(
            printer,
            start_y.saturating_add(6),
            1,
            width,
            elapsed,
            playable.duration(),
        );
    }

    fn draw_progress(
        &self,
        printer: &Printer<'_, '_>,
        row: usize,
        start: usize,
        width: usize,
        elapsed_ms: u128,
        duration_ms: u32,
    ) {
        if row >= printer.size.y || width == 0 || start >= printer.size.x {
            *self.progress_hitbox.write().unwrap() = None;
            return;
        }

        let width = min(width, printer.size.x - start);
        let filled = progress_width(elapsed_ms, duration_ms, width);
        let accent = Self::accent_style(printer);
        printer.with_color(ColorStyle::secondary(), |printer| {
            printer.print((start, row), &"━".repeat(width));
        });
        if filled > 0 {
            printer.with_color(accent, |printer| {
                printer.print((start, row), &"━".repeat(filled));
            });
        }
        *self.progress_hitbox.write().unwrap() = Some(ProgressHitbox { row, start, width });
    }

    fn draw_dashboard(&self, printer: &Printer<'_, '_>, playable: &Playable) {
        let (title, byline, album, kind) = Self::metadata(playable);
        let elapsed = self.spotify.get_current_progress().as_millis();
        let duration = playable.duration();
        let available_width = printer.size.x.saturating_sub(4);
        let card_width = min(78, available_width);
        let card_height = min(17, printer.size.y.saturating_sub(1));
        let left = printer.size.x.saturating_sub(card_width) / 2;
        let top = printer.size.y.saturating_sub(card_height) / 2;
        let inner_width = card_width.saturating_sub(4);
        let right = left.saturating_add(card_width).saturating_sub(1);
        let bottom = top.saturating_add(card_height).saturating_sub(1);
        let border_style = Self::accent_style(printer);

        printer.with_color(border_style, |printer| {
            printer.print((left, top), "╭");
            if card_width > 2 {
                printer.print((left + 1, top), &"─".repeat(card_width - 2));
            }
            printer.print((right, top), "╮");
            for row in top.saturating_add(1)..bottom {
                printer.print((left, row), "│");
                printer.print((right, row), "│");
            }
            printer.print((left, bottom), "╰");
            if card_width > 2 {
                printer.print((left + 1, bottom), &"─".repeat(card_width - 2));
            }
            printer.print((right, bottom), "╯");
        });

        let (icon, state) = self.status();
        let header = format!("{icon}  {state}  •  {kind}");
        Self::draw_centered(
            printer,
            top + 1,
            &header,
            inner_width,
            Self::playing_style(printer),
        );
        Self::draw_centered(
            printer,
            top + 3,
            &self.equalizer(min(31, inner_width), elapsed),
            inner_width,
            border_style,
        );
        Self::draw_centered(
            printer,
            top + 5,
            &title,
            inner_width,
            ColorStyle::title_primary(),
        );
        Self::draw_centered(
            printer,
            top + 6,
            &byline,
            inner_width,
            ColorStyle::primary(),
        );
        if !album.is_empty() {
            Self::draw_centered(
                printer,
                top + 7,
                &album,
                inner_width,
                ColorStyle::secondary(),
            );
        }

        let progress_start = left.saturating_add(3);
        let progress_width = card_width.saturating_sub(6);
        let progress_row = top + 10;
        self.draw_progress(
            printer,
            progress_row,
            progress_start,
            progress_width,
            elapsed,
            duration,
        );
        let elapsed_label = ms_to_hms(elapsed.try_into().unwrap_or(u32::MAX));
        let duration_label = playable.duration_str();
        printer.with_color(ColorStyle::secondary(), |printer| {
            printer.print((progress_start, progress_row + 1), &elapsed_label);
            let duration_x = progress_start
                .saturating_add(progress_width)
                .saturating_sub(duration_label.width());
            printer.print((duration_x, progress_row + 1), &duration_label);
        });

        let repeat = match self.queue.get_repeat() {
            RepeatSetting::None => "repeat off",
            RepeatSetting::RepeatPlaylist => "repeat all",
            RepeatSetting::RepeatTrack => "repeat one",
        };
        let shuffle = if self.queue.get_shuffle() {
            "shuffle on"
        } else {
            "shuffle off"
        };
        let volume = (self.spotify.volume() as f64 / u16::MAX as f64 * 100.0).round() as u16;
        Self::draw_centered(
            printer,
            top + 13,
            &format!("{repeat}   •   {shuffle}   •   volume {volume}%"),
            inner_width,
            ColorStyle::secondary(),
        );
        Self::draw_centered(
            printer,
            top + 15,
            "< previous     Shift+P play / pause     > next",
            inner_width,
            ColorStyle::primary(),
        );
    }
}

impl View for NowPlayingView {
    fn draw(&self, printer: &Printer<'_, '_>) {
        *self.progress_hitbox.write().unwrap() = None;
        if printer.size.x == 0 || printer.size.y == 0 {
            return;
        }

        for row in 0..printer.size.y {
            printer.print_hline((0, row), printer.size.x, " ");
        }

        let Some(playable) = self.queue.get_current() else {
            self.draw_empty(printer);
            return;
        };

        if printer.size.x >= 36 && printer.size.y >= 17 {
            self.draw_dashboard(printer, &playable);
        } else {
            self.draw_compact(printer, &playable);
        }
    }

    fn required_size(&mut self, constraint: Vec2) -> Vec2 {
        constraint
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
        let Some(position) = position.checked_sub(offset) else {
            return EventResult::Ignored;
        };
        let Some(hitbox) = *self.progress_hitbox.read().unwrap() else {
            return EventResult::Ignored;
        };

        if position.y != hitbox.row {
            return EventResult::Ignored;
        }
        match event {
            MouseEvent::Press(MouseButton::Left)
                if position.x >= hitbox.start
                    && position.x < hitbox.start.saturating_add(hitbox.width) =>
            {
                if let Some(playable) = self.queue.get_current() {
                    let offset = position.x - hitbox.start;
                    let target = (playable.duration() as u128 * offset as u128
                        / hitbox.width.max(1) as u128) as u32;
                    self.spotify.seek(target);
                }
                EventResult::consumed()
            }
            MouseEvent::WheelUp => {
                self.spotify.seek_relative(-5000);
                EventResult::consumed()
            }
            MouseEvent::WheelDown => {
                self.spotify.seek_relative(5000);
                EventResult::consumed()
            }
            _ => EventResult::Ignored,
        }
    }
}

impl ViewExt for NowPlayingView {
    fn title(&self) -> String {
        "Now Playing".to_string()
    }

    fn on_command(&mut self, _s: &mut Cursive, cmd: &Command) -> Result<CommandResult, String> {
        match cmd {
            Command::Save => {
                if let Some(mut playable) = self.queue.get_current() {
                    playable.save(&self.library);
                }
            }
            Command::Delete => {
                if let Some(mut playable) = self.queue.get_current() {
                    playable.unsave(&self.library);
                }
            }
            #[cfg(feature = "share_clipboard")]
            Command::Share(_) => {
                if let Some(url) = self
                    .queue
                    .get_current()
                    .and_then(|playable| playable.share_url())
                {
                    crate::sharing::write_share(url).ok();
                }
                return Ok(CommandResult::Consumed(None));
            }
            Command::Goto(mode) => {
                if let Some(playable) = self.queue.get_current() {
                    let queue = self.queue.clone();
                    let library = self.library.clone();
                    match mode {
                        GotoMode::Album => {
                            if let Some(album) = playable.album(&queue) {
                                return Ok(CommandResult::View(
                                    AlbumView::new(queue, library, &album).into_boxed_view_ext(),
                                ));
                            }
                        }
                        GotoMode::Artist => {
                            if let Some(artist) = playable
                                .artists()
                                .and_then(|artists| artists.into_iter().next())
                            {
                                return Ok(CommandResult::View(
                                    ArtistView::new(queue, library, &artist).into_boxed_view_ext(),
                                ));
                            }
                        }
                    }
                }
            }
            _ => return Ok(CommandResult::Ignored),
        }

        Ok(CommandResult::Consumed(None))
    }
}

fn progress_width(elapsed_ms: u128, duration_ms: u32, width: usize) -> usize {
    if duration_ms == 0 || width == 0 {
        return 0;
    }
    min(
        width,
        (elapsed_ms.saturating_mul(width as u128) / duration_ms as u128) as usize,
    )
}

fn truncate(text: &str, max_width: usize) -> String {
    if text.width() <= max_width {
        return text.to_string();
    }
    if max_width == 0 {
        return String::new();
    }

    let target = max_width.saturating_sub(1);
    let mut result = String::new();
    let mut width = 0;
    for character in text.chars() {
        let character_width = character.width().unwrap_or(0);
        if width + character_width > target {
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
    use super::{progress_width, truncate};

    #[test]
    fn progress_is_clamped_and_handles_empty_duration() {
        assert_eq!(progress_width(500, 1000, 20), 10);
        assert_eq!(progress_width(2000, 1000, 20), 20);
        assert_eq!(progress_width(500, 0, 20), 0);
        assert_eq!(progress_width(500, 1000, 0), 0);
    }

    #[test]
    fn truncation_respects_terminal_cell_width() {
        assert_eq!(truncate("hello", 8), "hello");
        assert_eq!(truncate("hello", 4), "hel…");
        assert_eq!(truncate("音楽 test", 5), "音楽…");
        assert_eq!(truncate("hello", 1), "…");
        assert_eq!(truncate("hello", 0), "");
    }
}
