use std::sync::Arc;

use cursive::Printer;
use cursive::align::HAlign;
use cursive::event::{Event, EventResult, MouseButton, MouseEvent};
use cursive::theme::{ColorStyle, ColorType, PaletteColor};
use cursive::traits::View;
use cursive::vec::Vec2;
use unicode_width::UnicodeWidthStr;

use crate::events::EventManager;
use crate::library::Library;
use crate::model::playable::Playable;
use crate::queue::{Queue, RepeatSetting};
use crate::spotify::{PlayerEvent, Spotify};
use crate::ui::accent;
use crate::ui::anim::{Animator, DEFAULT_FPS, lift, marquee};
use crate::ui::spectrum::BLOCKS;
use crate::utils::ms_to_hms;
use crate::waveform;

/// How far the progress bar is lifted towards white on the hardest beat. Smaller
/// than the now playing card's, because this line is always on screen.
const BEAT_LIFT: f32 = 0.25;
/// Frames per second the statusbar animates at. Deliberately slower than the now
/// playing card: this line is on screen whatever else you are doing, so it should
/// cost as little as a moving thing can.
const STATUSBAR_FPS: u32 = 12;
/// Bands in the little equalizer that stands in for the play icon.
const EQUALIZER_BANDS: usize = 3;

pub struct StatusBar {
    queue: Arc<Queue>,
    spotify: Spotify,
    library: Arc<Library>,
    last_size: Vec2,
    events: EventManager,
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
            last_size: Vec2::new(0, 0),
            events,
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

