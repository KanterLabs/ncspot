use std::cmp::min;
use std::collections::hash_map::DefaultHasher;
use std::f64::consts::{PI, TAU};
use std::hash::{Hash, Hasher};
use std::sync::{Arc, RwLock, Weak};
use std::thread;
use std::time::{Duration, Instant};

use cursive::align::HAlign;
use cursive::event::{Event, EventResult, MouseButton, MouseEvent};
use cursive::theme::{ColorStyle, ColorType, Effect, PaletteColor};
use cursive::{Cursive, Printer, Vec2, View};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::command::{Command, GotoMode};
use crate::commands::CommandResult;
use crate::events::EventManager;
use crate::library::Library;
use crate::model::playable::Playable;
use crate::queue::{Queue, RepeatSetting};
use crate::spotify::{PlayerEvent, Spotify, VOLUME_PERCENT};
use crate::traits::{IntoBoxedViewExt, ListItem, ViewExt};
use crate::ui::album::AlbumView;
use crate::ui::artist::ArtistView;
use crate::ui::contextmenu::ContextMenu;
use crate::ui::modal::Modal;
use crate::ui::queue::QueueView;
use crate::ui::quick_search::QuickSearch;
use crate::utils::ms_to_hms;

/// Eighth-block glyphs, used both for the spectrum and for sub-cell progress.
const BLOCKS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
const CARD_MAX_WIDTH: usize = 78;
const SPECTRUM_MAX_WIDTH: usize = 61;
const SPECTRUM_MAX_HEIGHT: usize = 3;
/// Tempo the spectrum pulses at. Nothing reports the real tempo, so the band
/// pumps at a plausible mid tempo rather than pretending to follow the track.
const SPECTRUM_TEMPO: f64 = 112.0;
/// Seconds a bar takes to close most of the gap up to, and back down from, its
/// target: hits land immediately, decay is slow, like a real meter.
const BAR_RISE: f64 = 0.05;
const BAR_FALL: f64 = 0.30;
/// Downward acceleration of a peak marker, in eighths of a cell per second squared.
const PEAK_GRAVITY: f64 = 30.0;
/// Longest frame gap the animation will integrate over, so a view that was hidden
/// for a while eases back in instead of snapping.
const MAX_FRAME_GAP: f64 = 0.25;
/// Frames per second the visualizer animates at, unless `visualizer_fps` says
/// otherwise. Fast enough to look fluid, slow enough to stay cheap.
const DEFAULT_FPS: u32 = 20;
/// Bounds for `visualizer_fps`, so a typo cannot melt a CPU or stall the animation.
const FPS_RANGE: std::ops::RangeInclusive<u32> = 1..=60;
/// How often the animation thread re-checks an idle view.
const IDLE_INTERVAL: Duration = Duration::from_millis(250);
/// A view drawn this recently is assumed to still be on screen.
const VISIBLE_FOR: Duration = Duration::from_millis(500);
/// How long frames keep being drawn after playback stops, so the band can settle.
const SETTLE_FOR: Duration = Duration::from_millis(900);
const VOLUME_METER_CELLS: usize = 8;
/// Cover art sizing, in cells: never taller than this, never shown below it, and
/// never at the cost of leaving the text column narrower than it needs.
#[cfg(feature = "album_art")]
const ART_MAX_HEIGHT: usize = 14;
#[cfg(feature = "album_art")]
const ART_MIN_HEIGHT: usize = 5;
/// The text column never gets narrower than the transport line, so the controls
/// stay whole and clickable however big the cover is.
#[cfg(feature = "album_art")]
const ART_MIN_TEXT_WIDTH: usize = 48;
/// Smallest terminal that still gets the full card; anything smaller is laid out flat.
const CARD_MIN_WIDTH: usize = 40;
const CARD_MIN_HEIGHT: usize = 15;

/// Drives the visualizer while it is on screen.
///
/// The rest of ncspot only redraws every few hundred milliseconds, which is far too
/// coarse for an animation, so this asks the event loop for extra frames — but only
/// while the view is actually being drawn and something is playing, so a background
/// tab never costs anything.
#[derive(Default)]
struct Animator {
    last_draw: RwLock<Option<Instant>>,
}

impl Animator {
    /// Start animating at `fps`. Zero frames per second means the user turned the
    /// animation off: the band still draws, it just never asks for a frame.
    fn spawn(events: EventManager, spotify: Spotify, fps: u32) -> Arc<Self> {
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

    fn mark_drawn(&self) {
        *self.last_draw.write().unwrap() = Some(Instant::now());
    }

    fn is_visible(&self) -> bool {
        self.last_draw
            .read()
            .unwrap()
            .is_some_and(|at| at.elapsed() < VISIBLE_FOR)
    }
}

/// Bar and peak marker animation, carried between frames. Heights are in eighths
/// of a cell; motion is integrated over real elapsed time so the animation looks
/// the same whether the terminal is redrawing at two frames a second or twenty.
#[derive(Default)]
struct SpectrumState {
    levels: Vec<f64>,
    peaks: Vec<f64>,
    /// Current downward speed of each peak marker, in eighths per second.
    peak_speed: Vec<f64>,
    last_frame: Option<Instant>,
}

impl SpectrumState {
    /// Advance the animation to now, from however long ago the last frame was.
    fn step(&mut self, targets: &[f64]) {
        let now = Instant::now();
        let elapsed = self
            .last_frame
            .map(|last| now.duration_since(last).as_secs_f64())
            .unwrap_or_default()
            .clamp(0.0, MAX_FRAME_GAP);
        self.last_frame = Some(now);
        self.advance(targets, elapsed);
    }

    /// Ease every bar toward its target and let the peak markers fall, over
    /// `elapsed` seconds.
    fn advance(&mut self, targets: &[f64], elapsed: f64) {
        // On the first frame, and after a resize, snap to the targets: a single
        // redraw (a paused view, or a frame triggered by something else entirely)
        // has to show the band, not an empty strip easing up from nothing.
        if self.levels.len() != targets.len() {
            self.levels = targets.to_vec();
            self.peaks = targets.to_vec();
            self.peak_speed = vec![0.0; targets.len()];
            return;
        }

        let rise = 1.0 - (-elapsed / BAR_RISE).exp();
        let fall = 1.0 - (-elapsed / BAR_FALL).exp();
        for (column, target) in targets.iter().enumerate() {
            let level = &mut self.levels[column];
            *level += (target - *level) * if *target > *level { rise } else { fall };

            if *level >= self.peaks[column] {
                self.peaks[column] = *level;
                self.peak_speed[column] = 0.0;
            } else {
                self.peak_speed[column] += PEAK_GRAVITY * elapsed;
                self.peaks[column] =
                    (self.peaks[column] - self.peak_speed[column] * elapsed).max(*level);
            }
        }
    }
}

/// What a click on a given run of cells does. The card advertises these controls,
/// so they have to be usable with the mouse as well as with the keyboard.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Control {
    /// Seek to the clicked fraction of the track.
    Seek,
    Previous,
    PlayPause,
    Next,
    Repeat,
    Shuffle,
    /// Set the volume to the clicked fraction of the meter.
    Volume,
}

/// A run of cells on one row that responds to the mouse. Rebuilt on every draw,
/// so the hit areas always match what is actually on screen.
#[derive(Clone, Copy)]
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

    /// Where in the hitbox the click landed, as a fraction of its width.
    fn fraction(&self, position: Vec2) -> f64 {
        if self.width <= 1 {
            return 0.0;
        }
        (position.x - self.start) as f64 / (self.width - 1) as f64
    }
}

