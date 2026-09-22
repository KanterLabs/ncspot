use std::collections::HashSet;
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::thread;

use cursive::theme::{Color, ColorStyle, ColorType};
use cursive::{Printer, Vec2};
use image::imageops::FilterType;
use log::debug;

use crate::events::EventManager;

/// Upper half block: the cell's foreground is the top pixel and its background the
/// bottom one, which doubles the vertical resolution of the art.
const HALF: &str = "▀";

/// A decoded cover, kept around so a resize does not have to hit the disk again.
struct Loaded {
    url: String,
    image: image::RgbImage,
}

/// A cover scaled to an exact cell grid, ready to print.
struct Scaled {
    url: String,
    size: Vec2,
    /// Top and bottom pixel of every cell, row major.
    cells: Vec<(Color, Color)>,
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
                let (top, bottom) = scaled.cells[row * size.x + column];
                let style = ColorStyle::new(ColorType::Color(top), ColorType::Color(bottom));
                printer.with_color(style, |printer| {
                    printer.print((offset.x + column, offset.y + row), HALF);
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

        // Two stacked half blocks per cell, so the pixel grid is twice as tall.
        let scaled = image::imageops::resize(
            &loaded.image,
            size.x as u32,
            (size.y * 2) as u32,
            FilterType::Triangle,
        );
        let cells = (0..size.y)
            .flat_map(|row| {
                let scaled = &scaled;
                (0..size.x).map(move |column| {
                    let pixel = |y: usize| {
                        let [r, g, b] = scaled.get_pixel(column as u32, y as u32).0;
                        Color::Rgb(r, g, b)
                    };
                    (pixel(row * 2), pixel(row * 2 + 1))
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

    #[test]
    fn each_cell_takes_two_stacked_pixels() {
        let art = AlbumArt::new(EventManager::new_for_test());
        art.load_for_test("cover", image());

        assert!(art.prepare("cover", Vec2::new(2, 2)));
        let scaled = art.scaled.read().unwrap();
        let scaled = scaled.as_ref().unwrap();
        assert_eq!(scaled.size, Vec2::new(2, 2));
        assert_eq!(scaled.cells.len(), 4);

        // Top left cell: red, and its background is the row below it, so darker.
        let (top, bottom) = scaled.cells[0];
        let (Color::Rgb(tr, _, _), Color::Rgb(br, _, _)) = (top, bottom) else {
            panic!("expected rgb cells");
        };
        assert!(tr > br, "the lower pixel should be the darker one");
        // Top right cell is the blue column.
        assert!(matches!(scaled.cells[1].0, Color::Rgb(0, 0, _)));
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
