//! The band drawn above the track on the now playing card.
//!
//! The levels themselves come from the audio tap; this owns how they move and how
//! they are drawn. Motion is integrated over real elapsed time rather than per
//! frame, so the band looks the same whether the terminal is redrawing twice a
//! second or twenty times.

use std::cmp::min;
use std::time::Instant;

use cursive::Printer;
use cursive::theme::ColorStyle;

/// Eighth-block glyphs, used both for the spectrum and for sub-cell progress.
pub const BLOCKS: [char; 8] = [
    '\u{2581}', '\u{2582}', '\u{2583}', '\u{2584}', '\u{2585}', '\u{2586}', '\u{2587}', '\u{2588}',
];
/// Partial blocks that hang from the top of a cell, for the reflection in the
/// mirror style. Unicode has no eighths going downward, so this is as fine as a
/// hanging bar gets.
const HANGING: [char; 3] = ['\u{2594}', '\u{2580}', '\u{2588}'];
/// Seconds a bar takes to close most of the gap up to, and back down from, its
/// target: hits land immediately, decay is slow, like a real meter.
const BAR_RISE: f64 = 0.05;
const BAR_FALL: f64 = 0.30;
/// Downward acceleration of a peak marker, in eighths of a cell per second squared.
const PEAK_GRAVITY: f64 = 30.0;
/// Longest frame gap the animation will integrate over, so a view that was hidden
/// for a while eases back in instead of snapping.
const MAX_FRAME_GAP: f64 = 0.25;

/// How the band is drawn. The levels are the same in every style; only the shape
/// they are given changes.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Style {
    /// One bar per band, growing up from the baseline.
    #[default]
    Bars,
    /// Bars with a dimmer reflection hanging below them.
    Mirror,
    /// A single line tracing the top of the band, drawn as a curve.
    Wave,
    /// One horizontal meter for the whole mix, with a peak marker, like the
    /// needle on an amplifier.
    Vu,
}

impl Style {
    /// The style named in the config, or bars for anything unrecognised: a typo
    /// should cost the shape asked for, not the visualizer.
    pub fn parse(name: Option<&str>) -> Self {
        match name
            .map(str::trim)
            .unwrap_or_default()
            .to_lowercase()
            .as_str()
        {
            "mirror" => Self::Mirror,
            "wave" => Self::Wave,
            "vu" => Self::Vu,
            _ => Self::Bars,
        }
    }
}

/// The colours a band is drawn in: `hot` for the top of a bar and for peaks,
/// `warm` for the body, `quiet` for the floor and for a band nothing is feeding.
#[derive(Clone, Copy)]
pub struct Palette {
    pub hot: ColorStyle,
    pub warm: ColorStyle,
    pub quiet: ColorStyle,
    /// Whether something is actually playing. A band nobody is feeding is drawn
    /// in one quiet colour, so a paused view does not look like a live one.
    pub live: bool,
}

/// Where a band goes: `rows` rows starting at `top`, `width` columns from `origin`.
#[derive(Clone, Copy)]
pub struct Area {
    pub origin: usize,
    pub top: usize,
    pub width: usize,
    pub rows: usize,
}

impl Area {
    fn baseline(&self) -> usize {
        self.top + self.rows - 1
    }
}

/// Bar and peak marker animation, carried between frames. Heights are in eighths
/// of a cell; motion is integrated over real elapsed time so the animation looks
/// the same whether the terminal is redrawing at two frames a second or twenty.
#[derive(Default)]
pub struct SpectrumState {
    levels: Vec<f64>,
    peaks: Vec<f64>,
    /// Current downward speed of each peak marker, in eighths per second.
    peak_speed: Vec<f64>,
    last_frame: Option<Instant>,
}