/// A styled run of text, optionally wired to a [`Control`].
struct Segment {
    text: String,
    style: ColorStyle,
    control: Option<Control>,
}

impl Segment {
    fn new(text: impl Into<String>, style: ColorStyle) -> Self {
        Self {
            text: text.into(),
            style,
            control: None,
        }
    }

    fn button(text: impl Into<String>, style: ColorStyle, control: Control) -> Self {
        Self {
            text: text.into(),
            style,
            control: Some(control),
        }
    }
}

/// A horizontal slice of the screen that content is laid out inside. Keeping the
/// slice explicit means the card contents centre on the card, not on the terminal.
#[derive(Clone, Copy)]
struct Region {
    start: usize,
    width: usize,
}

impl Region {
    fn new(start: usize, width: usize) -> Self {
        Self { start, width }
    }

    /// Clip to what the printer can actually show, or `None` if nothing is visible.
    fn visible(self, printer: &Printer<'_, '_>) -> Option<Self> {
        if self.start >= printer.size.x || self.width == 0 {
            return None;
        }
        Some(Self::new(
            self.start,
            min(self.width, printer.size.x - self.start),
        ))
    }

    /// The x position at which `content_width` cells are centred in this region.
    fn center(self, content_width: usize) -> usize {
        self.start + HAlign::Center.get_offset(content_width, self.width)
    }
}

/// Where a stack of blocks is drawn: the first row, the number of rows available,
/// the slice content is centered in, and the card edges that a rule spans.
#[derive(Clone, Copy)]
struct Frame {
    top: usize,
    height: usize,
    region: Region,
    edges: Option<(usize, usize)>,
}

/// One row-group of the dashboard. Blocks stack top to bottom inside the card; when
/// the terminal is too short, blocks with the highest `drop_order` go first, so the
/// layout degrades by shedding decoration before it sheds information.
struct Block {
    kind: BlockKind,
    drop_order: u8,
}

enum BlockKind {
    Blank,
    /// Decorative spectrum, `usize` rows tall. Shrinks a row at a time before it is dropped.
    Spectrum(usize),
    Line {
        text: String,
        style: ColorStyle,
        bold: bool,
    },
    Segments(Vec<Segment>),
    Progress,
    Times,
    Rule,
}

impl Block {
    fn new(kind: BlockKind, drop_order: u8) -> Self {
        Self { kind, drop_order }
    }

    fn blank() -> Self {
        Self::new(BlockKind::Blank, 10)
    }

    fn line(text: impl Into<String>, style: ColorStyle, drop_order: u8) -> Self {
        Self::new(
            BlockKind::Line {
                text: text.into(),
                style,
                bold: false,
            },
            drop_order,
        )
    }

    fn title(text: impl Into<String>) -> Self {
        Self::new(
            BlockKind::Line {
                text: text.into(),
                style: ColorStyle::title_primary(),
                bold: true,
            },
            0,
        )
    }

    fn height(&self) -> usize {
        match self.kind {
            BlockKind::Spectrum(rows) => rows,
            _ => 1,
        }
    }
}

fn blocks_height(blocks: &[Block]) -> usize {
    blocks.iter().map(Block::height).sum()
}

/// Shrink the layout until it fits `height` rows: repeatedly take the most
/// droppable block (bottom-most one, when several tie) and either shorten it, if it
/// is the spectrum, or remove it. Blocks at drop order 0 are never removed.
fn fit_blocks(blocks: &mut Vec<Block>, height: usize) {
    while blocks_height(blocks) > height {
        let Some(order) = blocks
            .iter()
            .map(|block| block.drop_order)
            .max()
            .filter(|order| *order > 0)
        else {
            return;
        };
        let index = blocks
            .iter()
            .rposition(|block| block.drop_order == order)
            .expect("drop order came from this list");
        if let BlockKind::Spectrum(rows) = &mut blocks[index].kind
            && *rows > 1
        {
            *rows -= 1;
            continue;
        }
        blocks.remove(index);
    }
}

/// A responsive dashboard for the item currently playing.
pub struct NowPlayingView {
    queue: Arc<Queue>,
    spotify: Spotify,
    library: Arc<Library>,
    hitboxes: RwLock<Vec<Hitbox>>,
    spectrum: RwLock<SpectrumState>,
    animator: Arc<Animator>,
    events: EventManager,
    #[cfg(feature = "album_art")]
    art: crate::ui::album_art::AlbumArt,
}

impl NowPlayingView {
    pub fn new(queue: Arc<Queue>, library: Arc<Library>, events: EventManager) -> Self {
        let spotify = queue.get_spotify();
        let fps = library.cfg.values().visualizer_fps.unwrap_or(DEFAULT_FPS);
        let animator = Animator::spawn(events.clone(), spotify.clone(), fps);
        #[cfg(feature = "album_art")]
        let events_for_art = events.clone();
        Self {
            queue,
            spotify,
            library,
            events,
            hitboxes: RwLock::new(Vec::new()),
            spectrum: RwLock::new(SpectrumState::default()),
            animator,
            #[cfg(feature = "album_art")]
            art: crate::ui::album_art::AlbumArt::new(events_for_art),
        }
    }

