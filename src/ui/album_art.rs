use std::collections::HashSet;
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::thread;
use std::time::{Duration, Instant};

use cursive::theme::{Color, ColorStyle, ColorType, PaletteColor};
use cursive::{Printer, Vec2};
use image::imageops::FilterType;
use log::debug;

use crate::events::EventManager;
use crate::ui::anim::{blend, blendable, fade, lift};

/// How long a cover takes to fade up out of the card once it has been decoded, so
/// it arrives rather than popping in a frame after the text.
const FADE: Duration = Duration::from_millis(320);
/// Covers kept decoded at once. The card needs one, a list of results needs one
/// per row, and a handful more costs a few hundred kilobytes.
const MAX_LOADED: usize = 12;
/// Cell grids kept at once. One cover can be scaled to several sizes at the same
/// time: the card's, and the thumbnail in the list behind it.
const MAX_SCALED: usize = 24;

/// Redraws asked for while a cover fades in. The rest of the UI only refreshes a
/// couple of times a second, which is not enough to see a fade, so the fade drives
/// its own frames and then stops.
const FADE_FRAMES: u32 = 12;

/// The quadrant blocks, indexed by which of a cell's four subpixels take the
/// foreground colour: bit 0 is top left, 1 top right, 2 bottom left, 3 bottom
/// right. Every way of splitting a cell in two is in here, so a cell can carry
/// four subpixels in two colours instead of the two a half block manages.
const QUADRANTS: [&str; 16] = [
    " ", "\u{2598}", "\u{259d}", "\u{2580}", "\u{2596}", "\u{258c}", "\u{259e}", "\u{259b}",
    "\u{2597}", "\u{259a}", "\u{2590}", "\u{259c}", "\u{2584}", "\u{2599}", "\u{259f}", "\u{2588}",
];

/// A decoded cover, kept around so a resize does not have to hit the disk again.
struct Loaded {
    url: String,
    image: image::RgbImage,
    /// When this cover became drawable, which is where its fade in starts.
    shown_at: Instant,
}

/// One printed cell: a block glyph and the two colours it is drawn in.
#[derive(Clone, Copy, PartialEq, Debug)]
struct Cell {
    glyph: &'static str,
    foreground: Color,
    background: Color,
}

/// A cover scaled to an exact cell grid, ready to print.
struct Scaled {
    url: String,
    size: Vec2,
    /// Every cell of the grid, row major.
    cells: Vec<Cell>,
}

/// Album art for the now playing card.
///
/// Covers are fetched and decoded off the UI thread, then drawn through cursive's
/// own buffer as half blocks, so the art composes with everything drawn around it
/// and needs no terminal graphics protocol.
pub struct AlbumArt {
    /// Decoded covers, least recently used first.
    loaded: Arc<RwLock<Vec<Loaded>>>,
    /// Cell grids, least recently used first.
    scaled: RwLock<Vec<Scaled>>,
    pending: Arc<RwLock<HashSet<String>>>,
    events: EventManager,
}

impl AlbumArt {
    pub fn new(events: EventManager) -> Self {
        Self {
            loaded: Arc::default(),
            scaled: RwLock::default(),
            pending: Arc::default(),
            events,
        }
    }