impl SpectrumState {
    /// Advance the animation to now, from however long ago the last frame was.
    pub fn step(&mut self, targets: &[f64]) {
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

/// Draw the band in whichever shape the user asked for.
pub fn draw(
    printer: &Printer<'_, '_>,
    state: &SpectrumState,
    area: Area,
    palette: &Palette,
    style: Style,
) {
    if area.rows == 0 || area.width == 0 || state.levels.len() < area.width {
        return;
    }
    match style {
        Style::Bars => bars(printer, state, area, palette, 0),
        Style::Mirror => mirror(printer, state, area, palette),
        Style::Wave => wave(printer, state, area, palette),
        Style::Vu => vu(printer, state, area, palette),
    }
}

/// Bars growing up from the baseline, with a peak marker floating above each.
///
/// `sunk` is how many of the bottom rows belong to something else, which is how
/// the mirror style keeps its reflection out of the way.
fn bars(
    printer: &Printer<'_, '_>,
    state: &SpectrumState,
    area: Area,
    palette: &Palette,
    sunk: usize,
) {
    let rows = area.rows.saturating_sub(sunk);
    if rows == 0 {
        return;
    }
    let baseline = area.top + rows - 1;

    for column in 0..area.width {
        let x = area.origin + column;
        let level = state.levels[column].round().max(0.0) as usize;
        let peak = state.peaks[column].round().max(0.0) as usize;

        // A dim floor frames the band, and keeps it readable as a band when a
        // column has decayed away to nothing.
        if level == 0 && baseline < printer.size.y {
            printer.with_color(palette.quiet, |printer| {
                printer.print((x, baseline), "\u{2581}")
            });
        }

        for tier in 0..rows {
            let y = baseline - tier;
            if y >= printer.size.y {
                continue;
            }
            let cell = min(8, level.saturating_sub(tier * 8));
            if cell == 0 {
                continue;
            }
            // Classic meter colouring: the higher the cell, the hotter it reads.
            let style = if !palette.live {
                palette.quiet
            } else if tier + 1 == rows {
                palette.hot
            } else {
                palette.warm
            };
            printer.with_color(style, |printer| {
                printer.print((x, y), &BLOCKS[cell - 1].to_string());
            });
        }

        // The peak marker floats above the bar and falls back under gravity, so a
        // column that just spiked stays legible after the bar drops.
        let peak_tier = peak / 8;
        let bar_tier = level.saturating_sub(1) / 8;
        if peak > 0 && peak_tier < rows && (level == 0 || peak_tier > bar_tier) {
            let y = baseline - peak_tier;
            let style = if palette.live {
                palette.hot
            } else {
                palette.quiet
            };
            if y < printer.size.y {
                printer.with_color(style, |printer| printer.print((x, y), "\u{2594}"));
            }
        }
    }
}

/// Bars in the upper rows with a dimmer copy hanging below them, the way a band
/// looks reflected in the glass of a rack unit.
fn mirror(printer: &Printer<'_, '_>, state: &SpectrumState, area: Area, palette: &Palette) {
    let reflected = area.rows / 2;
    bars(printer, state, area, palette, reflected);
    if reflected == 0 {
        return;
    }

    let surface = area.top + area.rows - reflected;
    // The reflection is shortened as well as dimmed, so it reads as a reflection
    // rather than as a second band competing with the first.
    let ceiling = (reflected * 8) as f64;
    for column in 0..area.width {
        let x = area.origin + column;
        let level = ((state.levels[column] * 0.6).min(ceiling)).round().max(0.0) as usize;
        for depth in 0..reflected {
            let y = surface + depth;
            if y >= printer.size.y {
                break;
            }
            let cell = min(8, level.saturating_sub(depth * 8));
            if cell == 0 {
                break;
            }
            let glyph = HANGING[match cell {
                1..=2 => 0,
                3..=6 => 1,
                _ => 2,
            }];
            printer.with_color(palette.quiet, |printer| {
                printer.print((x, y), &glyph.to_string());
            });
        }
    }
}

/// A single line tracing the top of the band.
fn wave(printer: &Printer<'_, '_>, state: &SpectrumState, area: Area, palette: &Palette) {
    let ceiling = (area.rows * 8) as f64;
    for column in 0..area.width {
        let x = area.origin + column;
        let level = state.levels[column].clamp(0.0, ceiling - 1.0);
        let eighths = level.round() as usize;
        let y = area.baseline() - (eighths / 8);
        if y >= printer.size.y {
            continue;
        }
        let style = if palette.live {
            palette.warm
        } else {
            palette.quiet
        };
        printer.with_color(style, |printer| {
            printer.print((x, y), &BLOCKS[eighths % 8].to_string());
        });
    }
}

/// One horizontal meter for the whole mix: the average level fills it, and the
/// loudest band of the moment marks the peak.
fn vu(printer: &Printer<'_, '_>, state: &SpectrumState, area: Area, palette: &Palette) {
    let ceiling = (area.rows * 8) as f64;
    let average = state.levels[..area.width].iter().sum::<f64>() / area.width as f64;
    let peak = state.peaks[..area.width]
        .iter()
        .fold(0.0f64, |peak, level| peak.max(*level));
    let row = area.top + area.rows / 2;
    if row >= printer.size.y {
        return;
    }

    let filled = ((average / ceiling) * area.width as f64).round().max(0.0) as usize;
    let filled = min(filled, area.width);
    printer.with_color(palette.quiet, |printer| {
        printer.print((area.origin, row), &"\u{2508}".repeat(area.width));
    });
    for column in 0..filled {
        // The last fifth of the meter is the hot end, the way a meter's red is.
        let style = if !palette.live {
            palette.quiet
        } else if column * 5 >= area.width * 4 {
            palette.hot
        } else {
            palette.warm
        };
        printer.with_color(style, |printer| {
            printer.print((area.origin + column, row), "\u{2588}");
        });
    }

    let marker = ((peak / ceiling) * area.width as f64).round().max(0.0) as usize;
    let marker = min(marker, area.width.saturating_sub(1));
    if marker > filled {
        let style = if palette.live {
            palette.hot
        } else {
            palette.quiet
        };
        printer.with_color(style, |printer| {
            printer.print((area.origin + marker, row), "\u{2503}");
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn an_unknown_style_falls_back_to_bars() {
        assert_eq!(Style::parse(Some("mirror")), Style::Mirror);
        assert_eq!(Style::parse(Some(" WAVE ")), Style::Wave);
        assert_eq!(Style::parse(Some("vu")), Style::Vu);
        assert_eq!(Style::parse(Some("wobble")), Style::Bars);
        assert_eq!(Style::parse(None), Style::Bars);
    }
}

#[cfg(test)]
mod drawing {
    use super::*;
    use cursive::Vec2;
    use cursive::backends::puppet::Backend as PuppetBackend;
    use cursive::backends::puppet::observed::ObservedScreen;
    use cursive::theme::ColorStyle;
    use cursive::view::View;

    /// A band on its own, so a style can be drawn without a whole card around it.
    struct Band {
        state: SpectrumState,
        style: Style,
    }

    impl View for Band {
        fn draw(&self, printer: &Printer<'_, '_>) {
            let palette = Palette {
                hot: ColorStyle::primary(),
                warm: ColorStyle::primary(),
                quiet: ColorStyle::secondary(),
                live: true,
            };
            let area = Area {
                origin: 0,
                top: 0,
                width: self.state.levels.len(),
                rows: printer.size.y,
            };
            draw(printer, &self.state, area, &palette, self.style);
        }

        fn required_size(&mut self, constraint: Vec2) -> Vec2 {
            constraint
        }
    }

    /// Draw `levels` in `style` and read the screen back a row at a time.
    fn render(levels: &[f64], style: Style, size: Vec2) -> Vec<String> {
        let mut state = SpectrumState::default();
        state.advance(levels, 0.0);

        let backend = PuppetBackend::init(Some(size));
        let screens = backend.stream();
        let mut siv = cursive::Cursive::new();
        siv.add_fullscreen_layer(Band { state, style });
        let mut runner = siv.into_runner(backend);
        runner.refresh();
        let screen: ObservedScreen = screens.try_iter().last().expect("a frame was captured");

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

    /// Full height in the first column, half in the second, silence in the third.
    fn levels(rows: usize) -> Vec<f64> {
        let ceiling = (rows * 8) as f64;
        vec![ceiling, ceiling / 2.0, 0.0]
    }

    #[test]
    fn bars_grow_up_from_the_baseline() {
        let rows = 4;
        let drawn = render(&levels(rows), Style::Bars, Vec2::new(3, rows));
        // The tall column reaches the top row; the quiet one is left as a floor.
        assert!(drawn[0].starts_with('█'), "{drawn:?}");
        assert_eq!(drawn[rows - 1].chars().nth(2), Some('▁'), "{drawn:?}");
        // The half height column is empty in the top half and solid in the bottom.
        assert_eq!(drawn[0].chars().nth(1), None, "{drawn:?}");
        assert_eq!(drawn[rows - 1].chars().nth(1), Some('█'), "{drawn:?}");
    }

    #[test]
    fn the_mirror_hangs_a_reflection_below_the_band() {
        let rows = 4;
        let drawn = render(&levels(rows), Style::Mirror, Vec2::new(3, rows));
        // The bars are squeezed into the upper rows, and the lower ones carry a
        // shorter copy hanging from the surface.
        let reflected = rows / 2;
        assert!(!drawn[rows - reflected].is_empty(), "{drawn:?}");
        assert!(
            !drawn[rows - 1].starts_with('▁'),
            "the floor should belong to the reflection: {drawn:?}"
        );
    }

    #[test]
    fn the_wave_draws_one_cell_per_column() {
        let rows = 4;
        let drawn = render(&levels(rows), Style::Wave, Vec2::new(3, rows));
        let drawn: String = drawn.concat();
        assert_eq!(
            drawn.chars().filter(|c| *c != ' ').count(),
            3,
            "one glyph per column, got {drawn:?}"
        );
    }

    #[test]
    fn the_vu_meter_fills_one_row() {
        let rows = 3;
        let drawn = render(&levels(rows), Style::Vu, Vec2::new(3, rows));
        let meter = drawn.iter().filter(|row| row.contains('█')).count();
        assert_eq!(meter, 1, "the meter is one row: {drawn:?}");
    }
}