    /// A tiny equalizer to stand in for the play icon, or `None` when there is no
    /// audio to draw one from and the icon should be shown instead.
    fn equalizer(&self) -> Option<String> {
        if !self.animated() || !matches!(self.spotify.get_current_status(), PlayerEvent::Playing(_))
        {
            return None;
        }
        let bands = self.spotify.audio_tap().bands(EQUALIZER_BANDS)?;
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

    fn playback_indicator(&self) -> &str {
        let status = self.spotify.get_current_status();
        let nerdfont = self.use_nerdfont();
        let flipped = self
            .library
            .cfg
            .values()
            .flip_status_indicators
            .unwrap_or(false);

        const NF_PLAY: &str = "\u{f04b} ";
        const NF_PAUSE: &str = "\u{f04c} ";
        const NF_STOP: &str = "\u{f04d} ";
        let indicators = match (nerdfont, flipped) {
            (false, false) => ("▶ ", "▮▮", "◼ "),
            (false, true) => ("▮▮", "▶ ", "▶ "),
            (true, false) => (NF_PLAY, NF_PAUSE, NF_STOP),
            (true, true) => (NF_PAUSE, NF_PLAY, NF_PLAY),
        };

        match status {
            PlayerEvent::Playing(_) => indicators.0,
            PlayerEvent::Paused(_) => indicators.1,
            PlayerEvent::Stopped | PlayerEvent::FinishedTrack => indicators.2,
        }
    }

    fn volume_display(&self) -> String {
        format!(
            " [{}%]",
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

    fn format_track(&self, t: &Playable) -> String {
        let format = self
            .library
            .cfg
            .values()
            .statusbar_format
            .clone()
            .unwrap_or_else(|| "%artists - %title".to_string());
        Playable::format(t, &format, &self.library)
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
        let theme_bar = *printer.theme.palette.custom("statusbar_progress").unwrap();
        let tinted = if self.library.cfg.values().cover_accent.unwrap_or(true) {
            accent::current(theme_bar)
        } else {
            theme_bar
        };
        let style_bar = ColorStyle::new(
            ColorType::Color(lift(tinted, BEAT_LIFT * self.pulse())),
            ColorType::Palette(PaletteColor::Background),
        );
        let style_bar_bg = ColorStyle::new(
            ColorType::Color(
                *printer
                    .theme
                    .palette
                    .custom("statusbar_progress_bg")
                    .unwrap(),
            ),
            ColorType::Palette(PaletteColor::Background),
        );
        let style = ColorStyle::new(
            ColorType::Color(*printer.theme.palette.custom("statusbar").unwrap()),
            ColorType::Color(*printer.theme.palette.custom("statusbar_bg").unwrap()),
        );

        printer.print(
            (0, 0),
            &vec![' '; printer.size.x].into_iter().collect::<String>(),
        );
        printer.with_color(style, |printer| {
            printer.print(
                (0, 1),
                &vec![' '; printer.size.x].into_iter().collect::<String>(),
            );
        });

        match self.equalizer() {
            // The equalizer takes the icon's place and the blank cell after it. It
            // is drawn on the statusbar's own background, in the bar's colour.
            Some(bars) => {
                let style = ColorStyle::new(style_bar.front, style.back);
                printer.with_color(style, |printer| printer.print((1, 1), &bars));
            }
            None => printer.with_color(style, |printer| {
                printer.print((1, 1), self.playback_indicator());
            }),
        }

        let updating = if !*self.library.is_done.read().unwrap() {
            if self.use_nerdfont() {
                "\u{f04e6} "
            } else {
                "[U] "
            }
        } else {
            ""
        };

        let repeat = if self.use_nerdfont() {
            match self.queue.get_repeat() {
                RepeatSetting::None => "",
                RepeatSetting::RepeatPlaylist => "\u{f0456} ",
                RepeatSetting::RepeatTrack => "\u{f0458} ",
            }
        } else {
            match self.queue.get_repeat() {
                RepeatSetting::None => "",
                RepeatSetting::RepeatPlaylist => "[R] ",
                RepeatSetting::RepeatTrack => "[R1] ",
            }
        };

        let shuffle = if self.queue.get_shuffle() {
            if self.use_nerdfont() {
                "\u{f049d} "
            } else {
                "[Z] "
            }
        } else {
            ""
        };

        let volume = self.volume_display();

        printer.with_color(style_bar_bg, |printer| {
            printer.print((0, 0), &"┉".repeat(printer.size.x));
        });

        let elapsed = self.spotify.get_current_progress();
        let elapsed_ms = elapsed.as_millis() as u32;

        let formatted_elapsed = ms_to_hms(elapsed.as_millis().try_into().unwrap_or(0));

        let playback_duration_status = match self.queue.get_current() {
            Some(ref t) => format!("{} / {}", formatted_elapsed, t.duration_str()),
            None => "".to_string(),
        };

        let right = updating.to_string()
            + repeat
            + shuffle
            // + saved
            + &playback_duration_status
            + &volume;
        let offset = HAlign::Right.get_offset(right.width(), printer.size.x);

        printer.with_color(style, |printer| {
            if let Some(ref t) = self.queue.get_current() {
                // Stop at the readout on the right instead of running underneath it,
                // and scroll rather than clip when the name does not fit the gap.
                let room = offset.saturating_sub(6);
                printer.print((4, 1), &marquee(&self.format_track(t), room, elapsed));
            }
            printer.print((offset, 1), &right);
        });

        if let Some(t) = self.queue.get_current() {
            printer.with_color(style_bar, |printer| {
                // Halves rather than whole cells, so the bar creeps forward steadily
                // on a long track instead of sitting still and then jumping a cell.
                let halves = elapsed_ms
                    .checked_mul(printer.size.x as u32 * 2)
                    .and_then(|v| v.checked_div(t.duration()))
                    .unwrap_or(0) as usize;
                let full = (halves / 2).min(printer.size.x);
                printer.print((0, 0), &"━".repeat(full + 1));
                if halves % 2 == 1 && full + 1 < printer.size.x {
                    printer.print((full + 1, 0), "╸");
                }
            });
        }
    }

    fn layout(&mut self, size: Vec2) {
        self.last_size = size;
    }

    fn required_size(&mut self, constraint: Vec2) -> Vec2 {
        Vec2::new(constraint.x, 2)
    }

    fn on_event(&mut self, event: Event) -> EventResult {
        if let Event::Mouse {
            offset,
            position,
            event,
        } = event
        {
            let position = position - offset;
            let volume_len = self.volume_display().len();

            if position.y == 0 {
                if event == MouseEvent::WheelUp {
                    self.spotify.seek_relative(-500);
                }

                if event == MouseEvent::WheelDown {
                    self.spotify.seek_relative(500);
                }

                if event == MouseEvent::Press(MouseButton::Left)
                    && let Some(playable) = self.queue.get_current()
                {
                    let f: f32 = position.x as f32 / self.last_size.x as f32;
                    let new = playable.duration() as f32 * f;
                    self.spotify.seek(new as u32);
                }
            } else if self.last_size.x - position.x < volume_len {
                if event == MouseEvent::WheelUp {
                    let volume = self
                        .spotify
                        .volume()
                        .saturating_add(crate::spotify::VOLUME_PERCENT);

                    self.spotify.set_volume(volume, true);
                }

                if event == MouseEvent::WheelDown {
                    let volume = self
                        .spotify
                        .volume()
                        .saturating_sub(crate::spotify::VOLUME_PERCENT);

                    self.spotify.set_volume(volume, true);
                }
            } else if event == MouseEvent::Press(MouseButton::Left) {
                self.queue.toggleplayback();
            }

            EventResult::Consumed(None)
        } else {
            EventResult::Ignored
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use crate::config::Config;
    use crate::events::EventManager;
    use crate::library::Library;
    use crate::queue::Queue;
    use crate::spotify::{PlayerEvent, Spotify};

    use super::StatusBar;

    fn statusbar(status: PlayerEvent) -> StatusBar {
        let cfg = Config::new_for_test();
        let ev = EventManager::new_for_test();
        let spotify = Spotify::new_for_test(cfg.clone(), ev.clone());
        let library = Library::new_for_test(ev.clone(), spotify.clone(), cfg.clone());
        spotify.update_status(status);
        let queue = Arc::new(Queue::new_for_test(
            Vec::new(),
            None,
            spotify,
            cfg,
            library.clone(),
        ));
        StatusBar::new(queue, library, ev)
    }

    #[test]
    fn the_equalizer_gives_way_to_the_icon_when_there_is_no_audio() {
        // Paused, so there is nothing to show a level for.
        let paused = statusbar(PlayerEvent::Paused(Duration::from_secs(1)));
        assert!(paused.equalizer().is_none());
        assert_eq!(paused.playback_indicator(), "\u{25ae}\u{25ae}");

        // Playing, but no packet has ever reached the tap: the icon stands in
        // rather than the bars sitting at zero pretending to be a level.
        let playing = statusbar(PlayerEvent::Playing(std::time::SystemTime::now()));
        assert!(playing.equalizer().is_none());
    }
}