    /// Draw the cover at `url` into `size` cells at `offset`, lifted `glow` of the
    /// way towards white so the art can pulse with the music.
    ///
    /// Returns false when the cover is not ready yet; the caller lays out without it
    /// and gets a redraw once the fetch lands.
    pub fn draw(
        &self,
        printer: &Printer<'_, '_>,
        offset: Vec2,
        size: Vec2,
        url: &str,
        glow: f32,
    ) -> bool {
        if size.x == 0 || size.y == 0 {
            return false;
        }
        if !self.prepare(url, size) {
            self.prefetch(url);
            return false;
        }

        // How far into its fade this cover is. Taken before the grid is locked, so
        // the two locks are never held at once.
        let amount = self
            .loaded
            .read()
            .unwrap()
            .iter()
            .find(|loaded| loaded.url == url)
            .map(|loaded| fade(loaded.shown_at.elapsed(), FADE))
            .unwrap_or(1.0);
        let card = printer.theme.palette[PaletteColor::Background];
        // A theme that leaves the background to the terminal gives nothing to fade
        // out of, so those themes get the coarser dim attribute for the first half
        // of the fade rather than no fade at all.
        let dim = !blendable(card) && amount < 0.55;

        let scaled = self.scaled.read().unwrap();
        let Some(scaled) = scaled
            .iter()
            .find(|scaled| scaled.url == url && scaled.size == size)
        else {
            return false;
        };
        for row in 0..size.y {
            for column in 0..size.x {
                let cell = scaled.cells[row * size.x + column];
                let style = ColorStyle::new(
                    ColorType::Color(lift(blend(card, cell.foreground, amount), glow)),
                    ColorType::Color(lift(blend(card, cell.background, amount), glow)),
                );
                printer.with_color(style, |printer| {
                    let print = |printer: &Printer<'_, '_>| {
                        printer.print((offset.x + column, offset.y + row), cell.glyph)
                    };
                    if dim {
                        printer.with_effect(cursive::theme::Effect::Dim, print);
                    } else {
                        print(printer);
                    }
                });
            }
        }
        true
    }

    /// Whether a cover for `url` is already loaded, scaling it to `size` if needed.
    pub fn is_ready(&self, url: &str) -> bool {
        self.loaded
            .read()
            .unwrap()
            .iter()
            .any(|loaded| loaded.url == url)
    }

    /// Make sure `self.scaled` holds this url at this size. False if not loaded yet.
    fn prepare(&self, url: &str, size: Vec2) -> bool {
        if self
            .scaled
            .read()
            .unwrap()
            .iter()
            .any(|scaled| scaled.url == url && scaled.size == size)
        {
            return true;
        }

        let loaded = self.loaded.read().unwrap();
        let Some(loaded) = loaded.iter().find(|loaded| loaded.url == url) else {
            return false;
        };

        // Four subpixels per cell, and the scaling is done in linear light so that
        // shrinking a cover does not wash its colours out the way averaging gamma
        // encoded values does.
        let linear = to_linear(&loaded.image);
        let scaled = image::imageops::resize(
            &linear,
            (size.x * 2) as u32,
            (size.y * 2) as u32,
            FilterType::Lanczos3,
        );
        let cells = (0..size.y)
            .flat_map(|row| {
                let scaled = &scaled;
                (0..size.x).map(move |column| {
                    let (x, y) = ((column * 2) as u32, (row * 2) as u32);
                    quantize([
                        scaled.get_pixel(x, y).0,
                        scaled.get_pixel(x + 1, y).0,
                        scaled.get_pixel(x, y + 1).0,
                        scaled.get_pixel(x + 1, y + 1).0,
                    ])
                })
            })
            .collect();

        let mut grids = self.scaled.write().unwrap();
        grids.retain(|scaled| !(scaled.url == url && scaled.size == size));
        grids.push(Scaled {
            url: url.to_string(),
            size,
            cells,
        });
        while grids.len() > MAX_SCALED {
            grids.remove(0);
        }
        true
    }

    /// Fetch and decode `url` in the background, unless that is already happening.
    pub fn prefetch(&self, url: &str) {
        {
            let mut pending = self.pending.write().unwrap();
            if !pending.insert(url.to_string()) {
                return;
            }
        }

        let url = url.to_string();
        let loaded = self.loaded.clone();
        let pending = self.pending.clone();
        let events = self.events.clone();
        thread::spawn(move || {
            let image = fetch(&url);
            let arrived = image.is_some();
            if let Some(image) = image {
                let mut covers = loaded.write().unwrap();
                covers.retain(|cover| cover.url != url);
                covers.push(Loaded {
                    url: url.clone(),
                    image,
                    shown_at: Instant::now(),
                });
                while covers.len() > MAX_LOADED {
                    covers.remove(0);
                }
            }
            pending.write().unwrap().remove(&url);

            // Drive the fade in. Without these the cover would appear at whatever
            // opacity the next unrelated redraw happened to catch it at.
            if arrived {
                for _ in 0..FADE_FRAMES {
                    if !events.try_trigger() {
                        break;
                    }
                    thread::sleep(FADE / FADE_FRAMES);
                }
            }
        });
    }