    fn use_nerdfont(&self) -> bool {
        self.library.cfg.values().use_nerdfont.unwrap_or(false)
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

    fn draw_line(
        printer: &Printer<'_, '_>,
        y: usize,
        region: Region,
        text: &str,
        style: ColorStyle,
        bold: bool,
    ) {
        let Some(region) = region.visible(printer) else {
            return;
        };
        if y >= printer.size.y {
            return;
        }

        let text = truncate(text, region.width);
        let x = region.center(text.width());
        printer.with_color(style, |printer| {
            if bold {
                printer.with_effect(Effect::Bold, |printer| printer.print((x, y), &text));
            } else {
                printer.print((x, y), &text);
            }
        });
    }

    /// Draw differently styled runs of text as one centered line, so key hints can
    /// highlight the key without splitting the line into separately aligned pieces.
    fn draw_segments(
        &self,
        printer: &Printer<'_, '_>,
        y: usize,
        region: Region,
        segments: &[Segment],
    ) {
        let Some(region) = region.visible(printer) else {
            return;
        };
        if y >= printer.size.y {
            return;
        }

        let total: usize = segments.iter().map(|segment| segment.text.width()).sum();
        if total > region.width {
            // Too narrow to place the runs individually: flatten and truncate, and
            // register no hit areas, since none of them would line up any more.
            let flattened: String = segments
                .iter()
                .map(|segment| segment.text.as_str())
                .collect();
            let style = segments
                .first()
                .map(|segment| segment.style)
                .unwrap_or_else(ColorStyle::primary);
            Self::draw_line(printer, y, region, &flattened, style, false);
            return;
        }

        let mut x = region.center(total);
        for segment in segments {
            printer.with_color(segment.style, |printer| {
                printer.print((x, y), &segment.text)
            });
            let width = segment.text.width();
            if let Some(control) = segment.control {
                self.hitboxes.write().unwrap().push(Hitbox {
                    row: y,
                    start: x,
                    width,
                    control,
                });
            }
            x += width;
        }
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
        let use_nerdfont = self.use_nerdfont();
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

    /// The item queued after the current one, formatted for a one line preview.
    fn next_up(&self) -> Option<String> {
        let index = self.queue.next_index()?;
        // Read the item out and release the lock immediately: drawing a preview must
        // never hold the queue lock while the rest of the card renders.
        let next = self.queue.queue.read().unwrap().get(index).cloned()?;
        let (title, byline, _, _) = Self::metadata(&next);
        Some(if byline.is_empty() {
            title
        } else {
            format!("{byline} – {title}")
        })
    }

    /// Position of the current item in the queue, as a `4 / 27` style label.
    fn queue_position(&self) -> Option<String> {
        let index = self.queue.get_current_index()?;
        let len = self.queue.len();
        (len > 1).then(|| format!("{} / {}", index + 1, len))
    }

    /// Target bar heights for the spectrum, in eighths of a cell.
    ///
    /// Nothing in ncspot can see the audio, so the band is synthesised: layered slow
    /// waves keep neighbouring columns correlated, so it moves like a spectrum rather
    /// than like static; a centred envelope puts the tallest bars in the middle; and a
    /// beat pulse makes the whole band pump. Playback time drives it, so pausing
    /// freezes the pattern in place and ducks it to a low idle band. `seed` shifts the
    /// pattern per track, so two tracks never animate identically.
    fn spectrum_targets(&self, width: usize, rows: usize, elapsed_ms: u128, seed: f64) -> Vec<f64> {
        if width == 0 || rows == 0 {
            return Vec::new();
        }

        let gain = if self.is_playing() { 1.0 } else { 0.45 };
        let time = elapsed_ms as f64 / 1000.0;
        let ceiling = (rows * 8) as f64;
        let center = (width as f64 - 1.0) / 2.0;
        let beat = (time * SPECTRUM_TEMPO / 60.0 * PI).sin().abs().powi(6);

        (0..width)
            .map(|column| {
                let x = column as f64;
                let offset = (x - center) / (center + 1.0);
                let envelope = 1.0 - 0.8 * offset * offset;
                // Three slow waves shape the band, a faster one adds per column
                // detail; the weights sum to one, so `wave` stays within ±1.
                let wave = 0.36 * (x * 0.21 + time * 2.3 + seed).sin()
                    + 0.28 * (x * 0.37 - time * 1.7 + seed * 1.7).sin()
                    + 0.20 * (x * 0.09 + time * 3.9 + seed * 0.3).sin()
                    + 0.16 * (x * 0.87 + time * 5.3 + seed * 2.3).sin();
                // Biasing the curve downward keeps the valleys low, so the band
                // reads as separate bars rather than as one solid block.
                let shape = (wave * 0.5 + 0.5).clamp(0.0, 1.0).powf(1.7);
                // Overdriven on purpose: loud columns clip at the top of the band,
                // the way a meter pins, instead of never reaching it.
                let level = (0.04 + 1.5 * shape) * (0.78 + 0.34 * beat) * envelope * ceiling * gain;
                level.clamp(0.0, ceiling)
            })
            .collect()
    }

    /// A stable per track phase offset, so each track gets its own pattern.
    fn spectrum_seed(playable: &Playable) -> f64 {
        let mut hasher = DefaultHasher::new();
        playable.uri().hash(&mut hasher);
        (hasher.finish() % 10_000) as f64 / 10_000.0 * TAU
    }

    fn draw_spectrum(
        &self,
        printer: &Printer<'_, '_>,
        top: usize,
        rows: usize,
        region: Region,
        elapsed_ms: u128,
        seed: f64,
    ) {
        let Some(region) = region.visible(printer) else {
            return;
        };
        let width = min(SPECTRUM_MAX_WIDTH, region.width);
        let targets = self.spectrum_targets(width, rows, elapsed_ms, seed);
        if targets.is_empty() {
            return;
        }

        let mut state = self.spectrum.write().unwrap();
        state.step(&targets);

        let accent = Self::accent_style(printer);
        let playing = Self::playing_style(printer);
        let quiet = ColorStyle::secondary();
        let playing_now = self.is_playing();
        let baseline = top + rows - 1;
        let origin = region.center(width);

        for column in 0..width {
            let x = origin + column;
            let level = state.levels[column].round().max(0.0) as usize;
            let peak = state.peaks[column].round().max(0.0) as usize;

            // A dim floor frames the band, and keeps it readable as a band when a
            // column has decayed away to nothing.
            if level == 0 && baseline < printer.size.y {
                printer.with_color(quiet, |printer| printer.print((x, baseline), "▁"));
            }

            for tier in 0..rows {
                let y = top + rows - 1 - tier;
                if y >= printer.size.y {
                    continue;
                }
                let cell = min(8, level.saturating_sub(tier * 8));
                if cell == 0 {
                    continue;
                }
                // Classic meter colouring: the higher the cell, the hotter it reads.
                let style = if !playing_now {
                    quiet
                } else if tier + 1 == rows {
                    playing
                } else {
                    accent
                };
                printer.with_color(style, |printer| {
                    printer.print((x, y), &BLOCKS[cell - 1].to_string());
                });
            }

            // The peak marker floats above the bar and falls back under gravity,
            // so a column that just spiked stays legible after the bar drops.
            let peak_tier = peak / 8;
            let bar_tier = level.saturating_sub(1) / 8;
            if peak > 0 && peak_tier < rows && (level == 0 || peak_tier > bar_tier) {
                let y = top + rows - 1 - peak_tier;
                let style = if playing_now { playing } else { quiet };
                if y < printer.size.y {
                    printer.with_color(style, |printer| printer.print((x, y), "▔"));
                }
            }
        }
    }

    fn draw_progress(
        &self,
        printer: &Printer<'_, '_>,
        row: usize,
        region: Region,
        elapsed_ms: u128,
        duration_ms: u32,
    ) {
        let Some(region) = region.visible(printer) else {
            return;
        };
        if row >= printer.size.y {
            return;
        }

        const PARTIALS: [char; 8] = ['▏', '▎', '▍', '▌', '▋', '▊', '▉', '█'];
        let Region { start, width } = region;
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

        self.hitboxes.write().unwrap().push(Hitbox {
            row,
            start,
            width,
            control: Control::Seek,
        });
    }

    fn draw_times(
        &self,
        printer: &Printer<'_, '_>,
        row: usize,
        region: Region,
        elapsed_ms: u128,
        playable: &Playable,
    ) {
        let Some(region) = region.visible(printer) else {
            return;
        };
        if row >= printer.size.y {
            return;
        }

        let Region { start, width } = region;
        let elapsed_label = ms_to_hms(elapsed_ms.try_into().unwrap_or(u32::MAX));
        let duration_label = playable.duration_str();

        printer.with_color(ColorStyle::secondary(), |printer| {
            printer.print((start, row), &elapsed_label);
            let duration_x = start
                .saturating_add(width)
                .saturating_sub(duration_label.width());
            printer.print((duration_x, row), &duration_label);
        });

        // Between the two ends, show as much detail as the remaining gap allows.
        let percent = percent_complete(elapsed_ms, playable.duration());
        let remaining = remaining_label(elapsed_ms, playable.duration());
        let edges = elapsed_label.width() + duration_label.width() + 4;
        let center = [format!("{percent}%  ·  {remaining}"), format!("{percent}%")]
            .into_iter()
            .find(|label| label.width() + edges <= width);
        if let Some(label) = center {
            let x = region.center(label.width());
            printer.with_color(Self::accent_style(printer), |printer| {
                printer.print((x, row), &label);
            });
        }
    }

    /// Rounded card with the transport state inlaid into the top border, an optional
    /// badge in the top right, and an optional footer label in the bottom border.
    fn draw_card(
        &self,
        printer: &Printer<'_, '_>,
        rect: (usize, usize, usize, usize),
        chip: &str,
        badge: Option<&str>,
        footer: Option<&str>,
    ) {
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
        let mut chip_end = left;
        if !chip.is_empty() && card_width > chip.width() + 8 {
            let offset = left + (card_width - chip.width() - 2) / 2;
            chip_end = offset + chip.width() + 1;
            Self::inlay(
                printer,
                offset,
                top,
                &chip,
                Self::playing_style(printer),
                border,
            );
        }

        if let Some(badge) = badge {
            let start = right.saturating_sub(badge.width() + 3);
            if start > chip_end + 1 {
                Self::inlay(
                    printer,
                    start,
                    top,
                    badge,
                    Self::playing_style(printer),
                    border,
                );
            }
        }

        if let Some(footer) = footer {
            let start = right.saturating_sub(footer.width() + 4);
            if start > left + 2 {
                Self::inlay(
                    printer,
                    start,
                    bottom,
                    footer,
                    ColorStyle::secondary(),
                    border,
                );
            }
        }
    }

    /// Print `text` into a border run, padded by a blank cell on either side so the
    /// label reads as inlaid rather than as part of the line.
    fn inlay(
        printer: &Printer<'_, '_>,
        x: usize,
        y: usize,
        text: &str,
        style: ColorStyle,
        border: ColorStyle,
    ) {
        printer.with_color(border, |printer| {
            printer.print((x, y), " ");
            printer.print((x + text.width() + 1, y), " ");
        });
        printer.with_color(style, |printer| printer.print((x + 1, y), text));
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

    /// Repeat / shuffle / volume, with active toggles highlighted. Each one is a
    /// click target: the labels are the controls, not just a readout.
    fn transport_segments(&self, printer: &Printer<'_, '_>) -> Vec<Segment> {
        let on = Self::playing_style(printer);
        let off = ColorStyle::secondary();
        let separator = || Segment::new("   •   ", ColorStyle::secondary());
        let nerdfont = self.use_nerdfont();

        let repeat_mode = self.queue.get_repeat();
        let (repeat, repeat_style) = match repeat_mode {
            RepeatSetting::None => ("repeat off", off),
            RepeatSetting::RepeatPlaylist => ("repeat all", on),
            RepeatSetting::RepeatTrack => ("repeat one", on),
        };
        let repeat_icon = match (nerdfont, repeat_mode) {
            (false, _) => "",
            (true, RepeatSetting::RepeatTrack) => "\u{f0458} ",
            (true, _) => "\u{f0456} ",
        };
        let (shuffle, shuffle_style) = if self.queue.get_shuffle() {
            ("shuffle on", on)
        } else {
            ("shuffle off", off)
        };
        let shuffle_icon = if nerdfont { "\u{f049d} " } else { "" };
        let volume = self.volume_percent();
        let volume_icon = if nerdfont { "\u{f057e} " } else { "" };

        vec![
            Segment::button(
                format!("{repeat_icon}{repeat}"),
                repeat_style,
                Control::Repeat,
            ),
            separator(),
            Segment::button(
                format!("{shuffle_icon}{shuffle}"),
                shuffle_style,
                Control::Shuffle,
            ),
            separator(),
            Segment::new(volume_icon, ColorStyle::secondary()),
            Segment::button(
                volume_meter(volume, VOLUME_METER_CELLS),
                Self::accent_style(printer),
                Control::Volume,
            ),
            Segment::new(format!(" {volume}%"), ColorStyle::secondary()),
        ]
    }

    fn volume_percent(&self) -> u16 {
        (self.spotify.volume() as f64 / u16::MAX as f64 * 100.0).round() as u16
    }

    /// Clickable transport buttons, each labelled with the key that does the same
    /// thing, so the keyboard shortcuts are discoverable from the card itself.
    fn button_segments(&self, printer: &Printer<'_, '_>, keys: bool) -> Vec<Segment> {
        let nerdfont = self.use_nerdfont();
        let (play_glyph, play_key) = if self.is_playing() {
            (if nerdfont { "\u{f03e4}" } else { "❚❚" }, "Shift+P")
        } else {
            (if nerdfont { "\u{f040a}" } else { "▶" }, "Shift+P")
        };
        let previous = if nerdfont { "\u{f04ab}" } else { "◀◀" };
        let next = if nerdfont { "\u{f04ad}" } else { "▶▶" };

        let label = |glyph: &str, key: &str| {
            if keys {
                format!("[ {glyph}  {key} ]")
            } else {
                format!("[ {glyph} ]")
            }
        };
        let gap = || Segment::new("  ", ColorStyle::secondary());
        let accent = Self::accent_style(printer);

        vec![
            Segment::button(label(previous, "<"), accent, Control::Previous),
            gap(),
            Segment::button(
                label(play_glyph, play_key),
                Self::playing_style(printer),
                Control::PlayPause,
            ),
            gap(),
            Segment::button(label(next, ">"), accent, Control::Next),
        ]
    }

    /// The stacked contents of the view. `card` selects the full dashboard layout;
    /// without it the flat layout for small terminals is built instead.
    fn blocks(&self, printer: &Printer<'_, '_>, playable: &Playable, card: bool) -> Vec<Block> {
        let (title, byline, album, kind) = Self::metadata(playable);
        let (icon, state) = self.status();
        let mut blocks = Vec::new();

        if card {
            blocks.push(Block::new(BlockKind::Spectrum(SPECTRUM_MAX_HEIGHT), 5));
        } else {
            blocks.push(Block::new(
                BlockKind::Segments(vec![
                    Segment::new(format!("{icon}  "), Self::playing_style(printer)),
                    Segment::new(state, ColorStyle::secondary()),
                ]),
                5,
            ));
        }
        blocks.push(Block::blank());
        blocks.push(Block::title(title));
        blocks.push(Block::line(byline, ColorStyle::primary(), 1));
        if !album.is_empty() {
            blocks.push(Block::line(album, ColorStyle::secondary(), 4));
        }
        if card {
            blocks.push(Block::blank());
            blocks.push(Block::line(kind, ColorStyle::secondary(), 6));
        }
        blocks.push(Block::blank());
        blocks.push(Block::new(BlockKind::Progress, 0));
        blocks.push(Block::new(BlockKind::Times, 2));

        if card {
            blocks.push(Block::blank());
            // The buttons carry their own key labels, so the card documents the
            // keyboard shortcuts without spending a separate hint line on them.
            blocks.push(Block::new(
                BlockKind::Segments(self.button_segments(printer, true)),
                0,
            ));
            blocks.push(Block::blank());
            blocks.push(Block::new(BlockKind::Rule, 8));
            blocks.push(Block::new(
                BlockKind::Segments(self.transport_segments(printer)),
                3,
            ));
            if let Some(next) = self.next_up() {
                blocks.push(Block::new(
                    BlockKind::Segments(vec![
                        Segment::new("next up  ", Self::accent_style(printer)),
                        Segment::new(next, ColorStyle::secondary()),
                    ]),
                    7,
                ));
            }
        } else {
            blocks.push(Block::blank());
            blocks.push(Block::new(
                BlockKind::Segments(self.button_segments(printer, false)),
                2,
            ));
        }

        blocks
    }

    /// Lay `blocks` out top to bottom, shrinking them to the available height first.
    fn draw_blocks(
        &self,
        printer: &Printer<'_, '_>,
        mut blocks: Vec<Block>,
        frame: Frame,
        playable: Option<&Playable>,
    ) {
        fit_blocks(&mut blocks, frame.height);
        let Frame { region, edges, .. } = frame;
        let elapsed = self.spotify.get_current_progress().as_millis();
        let mut row = frame.top;

        for block in &blocks {
            match &block.kind {
                BlockKind::Blank => {}
                BlockKind::Spectrum(rows) => {
                    let seed = playable.map(Self::spectrum_seed).unwrap_or_default();
                    self.draw_spectrum(printer, row, *rows, region, elapsed, seed);
                }
                BlockKind::Line { text, style, bold } => {
                    Self::draw_line(printer, row, region, text, *style, *bold)
                }
                BlockKind::Segments(segments) => self.draw_segments(printer, row, region, segments),
                BlockKind::Progress => {
                    if let Some(playable) = playable {
                        self.draw_progress(printer, row, region, elapsed, playable.duration());
                    }
                }
                BlockKind::Times => {
                    if let Some(playable) = playable {
                        self.draw_times(printer, row, region, elapsed, playable);
                    }
                }
                BlockKind::Rule => {
                    if let Some((left, right)) = edges {
                        self.draw_rule(printer, row, left, right);
                    }
                }
            }
            row += block.height();
        }
    }

    /// Install a decoded cover so tests can lay out the two column card.
    #[cfg(all(test, feature = "album_art"))]
    fn load_art_for_test(&self, url: &str, image: image::RgbImage) {
        self.art.load_for_test(url, image);
    }

    /// The cover to draw and the cells it gets, when the card can spare the width
    /// and the image is already loaded. Kicks off the fetch when it is not.
    #[cfg(feature = "album_art")]
    fn art_layout(
        &self,
        playable: Option<&Playable>,
        card_width: usize,
        content: usize,
    ) -> Option<(String, Vec2)> {
        let url = playable?.cover_url()?;
        // Terminal cells are about twice as tall as they are wide, and half blocks
        // split each one vertically, so a square cover wants twice as many columns.
        let by_width = card_width.saturating_sub(ART_MIN_TEXT_WIDTH + 9) / 2;
        let height = min(min(content, ART_MAX_HEIGHT), by_width);
        if height < ART_MIN_HEIGHT {
            return None;
        }
        let width = height * 2;

        if !self.art.is_ready(&url) {
            self.art.prefetch(&url);
            return None;
        }
        Some((url, Vec2::new(width, height)))
    }

    #[cfg(not(feature = "album_art"))]
    fn art_layout(
        &self,
        _playable: Option<&Playable>,
        _card_width: usize,
        _content: usize,
    ) -> Option<(String, Vec2)> {
        None
    }

    #[cfg(feature = "album_art")]
    fn draw_art(&self, printer: &Printer<'_, '_>, offset: Vec2, size: Vec2, url: &str) {
        self.art.draw(printer, offset, size, url);
    }

    #[cfg(not(feature = "album_art"))]
    fn draw_art(&self, _printer: &Printer<'_, '_>, _offset: Vec2, _size: Vec2, _url: &str) {}

    /// Draw `blocks` inside a centered card, sized to whatever the terminal allows.
    fn draw_card_layout(
        &self,
        printer: &Printer<'_, '_>,
        blocks: Vec<Block>,
        chip: &str,
        badge: Option<&str>,
        footer: Option<&str>,
    ) {
        let card_width = min(CARD_MAX_WIDTH, printer.size.x.saturating_sub(4));
        let left = printer.size.x.saturating_sub(card_width) / 2;
        let right = left + card_width.saturating_sub(1);

        // Reserve the borders and one padding row at each end, fit the content into
        // what is left, then size the card to the content so it stays centered.
        let mut blocks = blocks;
        fit_blocks(&mut blocks, printer.size.y.saturating_sub(4).max(1));
        let content = blocks_height(&blocks);
        let padding = usize::from(content + 4 <= printer.size.y);
        let card_height = min(printer.size.y, content + 2 * padding + 2);
        let top = printer.size.y.saturating_sub(card_height) / 2;
        let bottom = top + card_height.saturating_sub(1);
        let content_top = top + 1 + padding;

        self.draw_card(printer, (left, top, right, bottom), chip, badge, footer);

        let playable = self.queue.get_current();
        // With a cover loaded the card goes two column: art on the left, everything
        // else centered in what is left. Without one it stays a single column.
        let mut region = Region::new(left + 3, card_width.saturating_sub(6));
        // A rule spanning the whole card would cut across the art, so the two column
        // layout separates its sections with space instead.
        let mut edges = Some((left, right));
        if let Some((url, size)) = self.art_layout(playable.as_ref(), card_width, content) {
            let offset = Vec2::new(left + 3, content_top + (content - size.y) / 2);
            self.draw_art(printer, offset, size, &url);
            region = Region::new(left + 3 + size.x + 3, card_width.saturating_sub(9 + size.x));
            edges = None;
        }

        let frame = Frame {
            top: content_top,
            height: content,
            region,
            edges,
        };
        self.draw_blocks(printer, blocks, frame, playable.as_ref());
    }

    fn draw_empty(&self, printer: &Printer<'_, '_>) {
        let glyph = if self.use_nerdfont() {
            "\u{f075a}"
        } else {
            "◇"
        };
        let blocks = vec![
            Block::line(glyph, Self::accent_style(printer), 4),
            Block::blank(),
            Block::title("Nothing playing yet"),
            Block::line(
                "Pick a track in your library, or press Enter on a search result",
                ColorStyle::secondary(),
                1,
            ),
        ];

        if printer.size.x >= CARD_MIN_WIDTH && printer.size.y >= 8 {
            // Nothing is loaded, so the player state would only ever read "stopped".
            self.draw_card_layout(printer, blocks, "IDLE", None, None);
        } else {
            let height = printer.size.y;
            let frame = Frame {
                top: height.saturating_sub(min(height, blocks_height(&blocks))) / 2,
                height,
                region: Region::new(1, printer.size.x.saturating_sub(2)),
                edges: None,
            };
            self.draw_blocks(printer, blocks, frame, None);
        }
    }

    fn draw_flat(&self, printer: &Printer<'_, '_>, playable: &Playable) {
        let mut blocks = self.blocks(printer, playable, false);
        fit_blocks(&mut blocks, printer.size.y);
        let frame = Frame {
            top: printer.size.y.saturating_sub(blocks_height(&blocks)) / 2,
            height: printer.size.y,
            region: Region::new(1, printer.size.x.saturating_sub(2)),
            edges: None,
        };
        self.draw_blocks(printer, blocks, frame, Some(playable));
    }

    fn draw_dashboard(&self, printer: &Printer<'_, '_>, playable: &Playable) {
        let saved = self.library.is_saved_track(playable).then(|| {
            if self.use_nerdfont() {
                "\u{f012c} saved"
            } else {
                "♥ saved"
            }
        });
        let footer = self.queue_position();
        let (icon, state) = self.status();
        self.draw_card_layout(
            printer,
            self.blocks(printer, playable, true),
            &format!("{icon}  {state}"),
            saved,
            footer.as_deref(),
        );
    }

    /// Run the control that was clicked. These mirror the default command handlers,
    /// so a click and its key do the same thing.
    fn activate(&self, hitbox: Hitbox, position: Vec2) {
        match hitbox.control {
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
            Control::Repeat => {
                let mode = match self.queue.get_repeat() {
                    RepeatSetting::None => RepeatSetting::RepeatPlaylist,
                    RepeatSetting::RepeatPlaylist => RepeatSetting::RepeatTrack,
                    RepeatSetting::RepeatTrack => RepeatSetting::None,
                };
                self.queue.set_repeat(mode);
            }
            Control::Shuffle => self.queue.set_shuffle(!self.queue.get_shuffle()),
            Control::Volume => {
                let volume = u16::MAX as f64 * hitbox.fraction(position);
                self.spotify
                    .set_volume(volume.round().max(0.0) as u16, true);
            }
        }
    }

    /// Wheel over a control: seek through the track, or step the volume.
    fn scroll(&self, control: Control, steps: i32) {
        match control {
            Control::Volume => {
                // One notch is one percent, the same as the `+` and `-` keys.
                let step = VOLUME_PERCENT.saturating_mul(steps.unsigned_abs() as u16);
                let volume = if steps > 0 {
                    self.spotify.volume().saturating_add(step)
                } else {
                    self.spotify.volume().saturating_sub(step)
                };
                self.spotify.set_volume(volume, true);
            }
            _ => self.spotify.seek_relative(-5000 * steps),
        }
    }
}

impl View for NowPlayingView {
    fn draw(&self, printer: &Printer<'_, '_>) {
        self.animator.mark_drawn();
        self.hitboxes.write().unwrap().clear();
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

        if printer.size.x >= CARD_MIN_WIDTH && printer.size.y >= CARD_MIN_HEIGHT {
            self.draw_dashboard(printer, &playable);
        } else {
            self.draw_flat(printer, &playable);
        }
    }

    fn required_size(&mut self, constraint: Vec2) -> Vec2 {
        constraint
    }

    fn on_event(&mut self, event: Event) -> EventResult {
        // `/` is the jump key elsewhere, but a single item dashboard has no list to
        // jump through, so it is free here and keeps its "search" meaning.
        if event == Event::Char('/') {
            let spotify = self.spotify.clone();
            let queue = self.queue.clone();
            let library = self.library.clone();
            let events = self.events.clone();
            return EventResult::with_cb(move |s| {
                s.add_layer(Modal::new(QuickSearch::new(
                    spotify.clone(),
                    queue.clone(),
                    library.clone(),
                    events.clone(),
                )));
            });
        }

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
        let Some(hitbox) = self
            .hitboxes
            .read()
            .unwrap()
            .iter()
            .find(|hitbox| hitbox.contains(position))
            .copied()
        else {
            return EventResult::Ignored;
        };

        match event {
            MouseEvent::Press(MouseButton::Left) => {
                self.activate(hitbox, position);
                EventResult::consumed()
            }
            // Scrolling over a control nudges it, rather than doing nothing.
            MouseEvent::WheelUp => {
                self.scroll(hitbox.control, 1);
                EventResult::consumed()
            }
            MouseEvent::WheelDown => {
                self.scroll(hitbox.control, -1);
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

/// Time left in the current item, as a `-1:23` style countdown.
fn remaining_label(elapsed_ms: u128, duration_ms: u32) -> String {
    let remaining = (duration_ms as u128).saturating_sub(elapsed_ms);
    format!("-{}", ms_to_hms(remaining.try_into().unwrap_or(u32::MAX)))
}

/// A `cells` wide meter for a 0..=100 percentage, rounded to the nearest cell.
fn volume_meter(percent: u16, cells: usize) -> String {
    let filled = (percent as usize * cells + 50) / 100;
    let filled = min(filled, cells);
    let mut meter = "▮".repeat(filled);
    meter.push_str(&"▯".repeat(cells - filled));
    meter
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use cursive::Vec2;
    use unicode_width::UnicodeWidthStr;

    use cursive::backends::puppet::Backend as PuppetBackend;
    use cursive::backends::puppet::observed::ObservedScreen;

    use crate::command::Command;
    use crate::commands::CommandResult;
    use crate::config::Config;
    use crate::events::EventManager;
    use crate::library::Library;
    use crate::model::playable::Playable;
    use crate::model::track::Track;
    use crate::queue::{Queue, RepeatSetting};
    use crate::spotify::{PlayerEvent, Spotify};

    use super::{
        Block, BlockKind, NowPlayingView, SPECTRUM_MAX_HEIGHT, SpectrumState, ViewExt,
        blocks_height, fit_blocks, percent_complete, progress_eighths, remaining_label, truncate,
        volume_meter,
    };
    use cursive::theme::ColorStyle;

    fn track(title: &str, artist: &str, album: &str) -> Playable {
        Playable::Track(Track {
            id: Some(title.to_string()),
            uri: format!("spotify:track:{title}"),
            title: title.to_string(),
            track_number: 4,
            disc_number: 1,
            duration: 225_000,
            artists: vec![artist.to_string()],
            artist_ids: vec![],
            album: Some(album.to_string()),
            album_id: None,
            album_artists: vec![],
            cover_url: Some(format!("https://example.invalid/{title}.jpg")),
            url: String::new(),
            added_at: None,
            list_index: 0,
            is_local: false,
            is_playable: Some(true),
        })
    }

    /// Render the view onto a puppet terminal of `size` and capture the result.
    fn render_state(
        size: Vec2,
        tracks: Vec<Playable>,
        current: Option<usize>,
        status: PlayerEvent,
        saved: bool,
        frames: usize,
    ) -> ObservedScreen {
        let cfg = Config::new_for_test();
        let ev = EventManager::new_for_test();
        let spotify = Spotify::new_for_test(cfg.clone(), ev.clone());
        let library = Library::new_for_test(ev.clone(), spotify.clone(), cfg.clone());
        let events = ev;
        spotify.update_status(status);
        if saved {
            *library.is_done.write().unwrap() = true;
            for playable in &tracks {
                if let Playable::Track(track) = playable {
                    library.tracks.write().unwrap().push(track.clone());
                }
            }
        }
        let queue = Arc::new(Queue::new_for_test(
            tracks,
            current,
            spotify,
            cfg.clone(),
            library.clone(),
        ));

        let backend = PuppetBackend::init(Some(size));
        let screens = backend.stream();
        let mut siv = cursive::Cursive::new();
        siv.set_theme(crate::theme::load(&cfg.values().theme));
        let view = NowPlayingView::new(queue, library, events);
        #[cfg(feature = "album_art")]
        if let Some(playable) = view.queue.get_current()
            && let Some(url) = playable.cover_url()
        {
            view.load_art_for_test(
                &url,
                image::RgbImage::from_fn(64, 64, |x, y| image::Rgb([x as u8, y as u8, 128])),
            );
        }
        siv.add_fullscreen_layer(view);
        let mut runner = siv.into_runner(backend);
        for frame in 0..frames.max(1) {
            if frame > 0 {
                // The visualizer integrates over real elapsed time, so frames have to
                // be spaced out for the bars to animate at all.
                std::thread::sleep(Duration::from_millis(30));
            }
            runner.refresh();
        }
        screens
            .try_iter()
            .last()
            .expect("puppet backend captured a frame")
    }

    /// A view with its own queue, for exercising the drawing helpers directly.
    fn view(tracks: Vec<Playable>, current: Option<usize>) -> NowPlayingView {
        let cfg = Config::new_for_test();
        let ev = EventManager::new_for_test();
        let spotify = Spotify::new_for_test(cfg.clone(), ev.clone());
        let library = Library::new_for_test(ev.clone(), spotify.clone(), cfg.clone());
        spotify.update_status(PlayerEvent::Paused(Duration::from_secs(62)));
        let queue = Queue::new_for_test(tracks, current, spotify, cfg, library.clone());
        NowPlayingView::new(Arc::new(queue), library, ev)
    }

    /// Every frame of an animated render, for exercising the animation.
    fn render_frames(size: Vec2, frames: usize) -> Vec<Vec<String>> {
        let cfg = Config::new_for_test();
        let ev = EventManager::new_for_test();
        let spotify = Spotify::new_for_test(cfg.clone(), ev.clone());
        let library = Library::new_for_test(ev.clone(), spotify.clone(), cfg.clone());
        spotify.update_status(PlayerEvent::Playing(std::time::SystemTime::now()));
        let queue = Arc::new(Queue::new_for_test(
            queued(),
            Some(0),
            spotify,
            cfg.clone(),
            library.clone(),
        ));
        let backend = PuppetBackend::init(Some(size));
        let screens = backend.stream();
        let mut siv = cursive::Cursive::new();
        siv.set_theme(crate::theme::load(&cfg.values().theme));
        siv.add_fullscreen_layer(NowPlayingView::new(queue, library, ev));
        let mut runner = siv.into_runner(backend);
        for _ in 0..frames {
            runner.refresh();
            std::thread::sleep(Duration::from_millis(50));
        }
        screens.try_iter().map(|screen| rows(&screen)).collect()
    }

    #[test]
    fn transport_commands_fall_through_to_the_player() {
        // Skip is a player-wide command, not a view command: the view has to
        // ignore it so the default handler in `CommandManager` runs.
        let mut siv = cursive::Cursive::new();
        let mut view = view(queued(), Some(0));
        for command in [Command::Next, Command::Previous, Command::TogglePlay] {
            assert!(
                matches!(
                    view.on_command(&mut siv, &command),
                    Ok(CommandResult::Ignored)
                ),
                "{command:?} was swallowed by the now playing view"
            );
        }
    }

    #[test]
    fn clicking_next_skips_to_the_following_track() {
        let (queue, _) = click(Vec2::new(90, 30), "▶▶");
        assert_eq!(queue.get_current_index(), Some(1));
    }

    #[test]
    fn clicking_the_toggles_changes_them() {
        let (queue, _) = click(Vec2::new(90, 30), "shuffle off");
        assert!(queue.get_shuffle());

        let (queue, _) = click(Vec2::new(90, 30), "repeat off");
        assert_eq!(queue.get_repeat(), RepeatSetting::RepeatPlaylist);
    }

    #[test]
    fn clicking_the_volume_meter_sets_the_volume() {
        // The click lands on the first cell of the meter, so volume drops to zero.
        let (_, spotify) = click(Vec2::new(90, 30), "▮");
        assert_eq!(spotify.volume(), 0);
    }

    #[test]
    fn clicking_inert_text_does_nothing() {
        let (queue, spotify) = click(Vec2::new(90, 30), "Juno Reactor");
        assert_eq!(queue.get_current_index(), Some(0));
        assert!(!queue.get_shuffle());
        assert_eq!(spotify.get_current_progress(), Duration::from_secs(62));
    }

    #[test]
    fn the_band_keeps_moving_while_playing() {
        let frames = render_frames(Vec2::new(90, 28), 6);
        let bands: Vec<Vec<String>> = frames
            .iter()
            .map(|frame| frame.iter().skip(4).take(4).cloned().collect())
            .collect();
        assert!(
            bands.windows(2).any(|pair| pair[0] != pair[1]),
            "the visualizer never moved:\n{bands:#?}"
        );
    }

    fn render(size: Vec2, tracks: Vec<Playable>, current: Option<usize>) -> ObservedScreen {
        render_state(
            size,
            tracks,
            current,
            PlayerEvent::Paused(Duration::from_secs(62)),
            false,
            1,
        )
    }

    fn queued() -> Vec<Playable> {
        vec![
            track("Solaris", "Juno Reactor", "Shango"),
            track("Pistolero", "Juno Reactor", "Shango"),
        ]
    }

    /// Render, click once at the first cell of `label`, and hand back the queue and
    /// player so the effect of the click can be checked.
    fn click(size: Vec2, label: &str) -> (Arc<Queue>, Spotify) {
        let cfg = Config::new_for_test();
        let ev = EventManager::new_for_test();
        let spotify = Spotify::new_for_test(cfg.clone(), ev.clone());
        let library = Library::new_for_test(ev.clone(), spotify.clone(), cfg.clone());
        spotify.update_status(PlayerEvent::Paused(Duration::from_secs(62)));
        let queue = Arc::new(Queue::new_for_test(
            queued(),
            Some(0),
            spotify.clone(),
            cfg.clone(),
            library.clone(),
        ));

        let backend = PuppetBackend::init(Some(size));
        let screens = backend.stream();
        let mut siv = cursive::Cursive::new();
        siv.set_theme(crate::theme::load(&cfg.values().theme));
        siv.add_fullscreen_layer(NowPlayingView::new(queue.clone(), library, ev));
        let mut runner = siv.into_runner(backend);
        runner.refresh();

        let screen = screens.try_iter().last().expect("a frame");
        let position = find_text(&screen, label)
            .unwrap_or_else(|| panic!("{label:?} is not on screen:\n{:#?}", rows(&screen)));
        runner.on_event(cursive::event::Event::Mouse {
            offset: Vec2::zero(),
            position,
            event: cursive::event::MouseEvent::Press(cursive::event::MouseButton::Left),
        });

        (queue, spotify)
    }

    /// The screen position of the first cell of `needle`, comparing cell by cell so
    /// wide glyphs and box drawing characters cannot shift the match.
    fn find_text(screen: &ObservedScreen, needle: &str) -> Option<Vec2> {
        for y in 0..screen.size().y {
            let cells: Vec<String> = (0..screen.size().x)
                .map(|x| match &screen[Vec2::new(x, y)] {
                    Some(cell) if !cell.letter.is_continuation() => cell.letter.unwrap(),
                    _ => String::new(),
                })
                .collect();
            for start in 0..cells.len() {
                if cells[start..].concat().starts_with(needle) {
                    return Some(Vec2::new(start, y));
                }
            }
        }
        None
    }

    /// The rendered screen as plain rows of text, trailing blanks trimmed.
    fn rows(screen: &ObservedScreen) -> Vec<String> {
        (0..screen.size().y)
            .map(|y| {
                (0..screen.size().x)
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

    #[cfg(feature = "album_art")]
    #[test]
    fn a_wide_card_puts_the_cover_beside_the_metadata() {
        let screen = render(Vec2::new(96, 30), queued(), Some(0));
        let art = find_text(&screen, "\u{2580}\u{2580}\u{2580}\u{2580}")
            .expect("the cover is drawn as half blocks");
        let title = find_text(&screen, "Solaris").expect("the title is drawn");
        // The text column sits to the right of the cover, and the transport line
        // survives intact so its buttons stay clickable.
        assert!(
            title.x > art.x,
            "title at {title:?} is not right of art at {art:?}"
        );
        assert!(rows(&screen).join("\n").contains("shuffle off"));
    }

    #[cfg(feature = "album_art")]
    #[test]
    fn a_narrow_card_drops_the_cover_rather_than_the_text() {
        let screen = render(Vec2::new(64, 24), queued(), Some(0));
        assert!(find_text(&screen, "\u{2580}\u{2580}\u{2580}\u{2580}").is_none());
        assert!(find_text(&screen, "Solaris").is_some());
    }

    #[test]
    fn dashboard_frames_the_track_in_a_card() {
        let rows = rows(&render(Vec2::new(90, 28), queued(), Some(0)));
        let screen = rows.join("\n");

        // The card is centred, and everything it advertises is on screen.
        let top = rows.iter().find(|row| row.contains('╭')).unwrap();
        assert!(top.starts_with("      ╭") && top.ends_with("╮"));
        assert!(top.contains("PAUSED"));
        assert!(screen.contains("Solaris"));
        assert!(screen.contains("Juno Reactor"));
        assert!(screen.contains("Shango"));
        assert!(screen.contains("TRACK 4"));
        assert!(screen.contains("1:02"));
        assert!(screen.contains("27%  ·  -2:43"));
        assert!(screen.contains("3:45"));
        assert!(screen.contains("repeat off"));
        assert!(screen.contains("next up"));
        assert!(screen.contains("Juno Reactor – Pistolero"));
        // Queue position is inlaid into the bottom border.
        assert!(
            rows.iter()
                .any(|row| row.contains("1 / 2") && row.contains('╯'))
        );
    }

    #[test]
    fn saved_tracks_are_badged_in_the_border() {
        let saved = rows(&render_state(
            Vec2::new(90, 28),
            queued(),
            Some(0),
            PlayerEvent::Paused(Duration::from_secs(62)),
            true,
            1,
        ));
        let top = saved.iter().find(|row| row.contains('╭')).unwrap();
        assert!(top.contains("♥ saved"), "{top}");

        let unsaved = rows(&render(Vec2::new(90, 28), queued(), Some(0)));
        let top = unsaved.iter().find(|row| row.contains('╭')).unwrap();
        assert!(!top.contains("saved"), "{top}");
    }

    #[test]
    fn playing_draws_a_spectrum_above_the_title() {
        let rows = rows(&render_state(
            Vec2::new(90, 28),
            queued(),
            Some(0),
            PlayerEvent::Playing(std::time::SystemTime::now()),
            false,
            8,
        ));
        let title = rows.iter().position(|row| row.contains("Solaris")).unwrap();
        let bars = rows[..title]
            .iter()
            .filter(|row| row.contains('█') || row.contains('▄'))
            .count();
        assert!(bars > 1, "expected a multi row spectrum, got:\n{rows:#?}");
    }

    #[test]
    fn small_terminals_drop_the_card_but_keep_the_essentials() {
        let rows = rows(&render(Vec2::new(34, 12), queued(), Some(0)));
        let screen = rows.join("\n");

        assert!(!screen.contains('╭'), "no room for a card:\n{screen}");
        assert!(screen.contains("Solaris"));
        assert!(screen.contains("Juno Reactor"));
        assert!(screen.contains("1:02"));
        // Every row has to stay inside the terminal.
        assert!(rows.iter().all(|row| row.width() <= 34));
    }

    #[test]
    fn nothing_playing_explains_what_to_do() {
        let screen = rows(&render(Vec2::new(80, 20), vec![], None)).join("\n");
        assert!(screen.contains("Nothing playing yet"));
        assert!(screen.contains("IDLE"));
        assert!(!screen.contains("PAUSED"));
    }

    fn layout() -> Vec<Block> {
        vec![
            Block::new(BlockKind::Spectrum(3), 5),
            Block::blank(),
            Block::title("Song"),
            Block::line("Artist", ColorStyle::primary(), 1),
            Block::new(BlockKind::Progress, 0),
        ]
    }

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

    #[test]
    fn remaining_counts_down_and_never_goes_negative() {
        assert_eq!(remaining_label(60_000, 180_000), "-2:00");
        assert_eq!(remaining_label(200_000, 180_000), "-0:00");
    }

    #[test]
    fn volume_meter_fills_proportionally() {
        assert_eq!(volume_meter(0, 8), "▯▯▯▯▯▯▯▯");
        assert_eq!(volume_meter(50, 8), "▮▮▮▮▯▯▯▯");
        assert_eq!(volume_meter(100, 8), "▮▮▮▮▮▮▮▮");
        // Rounding must never overrun the meter, even above nominal volume.
        assert_eq!(volume_meter(150, 8), "▮▮▮▮▮▮▮▮");
    }

    #[test]
    fn bars_snap_into_place_on_the_first_frame() {
        // A single redraw — a paused view, say — has to show the band right away.
        let mut state = SpectrumState::default();
        state.advance(&[4.0, 12.0, 20.0], 0.0);
        assert_eq!(state.levels, [4.0, 12.0, 20.0]);
        assert_eq!(state.peaks, [4.0, 12.0, 20.0]);
    }

    #[test]
    fn bars_rise_fast_and_fall_slowly() {
        let mut state = SpectrumState::default();
        state.advance(&[0.0], 0.0);

        state.advance(&[24.0], 0.05);
        let risen = state.levels[0];
        assert!(risen > 12.0 && risen <= 24.0, "{risen}");

        state.advance(&[0.0], 0.05);
        let fallen = state.levels[0];
        assert!(fallen > risen / 2.0, "fell too fast: {risen} -> {fallen}");
    }

    #[test]
    fn peak_markers_hang_above_the_bar_and_drop_back() {
        let mut state = SpectrumState::default();
        state.advance(&[24.0], 0.0);
        state.advance(&[0.0], 0.05);
        assert!(state.peaks[0] > state.levels[0]);

        let hanging = state.peaks[0];
        for _ in 0..4 {
            state.advance(&[0.0], 0.05);
        }
        assert!(state.peaks[0] < hanging, "the peak never fell");
        assert!(
            state.peaks[0] >= state.levels[0],
            "the peak fell through the bar"
        );
    }

    #[test]
    fn spectrum_targets_stay_inside_the_band() {
        let view = view(queued(), Some(0));
        for elapsed in [0, 137, 4_000, 60_000] {
            let targets = view.spectrum_targets(48, SPECTRUM_MAX_HEIGHT, elapsed, 1.3);
            assert_eq!(targets.len(), 48);
            let ceiling = (SPECTRUM_MAX_HEIGHT * 8) as f64;
            assert!(targets.iter().all(|level| (0.0..=ceiling).contains(level)));
        }

        // The envelope tapers the band, so over time the edges stay under the middle.
        let average = |column: usize| {
            (0..40)
                .map(|frame| {
                    view.spectrum_targets(48, SPECTRUM_MAX_HEIGHT, frame * 250, 1.3)[column]
                })
                .sum::<f64>()
                / 40.0
        };
        let middle = average(24);
        assert!(average(0) < middle && average(47) < middle);
    }

    #[test]
    fn pausing_ducks_the_band_without_flattening_it() {
        let view = view(queued(), Some(0));
        let paused = view.spectrum_targets(48, SPECTRUM_MAX_HEIGHT, 4_000, 1.3);
        assert!(
            paused.iter().any(|level| *level > 1.0),
            "paused band is flat"
        );

        view.spotify
            .update_status(PlayerEvent::Playing(std::time::SystemTime::now()));
        let playing = view.spectrum_targets(48, SPECTRUM_MAX_HEIGHT, 4_000, 1.3);
        let total = |levels: &[f64]| levels.iter().sum::<f64>();
        assert!(total(&paused) < total(&playing) * 0.6);
    }

    #[test]
    fn every_track_gets_its_own_pattern() {
        let one = NowPlayingView::spectrum_seed(&track("One", "Artist", "Album"));
        let other = NowPlayingView::spectrum_seed(&track("Other", "Artist", "Album"));
        assert_ne!(one, other);
        assert_eq!(one, NowPlayingView::spectrum_seed(&track("One", "A", "B")));
    }

    #[test]
    fn layout_keeps_its_full_height_when_it_fits() {
        let mut blocks = layout();
        fit_blocks(&mut blocks, 7);
        assert_eq!(blocks_height(&blocks), 7);
    }

    #[test]
    fn layout_sheds_decoration_before_content() {
        let mut blocks = layout();
        fit_blocks(&mut blocks, 5);
        assert_eq!(blocks_height(&blocks), 5);
        // The blank row goes first, then the spectrum shrinks a row at a time.
        assert!(!blocks.iter().any(|b| matches!(b.kind, BlockKind::Blank)));
        assert!(matches!(blocks[0].kind, BlockKind::Spectrum(2)));
        assert!(matches!(blocks.last().unwrap().kind, BlockKind::Progress));
    }

    #[test]
    fn layout_never_drops_the_title_or_progress_bar() {
        let mut blocks = layout();
        fit_blocks(&mut blocks, 1);
        assert_eq!(blocks.len(), 2);
        assert!(matches!(blocks[0].kind, BlockKind::Line { bold: true, .. }));
        assert!(matches!(blocks[1].kind, BlockKind::Progress));
    }
}
