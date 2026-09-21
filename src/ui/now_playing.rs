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
use crate::ui::contextmenu::ContextMenu;
use crate::ui::queue::QueueView;
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

    /// Draw differently styled runs of text as one centered line, so key hints can
    /// highlight the key without splitting the line into separately aligned pieces.
    fn draw_centered_segments(
        printer: &Printer<'_, '_>,
        y: usize,
        segments: &[(String, ColorStyle)],
        max_width: usize,
    ) {
        if y >= printer.size.y || printer.size.x == 0 {
            return;
        }

        let total: usize = segments.iter().map(|(text, _)| text.width()).sum();
        if total > min(max_width, printer.size.x) {
            let flattened: String = segments.iter().map(|(text, _)| text.as_str()).collect();
            let style = segments.first().map(|(_, style)| *style);
            Self::draw_centered(
                printer,
                y,
                &flattened,
                max_width,
                style.unwrap_or_else(ColorStyle::primary),
            );
            return;
        }

        let mut x = HAlign::Center.get_offset(total, printer.size.x);
        for (text, style) in segments {
            printer.with_color(*style, |printer| printer.print((x, y), text));
            x += text.width();
        }
    }

    fn draw_empty(&self, printer: &Printer<'_, '_>) {
        let middle = printer.size.y / 2;
        let accent = Self::accent_style(printer);
        Self::draw_centered(printer, middle.saturating_sub(2), "◇", 1, accent);
        Self::draw_centered(
            printer,
            middle.saturating_sub(1),
            &"▁".repeat(min(9, printer.size.x)),
            printer.size.x,
            ColorStyle::secondary(),
        );
        Self::draw_centered(
            printer,
            middle.saturating_add(1),
            "Nothing playing yet",
            printer.size.x.saturating_sub(2),
            ColorStyle::title_primary(),
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

    fn is_playing(&self) -> bool {
        matches!(self.spotify.get_current_status(), PlayerEvent::Playing(_))
    }

    /// Per-column bar heights (0..=7) of the decorative spectrum. Layered sine waves
    /// under a centered envelope read as a plausible spectrum rather than a sawtooth,
    /// and playback time drives the animation so it freezes while paused.
    fn equalizer(&self, width: usize, elapsed_ms: u128) -> Vec<usize> {
        if width == 0 {
            return Vec::new();
        }
        if !self.is_playing() {
            return vec![0; width];
        }

        let time = elapsed_ms as f64 / 1000.0;
        let center = (width as f64 - 1.0) / 2.0;
        (0..width)
            .map(|column| {
                let x = column as f64;
                let envelope = 1.0 - 0.6 * ((x - center) / (center + 1.0)).powi(2);
                let wave = 0.5 * (x * 0.55 + time * 5.1).sin()
                    + 0.3 * (x * 1.31 - time * 3.3).sin()
                    + 0.2 * (x * 0.23 + time * 7.9).sin();
                let level = (wave * 0.5 + 0.5) * envelope * 7.0;
                level.round().clamp(0.0, 7.0) as usize
            })
            .collect()
    }

    fn draw_equalizer(
        &self,
        printer: &Printer<'_, '_>,
        row: usize,
        width: usize,
        elapsed_ms: u128,
    ) {
        const BARS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
        let levels = self.equalizer(width, elapsed_ms);
        if levels.is_empty() || row >= printer.size.y {
            return;
        }

        let accent = Self::accent_style(printer);
        let playing = Self::playing_style(printer);
        let quiet = ColorStyle::secondary();
        let origin = HAlign::Center.get_offset(levels.len(), printer.size.x);
        for (offset, level) in levels.into_iter().enumerate() {
            let x = origin + offset;
            let style = match level {
                0..=2 => quiet,
                3..=5 => accent,
                _ => playing,
            };
            printer.with_color(style, |printer| {
                printer.print((x, row), &BARS[level].to_string());
            });
        }
    }

    fn draw_compact(&self, printer: &Printer<'_, '_>, playable: &Playable) {
        let (title, byline, album, _) = Self::metadata(playable);
        let elapsed = self.spotify.get_current_progress().as_millis();
        let (icon, state) = self.status();
        let width = printer.size.x.saturating_sub(2);
        let start_y = printer.size.y.saturating_sub(7) / 2;

        Self::draw_centered_segments(
            printer,
            start_y,
            &[
                (format!("{icon}  "), Self::playing_style(printer)),
                (state.to_string(), ColorStyle::secondary()),
            ],
            width,
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
        let progress_row = start_y.saturating_add(6);
        self.draw_progress(
            printer,
            progress_row,
            1,
            width,
            elapsed,
            playable.duration(),
        );
        self.draw_times(printer, progress_row + 1, 1, width, elapsed, playable);
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

        const PARTIALS: [char; 8] = ['▏', '▎', '▍', '▌', '▋', '▊', '▉', '█'];
        let width = min(width, printer.size.x - start);
        let eighths = progress_eighths(elapsed_ms, duration_ms, width);
        let filled = eighths / 8;
        let remainder = eighths % 8;
        let accent = Self::accent_style(printer);

        printer.with_color(ColorStyle::secondary(), |printer| {
            printer.print((start, row), &"┈".repeat(width));
        });
        printer.with_color(accent, |printer| {
            if filled > 0 {
                printer.print((start, row), &"█".repeat(filled));
            }
            if remainder > 0 && filled < width {
                printer.print((start + filled, row), &PARTIALS[remainder - 1].to_string());
            }
        });
        // A playhead makes the seek target obvious, and marks where a click will land.
        if width > 1 {
            let head = min(filled, width - 1);
            printer.with_color(Self::playing_style(printer), |printer| {
                printer.print((start + head, row), "●");
            });
        }

        *self.progress_hitbox.write().unwrap() = Some(ProgressHitbox { row, start, width });
    }

    fn draw_times(
        &self,
        printer: &Printer<'_, '_>,
        row: usize,
        start: usize,
        width: usize,
        elapsed_ms: u128,
        playable: &Playable,
    ) {
        if row >= printer.size.y || width == 0 || start >= printer.size.x {
            return;
        }

        let width = min(width, printer.size.x - start);
        let elapsed_label = ms_to_hms(elapsed_ms.try_into().unwrap_or(u32::MAX));
        let duration_label = playable.duration_str();
        let percent = percent_complete(elapsed_ms, playable.duration());

        printer.with_color(ColorStyle::secondary(), |printer| {
            printer.print((start, row), &elapsed_label);
            let duration_x = start
                .saturating_add(width)
                .saturating_sub(duration_label.width());
            printer.print((duration_x, row), &duration_label);
        });

        let percent_label = format!("{percent}%");
        let used = elapsed_label.width() + duration_label.width() + percent_label.width() + 4;
        if used <= width {
            let offset = start + (width - percent_label.width()) / 2;
            printer.with_color(Self::accent_style(printer), |printer| {
                printer.print((offset, row), &percent_label);
            });
        }
    }

    /// Rounded card with the transport state inlaid into the top border.
    fn draw_card(&self, printer: &Printer<'_, '_>, rect: (usize, usize, usize, usize), chip: &str) {
        let (left, top, right, bottom) = rect;
        let card_width = right.saturating_sub(left) + 1;
        let border = Self::accent_style(printer);

        printer.with_color(border, |printer| {
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

        let chip = truncate(chip, card_width.saturating_sub(8));
        if !chip.is_empty() && card_width > chip.width() + 8 {
            let offset = left + (card_width - chip.width() - 2) / 2;
            printer.with_color(border, |printer| {
                printer.print((offset, top), " ");
                printer.print((offset + chip.width() + 1, top), " ");
            });
            printer.with_color(Self::playing_style(printer), |printer| {
                printer.print((offset + 1, top), &chip);
            });
        }
    }

    fn draw_rule(&self, printer: &Printer<'_, '_>, row: usize, left: usize, right: usize) {
        if row >= printer.size.y || right <= left + 1 {
            return;
        }
        printer.with_color(ColorStyle::secondary(), |printer| {
            printer.print((left, row), "├");
            printer.print((left + 1, row), &"─".repeat(right - left - 1));
            printer.print((right, row), "┤");
        });
    }

    fn draw_dashboard(&self, printer: &Printer<'_, '_>, playable: &Playable) {
        let (title, byline, album, kind) = Self::metadata(playable);
        let elapsed = self.spotify.get_current_progress().as_millis();
        let available_width = printer.size.x.saturating_sub(4);
        let card_width = min(78, available_width);
        let card_height = min(17, printer.size.y.saturating_sub(1));
        let left = printer.size.x.saturating_sub(card_width) / 2;
        let top = printer.size.y.saturating_sub(card_height) / 2;
        let inner_width = card_width.saturating_sub(4);
        let right = left.saturating_add(card_width).saturating_sub(1);
        let bottom = top.saturating_add(card_height).saturating_sub(1);

        let (icon, state) = self.status();
        self.draw_card(
            printer,
            (left, top, right, bottom),
            &format!("{icon}  {state}"),
        );

        self.draw_equalizer(printer, top + 2, min(31, inner_width), elapsed);
        Self::draw_centered(
            printer,
            top + 4,
            &title,
            inner_width,
            ColorStyle::title_primary(),
        );
        Self::draw_centered(
            printer,
            top + 5,
            &byline,
            inner_width,
            ColorStyle::primary(),
        );
        if !album.is_empty() {
            Self::draw_centered(
                printer,
                top + 6,
                &album,
                inner_width,
                ColorStyle::secondary(),
            );
        }

        let saved = if self.library.is_saved_track(playable) {
            "  •  ♥ saved"
        } else {
            ""
        };
        Self::draw_centered(
            printer,
            top + 8,
            &format!("{kind}{saved}"),
            inner_width,
            ColorStyle::secondary(),
        );

        let progress_start = left.saturating_add(3);
        let progress_width = card_width.saturating_sub(6);
        let progress_row = top + 10;
        self.draw_progress(
            printer,
            progress_row,
            progress_start,
            progress_width,
            elapsed,
            playable.duration(),
        );
        self.draw_times(
            printer,
            progress_row + 1,
            progress_start,
            progress_width,
            elapsed,
            playable,
        );

        self.draw_rule(printer, top + 12, left, right);
        Self::draw_centered_segments(
            printer,
            top + 13,
            &self.transport_segments(printer),
            inner_width,
        );
        Self::draw_centered_segments(
            printer,
            top + 15,
            &[
                ("<".to_string(), Self::accent_style(printer)),
                (" previous     ".to_string(), ColorStyle::secondary()),
                ("Shift+P".to_string(), Self::accent_style(printer)),
                (" play / pause     ".to_string(), ColorStyle::secondary()),
                (">".to_string(), Self::accent_style(printer)),
                (" next".to_string(), ColorStyle::secondary()),
            ],
            inner_width,
        );
    }

    /// Repeat / shuffle / volume, with active toggles highlighted.
    fn transport_segments(&self, printer: &Printer<'_, '_>) -> Vec<(String, ColorStyle)> {
        let on = Self::playing_style(printer);
        let off = ColorStyle::secondary();
        let separator = ("   •   ".to_string(), ColorStyle::secondary());

        let (repeat, repeat_style) = match self.queue.get_repeat() {
            RepeatSetting::None => ("repeat off", off),
            RepeatSetting::RepeatPlaylist => ("repeat all", on),
            RepeatSetting::RepeatTrack => ("repeat one", on),
        };
        let (shuffle, shuffle_style) = if self.queue.get_shuffle() {
            ("shuffle on", on)
        } else {
            ("shuffle off", off)
        };
        let volume = (self.spotify.volume() as f64 / u16::MAX as f64 * 100.0).round() as u16;

        vec![
            (repeat.to_string(), repeat_style),
            separator.clone(),
            (shuffle.to_string(), shuffle_style),
            separator,
            (format!("volume {volume}%"), Self::accent_style(printer)),
        ]
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

    fn on_command(&mut self, s: &mut Cursive, cmd: &Command) -> Result<CommandResult, String> {
        match cmd {
            Command::Play => {
                self.queue.toggleplayback();
            }
            Command::Queue => {
                if let Some(mut playable) = self.queue.get_current() {
                    playable.queue(&self.queue);
                }
            }
            Command::PlayNext => {
                if let Some(mut playable) = self.queue.get_current() {
                    playable.play_next(&self.queue);
                }
            }
            Command::Save => {
                if let Some(mut playable) = self.queue.get_current() {
                    playable.save(&self.library);
                }
            }
            Command::SaveQueue => {
                s.add_layer(QueueView::save_dialog(
                    self.queue.clone(),
                    self.library.clone(),
                ));
            }
            Command::Add => {
                if let Some(track) = self
                    .queue
                    .get_current()
                    .and_then(|playable| playable.track())
                {
                    return Ok(CommandResult::Modal(Box::new(
                        ContextMenu::add_track_dialog(
                            self.library.clone(),
                            self.queue.get_spotify(),
                            track,
                        ),
                    )));
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
            Command::Open(_) => {
                if let Some(playable) = self.queue.get_current() {
                    let target = playable.as_listitem();
                    let queue = self.queue.clone();
                    let library = self.library.clone();
                    if let Some(view) = target.open(queue.clone(), library.clone()) {
                        return Ok(CommandResult::View(view));
                    }
                    return Ok(CommandResult::Modal(Box::new(ContextMenu::new(
                        target.as_ref(),
                        queue,
                        library,
                    ))));
                }
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
            Command::ShowRecommendations(_) => {
                if let Some(playable) = self.queue.get_current() {
                    let mut target = playable.as_listitem();
                    if let Some(view) =
                        target.open_recommendations(self.queue.clone(), self.library.clone())
                    {
                        return Ok(CommandResult::View(view));
                    }
                }
            }
            // A single-item dashboard has no selection to move, search, sort, or shift. Treat
            // these list-only commands as successful no-ops so shared keybindings remain quiet.
            Command::Move(_, _)
            | Command::Shift(_, _)
            | Command::Jump(_)
            | Command::Insert(_)
            | Command::Sort(_, _) => {}
            _ => return Ok(CommandResult::Ignored),
        }

        Ok(CommandResult::Consumed(None))
    }
}

/// Progress in eighths of a cell, so the bar can render sub-cell partial blocks.
fn progress_eighths(elapsed_ms: u128, duration_ms: u32, width: usize) -> usize {
    if duration_ms == 0 || width == 0 {
        return 0;
    }
    min(
        width * 8,
        (elapsed_ms.saturating_mul(width as u128 * 8) / duration_ms as u128) as usize,
    )
}

fn percent_complete(elapsed_ms: u128, duration_ms: u32) -> u8 {
    if duration_ms == 0 {
        return 0;
    }
    min(100, elapsed_ms.saturating_mul(100) / duration_ms as u128) as u8
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
    use super::{percent_complete, progress_eighths, truncate};

    #[test]
    fn progress_is_clamped_and_handles_empty_duration() {
        assert_eq!(progress_eighths(500, 1000, 20), 80);
        assert_eq!(progress_eighths(2000, 1000, 20), 160);
        assert_eq!(progress_eighths(500, 0, 20), 0);
        assert_eq!(progress_eighths(500, 1000, 0), 0);
    }

    #[test]
    fn progress_resolves_below_a_single_cell() {
        // A third of the way into the first of 20 cells: two eighths, no full cell.
        assert_eq!(progress_eighths(17, 1000, 20), 2);
    }

    #[test]
    fn percent_is_clamped_and_handles_empty_duration() {
        assert_eq!(percent_complete(500, 1000), 50);
        assert_eq!(percent_complete(5000, 1000), 100);
        assert_eq!(percent_complete(500, 0), 0);
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