    /// Drop the covers loaded longest ago once the cache outgrows its budget.
    #[cfg(test)]
    fn trim_loaded(&self) {
        let mut covers = self.loaded.write().unwrap();
        while covers.len() > MAX_LOADED {
            covers.remove(0);
        }
    }

    /// Install an already decoded cover, so tests do not need the network.
    #[cfg(test)]
    pub fn load_for_test(&self, url: &str, image: image::RgbImage) {
        self.loaded.write().unwrap().push(Loaded {
            url: url.to_string(),
            image,
            // Past its fade already, so a test renders the cover at full strength.
            shown_at: Instant::now()
                .checked_sub(FADE)
                .unwrap_or_else(Instant::now),
        });
    }
}

/// Sizes the cover is reduced to before its colours are counted. Small enough to
/// be quick, big enough that a detail like a jacket does not vanish.
const ACCENT_SAMPLE: u32 = 48;
/// Colours are counted in a coarse cube, so near enough shades land together.
const ACCENT_BINS: u32 = 6;

/// The colour to tint a card with, drawn from the cover itself.
///
/// Shades are counted in a coarse colour cube and each bin is scored on how much
/// of the cover it covers and how vivid it is, so a cover's one bright colour wins
/// over the grey or black that usually covers more of it. Covers with nothing
/// vivid in them give None rather than a muddy tint.
fn accent_of(image: &image::RgbImage) -> Option<Color> {
    let sample = image::imageops::resize(image, ACCENT_SAMPLE, ACCENT_SAMPLE, FilterType::Triangle);

    let bins = (ACCENT_BINS * ACCENT_BINS * ACCENT_BINS) as usize;
    let mut counts = vec![0u32; bins];
    let mut sums = vec![[0u32; 3]; bins];
    for pixel in sample.pixels() {
        let [r, g, b] = pixel.0;
        let bin = |channel: u8| (channel as u32 * ACCENT_BINS / 256).min(ACCENT_BINS - 1);
        let index = (bin(r) * ACCENT_BINS * ACCENT_BINS + bin(g) * ACCENT_BINS + bin(b)) as usize;
        counts[index] += 1;
        for (channel, value) in [r, g, b].into_iter().enumerate() {
            sums[index][channel] += value as u32;
        }
    }

    let mut best: Option<(f32, [u8; 3])> = None;
    for (index, &count) in counts.iter().enumerate() {
        if count == 0 {
            continue;
        }
        let mean = [0, 1, 2].map(|channel| (sums[index][channel] / count) as u8);
        let (saturation, value) = saturation_and_value(mean);
        // Black, white and washed out shades make poor highlights whatever their
        // share of the cover.
        if saturation < 0.28 || !(0.12..=0.97).contains(&value) {
            continue;
        }
        let share = (count as f32).sqrt();
        let score = share * saturation * (0.4 + 0.6 * value);
        if best.is_none_or(|(previous, _)| score > previous) {
            best = Some((score, mean));
        }
    }

    let (_, colour) = best?;
    Some(readable(colour))
}

/// How colourful a pixel is and how bright, both 0 to 1.
fn saturation_and_value(colour: [u8; 3]) -> (f32, f32) {
    let channels = colour.map(|channel| channel as f32 / 255.0);
    let high = channels.iter().cloned().fold(0.0f32, f32::max);
    let low = channels.iter().cloned().fold(1.0f32, f32::min);
    let saturation = if high <= 0.0 {
        0.0
    } else {
        (high - low) / high
    };
    (saturation, high)
}

