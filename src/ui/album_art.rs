use std::collections::HashSet;
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::thread;

use cursive::theme::{Color, ColorStyle, ColorType};
use cursive::{Printer, Vec2};
use image::imageops::FilterType;
use log::debug;

use crate::events::EventManager;

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
    loaded: Arc<RwLock<Option<Loaded>>>,
    scaled: RwLock<Option<Scaled>>,
    pending: Arc<RwLock<HashSet<String>>>,
    events: EventManager,
}

impl AlbumArt {
    pub fn new(events: EventManager) -> Self {
        Self {
            loaded: Arc::default(),
            scaled: RwLock::new(None),
            pending: Arc::default(),
            events,
        }
    }

    /// Draw the cover at `url` into `size` cells at `offset`.
    ///
    /// Returns false when the cover is not ready yet; the caller lays out without it
    /// and gets a redraw once the fetch lands.
    pub fn draw(&self, printer: &Printer<'_, '_>, offset: Vec2, size: Vec2, url: &str) -> bool {
        if size.x == 0 || size.y == 0 {
            return false;
        }
        if !self.prepare(url, size) {
            self.prefetch(url);
            return false;
        }

        let scaled = self.scaled.read().unwrap();
        let Some(scaled) = scaled.as_ref() else {
            return false;
        };
        for row in 0..size.y {
            for column in 0..size.x {
                let cell = scaled.cells[row * size.x + column];
                let style = ColorStyle::new(
                    ColorType::Color(cell.foreground),
                    ColorType::Color(cell.background),
                );
                printer.with_color(style, |printer| {
                    printer.print((offset.x + column, offset.y + row), cell.glyph);
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
            .as_ref()
            .is_some_and(|loaded| loaded.url == url)
    }

    /// Make sure `self.scaled` holds this url at this size. False if not loaded yet.
    fn prepare(&self, url: &str, size: Vec2) -> bool {
        if self
            .scaled
            .read()
            .unwrap()
            .as_ref()
            .is_some_and(|scaled| scaled.url == url && scaled.size == size)
        {
            return true;
        }

        let loaded = self.loaded.read().unwrap();
        let Some(loaded) = loaded.as_ref().filter(|loaded| loaded.url == url) else {
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

        *self.scaled.write().unwrap() = Some(Scaled {
            url: url.to_string(),
            size,
            cells,
        });
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
            if let Some(image) = image {
                *loaded.write().unwrap() = Some(Loaded {
                    url: url.clone(),
                    image,
                });
                events.trigger();
            }
            pending.write().unwrap().remove(&url);
        });
    }

    /// Install an already decoded cover, so tests do not need the network.
    #[cfg(test)]
    pub fn load_for_test(&self, url: &str, image: image::RgbImage) {
        *self.loaded.write().unwrap() = Some(Loaded {
            url: url.to_string(),
            image,
        });
    }
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
    use super::AlbumArt;
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
        let scaled = scaled.as_ref().unwrap();
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
    fn a_cover_is_only_ready_for_its_own_url() {
        let art = AlbumArt::new(EventManager::new_for_test());
        art.load_for_test("cover", image());
        assert!(art.is_ready("cover"));
        assert!(!art.is_ready("another"));
        assert!(!art.prepare("another", Vec2::new(2, 2)));
    }

    #[test]
    fn rescaling_replaces_the_cached_grid() {
        let art = AlbumArt::new(EventManager::new_for_test());
        art.load_for_test("cover", image());
        assert!(art.prepare("cover", Vec2::new(2, 2)));
        assert!(art.prepare("cover", Vec2::new(4, 3)));
        let scaled = art.scaled.read().unwrap();
        assert_eq!(scaled.as_ref().unwrap().cells.len(), 12);
    }
}