/// Lift a colour until it reads against a dark terminal, keeping its hue.
fn readable(colour: [u8; 3]) -> Color {
    const FLOOR: f32 = 0.62;
    let (_, value) = saturation_and_value(colour);
    let lifted = if value < FLOOR && value > 0.0 {
        let scale = FLOOR / value;
        colour.map(|channel| ((channel as f32 * scale).min(255.0)) as u8)
    } else {
        colour
    };
    Color::Rgb(lifted[0], lifted[1], lifted[2])
}

/// Pick the block glyph and the two colours that best stand in for a cell's four
/// subpixels, ordered top left, top right, bottom left, bottom right.
///
/// Every split of the four is tried and the one whose two colours sit closest to
/// the subpixels they cover wins, so an edge running through a cell is drawn as an
/// edge rather than smeared into an average.
fn quantize(subpixels: [[f32; 3]; 4]) -> Cell {
    // The solid block is the starting point, so a cell with nothing to split stays
    // one glyph instead of an arbitrary quarter.
    let mut best = (f32::MAX, 15usize, [0.0; 3], [0.0; 3]);
    for mask in (1..16usize).rev() {
        let mut sums = [[0.0f32; 3]; 2];
        let mut counts = [0.0f32; 2];
        for (index, subpixel) in subpixels.iter().enumerate() {
            let group = usize::from(mask >> index & 1 == 0);
            counts[group] += 1.0;
            for channel in 0..3 {
                sums[group][channel] += subpixel[channel];
            }
        }
        let mean = |group: usize| {
            let filled = if counts[group] > 0.0 {
                group
            } else {
                1 - group
            };
            let mut mean = [0.0f32; 3];
            for channel in 0..3 {
                mean[channel] = sums[filled][channel] / counts[filled];
            }
            mean
        };
        let (foreground, background) = (mean(0), mean(1));

        let mut cost = 0.0;
        for (index, subpixel) in subpixels.iter().enumerate() {
            let target = if mask >> index & 1 == 1 {
                foreground
            } else {
                background
            };
            for channel in 0..3 {
                let difference = subpixel[channel] - target[channel];
                cost += difference * difference;
            }
        }
        if cost < best.0 {
            best = (cost, mask, foreground, background);
        }
    }

    let (_, mask, foreground, background) = best;
    Cell {
        glyph: QUADRANTS[mask],
        foreground: to_color(foreground),
        background: to_color(background),
    }
}

/// The image with its channels in linear light, where averaging is meaningful.
fn to_linear(image: &image::RgbImage) -> image::ImageBuffer<image::Rgb<f32>, Vec<f32>> {
    image::ImageBuffer::from_fn(image.width(), image.height(), |x, y| {
        let [r, g, b] = image.get_pixel(x, y).0;
        image::Rgb([srgb_to_linear(r), srgb_to_linear(g), srgb_to_linear(b)])
    })
}

fn srgb_to_linear(value: u8) -> f32 {
    let value = value as f32 / 255.0;
    if value <= 0.04045 {
        value / 12.92
    } else {
        ((value + 0.055) / 1.055).powf(2.4)
    }
}

fn linear_to_srgb(value: f32) -> u8 {
    let value = value.clamp(0.0, 1.0);
    let encoded = if value <= 0.0031308 {
        value * 12.92
    } else {
        1.055 * value.powf(1.0 / 2.4) - 0.055
    };
    (encoded * 255.0).round() as u8
}

fn to_color(linear: [f32; 3]) -> Color {
    Color::Rgb(
        linear_to_srgb(linear[0]),
        linear_to_srgb(linear[1]),
        linear_to_srgb(linear[2]),
    )
}

/// Warm the on-disk cover cache for `urls`, so art for a result that gets played
/// is already there. Downloads happen on one background thread, newest first, and
/// anything already cached or already queued is skipped.
pub fn prefetch_covers(urls: Vec<String>) {
    static QUEUED: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    let queued = QUEUED.get_or_init(Mutex::default);

    let wanted: Vec<String> = {
        let mut queued = queued.lock().unwrap();
        urls.into_iter()
            .filter(|url| queued.insert(url.clone()))
            .collect()
    };
    if wanted.is_empty() {
        return;
    }
    thread::spawn(move || {
        for url in wanted {
            let path = crate::utils::cache_path_for_url(url.clone());
            if !path.exists()
                && let Err(e) = crate::utils::download(url.clone(), path)
            {
                debug!("could not prefetch cover {url}: {e}");
            }
        }
    });
}

/// The colour of the cover at `url`, fetching and decoding it if that is what it
/// takes. Meant for the one call made when the playing track changes, not for
/// drawing: it does the work on the calling thread.
pub fn accent_for(url: &str) -> Option<Color> {
    accent_of(&fetch(url)?)
}

/// Read the cover from the shared cover cache, downloading it first if needed.
fn fetch(url: &str) -> Option<image::RgbImage> {
    let path = crate::utils::cache_path_for_url(url.to_string());
    if !path.exists()
        && let Err(e) = crate::utils::download(url.to_string(), path.clone())
    {
        debug!("could not download cover {url}: {e}");
        return None;
    }

    // Cached covers are named after the URL and have no extension, so the format
    // has to be sniffed from the contents rather than guessed from the path.
    let decoded = image::ImageReader::open(&path)
        .and_then(|reader| reader.with_guessed_format())
        .map_err(|e| e.to_string())
        .and_then(|reader| reader.decode().map_err(|e| e.to_string()));
    match decoded {
        Ok(image) => Some(image.to_rgb8()),
        Err(e) => {
            debug!("could not decode cover {url}: {e}");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{AlbumArt, MAX_LOADED};
    use crate::events::EventManager;
    use cursive::Vec2;
    use cursive::theme::Color;

    /// A 2x4 image: left column red, right column blue, darkening downward.
    fn image() -> image::RgbImage {
        image::RgbImage::from_fn(2, 4, |x, y| {
            let shade = 255 - (y as u8 * 60);
            if x == 0 {
                image::Rgb([shade, 0, 0])
            } else {
                image::Rgb([0, 0, shade])
            }
        })
    }

    fn channels(colour: Color) -> [u8; 3] {
        let Color::Rgb(r, g, b) = colour else {
            panic!("expected an rgb colour");
        };
        [r, g, b]
    }

    #[test]
    fn each_cell_carries_four_subpixels() {
        let art = AlbumArt::new(EventManager::new_for_test());
        art.load_for_test("cover", image());

        assert!(art.prepare("cover", Vec2::new(2, 2)));
        let scaled = art.scaled.read().unwrap();
        let scaled = scaled.last().expect("the grid was cached");
        assert_eq!(scaled.size, Vec2::new(2, 2));
        assert_eq!(scaled.cells.len(), 4);

        // The left column of the cover is red and the right one blue, whichever way
        // round a cell happens to assign its two colours.
        let [r, _, b] = channels(scaled.cells[0].foreground);
        assert!(r > b, "the left cell should be red, got {r} vs {b}");
        let [r, _, b] = channels(scaled.cells[1].foreground);
        assert!(b > r, "the right cell should be blue, got {r} vs {b}");
    }

    #[test]
    fn an_edge_through_a_cell_is_drawn_as_an_edge() {
        // A cell split down the middle: the two colours have to survive whole
        // rather than be averaged into one muddy block.
        let cell = super::quantize([
            [1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0],
            [1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0],
        ]);
        // Left half in one colour: either the left block or its complement.
        assert!(
            ["\u{258c}", "\u{2590}"].contains(&cell.glyph),
            "expected a vertical split, got {}",
            cell.glyph
        );
        let mut colours = [channels(cell.foreground), channels(cell.background)];
        colours.sort();
        assert_eq!(colours, [[0, 0, 255], [255, 0, 0]]);
    }

    #[test]
    fn a_flat_cell_is_one_solid_block() {
        let cell = super::quantize([[0.5, 0.5, 0.5]; 4]);
        assert_eq!(cell.glyph, "\u{2588}");
        assert_eq!(cell.foreground, cell.background);
    }

    #[test]
    fn colours_are_averaged_in_linear_light() {
        // Averaging in gamma encoded values would give 128 for half intensity;
        // linear light encodes it to the perceptually correct, lighter mid grey.
        let [grey, _, _] = channels(super::quantize([[0.5; 3]; 4]).foreground);
        assert!(
            grey > 180,
            "linear 0.5 should encode well above 128, got {grey}"
        );
    }

    #[test]
    fn the_accent_is_the_covers_vivid_colour_not_its_biggest_one() {
        // Mostly dark grey with a band of orange: the orange is what a card wants.
        let cover = image::RgbImage::from_fn(32, 32, |_, y| {
            if y < 26 {
                image::Rgb([40, 40, 42])
            } else {
                image::Rgb([230, 120, 20])
            }
        });
        let [r, g, b] = channels(super::accent_of(&cover).expect("a vivid colour is there"));
        assert!(r > g && g > b, "expected an orange accent, got {r},{g},{b}");
    }

    #[test]
    fn a_colourless_cover_keeps_the_theme() {
        let cover = image::RgbImage::from_fn(32, 32, |_, y| {
            let shade = 30 + (y as u8 * 4);
            image::Rgb([shade, shade, shade])
        });
        assert!(super::accent_of(&cover).is_none());
    }

    #[test]
    fn a_dim_accent_is_lifted_until_it_reads() {
        // A deep, dark blue cover still has to give a colour you can see on a card.
        let cover = image::RgbImage::from_fn(32, 32, |_, _| image::Rgb([10, 14, 60]));
        let [r, g, b] = channels(super::accent_of(&cover).expect("a colour is there"));
        assert!(b > 150, "the accent should be lifted, got {r},{g},{b}");
        assert!(b > r && b > g, "the hue should survive the lift");
    }

    #[test]
    fn a_cover_is_only_ready_for_its_own_url() {
        let art = AlbumArt::new(EventManager::new_for_test());
        art.load_for_test("cover", image());
        assert!(art.is_ready("cover"));
        assert!(!art.is_ready("another"));
        assert!(!art.prepare("another", Vec2::new(2, 2)));
    }

    #[test]
    fn one_cover_can_be_held_at_several_sizes_at_once() {
        // The card and a thumbnail of the same cover are on screen together, so
        // scaling for one must not throw the other away.
        let art = AlbumArt::new(EventManager::new_for_test());
        art.load_for_test("cover", image());
        assert!(art.prepare("cover", Vec2::new(2, 2)));
        assert!(art.prepare("cover", Vec2::new(4, 3)));

        let scaled = art.scaled.read().unwrap();
        let cells = |size: Vec2| {
            scaled
                .iter()
                .find(|scaled| scaled.size == size)
                .map(|scaled| scaled.cells.len())
        };
        assert_eq!(cells(Vec2::new(2, 2)), Some(4));
        assert_eq!(cells(Vec2::new(4, 3)), Some(12));
    }

    #[test]
    fn the_cover_cache_stays_within_its_budget() {
        let art = AlbumArt::new(EventManager::new_for_test());
        for index in 0..MAX_LOADED + 3 {
            art.load_for_test(&format!("cover {index}"), image());
        }
        art.trim_loaded();
        assert_eq!(art.loaded.read().unwrap().len(), MAX_LOADED);
        // The covers that went are the ones loaded longest ago.
        assert!(!art.is_ready("cover 0"));
        assert!(art.is_ready(&format!("cover {}", MAX_LOADED + 2)));
    }
}
