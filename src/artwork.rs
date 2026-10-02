//! Bounded album-art loading for the local OpenTUI RPC.
//!
//! Artwork is deliberately kept separate from the legacy Cursive renderer.  The
//! RPC only needs a small, exact RGB grid, while the existing renderer keeps its
//! own decoded-cover cache and cell quantisation.  This cache is bounded and its
//! mutex is never held while a cover is read, downloaded, or decoded.

#[cfg(any(test, feature = "album_art", feature = "cover"))]
use base64::Engine;
use std::hash::Hash;
use std::sync::Mutex;
use std::time::Instant;

#[cfg(any(feature = "album_art", feature = "cover"))]
use log::debug;
#[cfg(any(feature = "album_art", feature = "cover"))]
use std::fs;
#[cfg(any(feature = "album_art", feature = "cover"))]
use std::io::{Cursor, Read};
#[cfg(any(feature = "album_art", feature = "cover"))]
use std::path::{Path, PathBuf};
#[cfg(any(feature = "album_art", feature = "cover"))]
use std::sync::atomic::{AtomicU64, Ordering};
#[cfg(any(feature = "album_art", feature = "cover"))]
use std::time::Duration;
#[cfg(any(feature = "album_art", feature = "cover"))]
use url::Url;

pub const DEFAULT_WIDTH: usize = 20;
pub const DEFAULT_HEIGHT: usize = 10;
pub const MAX_WIDTH: usize = 40;
pub const MAX_HEIGHT: usize = 20;
const MAX_IMAGE_CACHE_ENTRIES: usize = 8;
const MAX_IMAGE_CACHE_BYTES: usize = 20 * 1024 * 1024;

/// Keep the response cache small.  The largest response is 40*40 RGB cells,
/// represented as seven-byte strings (including `#`); at most 64 resized covers
/// are retained, including the string allocation overhead.
#[cfg(any(test, feature = "album_art", feature = "cover"))]
const MAX_CACHE_ENTRIES: usize = 64;
/// A cover is expected to be a small CDN image.  This cap applies both to a
/// downloaded body and to a file already present in the shared cover cache.
#[cfg(any(feature = "album_art", feature = "cover"))]
const MAX_SOURCE_BYTES: u64 = 8 * 1024 * 1024;
/// Restrict decoded dimensions and allocations before asking `image` to expand
/// compressed input.  Spotify cover art is normally 640 or 300 pixels square.
#[cfg(any(feature = "album_art", feature = "cover"))]
const MAX_SOURCE_DIMENSION: u32 = 4096;
#[cfg(any(feature = "album_art", feature = "cover"))]
const MAX_DECODE_BYTES: u64 = 64 * 1024 * 1024;
#[cfg(any(feature = "album_art", feature = "cover"))]
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
#[cfg(any(feature = "album_art", feature = "cover"))]
const MAX_IMAGE_DIMENSION: u32 = 640;

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
struct CacheKey {
    url: String,
    width: usize,
    height: usize,
}

#[derive(Debug)]
struct CacheEntry {
    key: CacheKey,
    pixels: Vec<String>,
    used_at: Instant,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArtworkImage {
    pub width: u32,
    pub height: u32,
    pub data: String,
}

#[derive(Debug)]
struct ImageCacheEntry {
    key: String,
    image: ArtworkImage,
    bytes: usize,
    used_at: Instant,
}

#[derive(Debug)]
pub struct ArtworkResponse {
    pub pixels: Vec<String>,
    pub image: Option<ArtworkImage>,
}

/// A bounded cache of resized artwork responses.
pub struct ArtworkCache {
    entries: Mutex<Vec<CacheEntry>>,
    images: Mutex<Vec<ImageCacheEntry>>,
}

impl Default for ArtworkCache {
    fn default() -> Self {
        Self::new()
    }
}

impl ArtworkCache {
    pub fn new() -> Self {
        Self {
            entries: Mutex::new(Vec::new()),
            images: Mutex::new(Vec::new()),
        }
    }

    /// Return a cached response or load, decode, and resize the cover.
    ///
    /// The cache lock is held only for the lookup and insertion.  In particular,
    /// network I/O and image decompression happen after the lookup lock has been
    /// released, so an unavailable CDN cannot block another RPC request forever.
    #[cfg(test)]
    pub fn get_or_fetch(
        &self,
        url: &str,
        width: usize,
        height: usize,
    ) -> Result<Vec<String>, ArtworkError> {
        self.get_or_fetch_with_image(url, width, height, false)
            .map(|response| response.pixels)
    }

    /// Return a cached response, optionally including a normalized PNG image.
    ///
    /// The source image is loaded at most once for a cache miss.  If the cell
    /// grid is already cached, PNG generation only reads the shared on-disk
    /// source; it never starts another network request.  The cache mutexes are
    /// held only for lookups and insertions.
    pub fn get_or_fetch_with_image(
        &self,
        url: &str,
        width: usize,
        height: usize,
        want_image: bool,
    ) -> Result<ArtworkResponse, ArtworkError> {
        validate_dimensions(width, height)?;
        let key = CacheKey {
            url: url.to_owned(),
            width,
            height,
        };
        let cached_pixels = self.cached(&key);
        let cached_image = want_image.then(|| self.cached_image(url)).flatten();
        if let Some(pixels) = cached_pixels.as_ref()
            && (!want_image || cached_image.is_some())
        {
            return Ok(ArtworkResponse {
                pixels: pixels.clone(),
                image: cached_image,
            });
        }

        #[cfg(any(feature = "album_art", feature = "cover"))]
        {
            let mut source = None;
            let pixels = match cached_pixels {
                Some(pixels) => pixels,
                None => {
                    let (bytes, downloaded) = load_source(url)?;
                    let pixels = decode_pixels(&bytes, width, height)?;
                    if downloaded {
                        persist_cached(&cover_path(url)?, &bytes);
                    }
                    self.remember(key, pixels.clone());
                    source = Some(bytes);
                    pixels
                }
            };

            let image = if !want_image {
                None
            } else if cached_image.is_some() {
                cached_image
            } else {
                // A grid cache hit means the source may already be on disk,
                // but it must never trigger a second CDN request here.
                let bytes = match source {
                    Some(bytes) => Ok(bytes),
                    None => read_source_cached(url),
                };
                bytes
                    .ok()
                    .and_then(|bytes| encode_png(&bytes).ok())
                    .inspect(|image| self.remember_image(url, image.clone()))
            };

            Ok(ArtworkResponse { pixels, image })
        }
        #[cfg(not(any(feature = "album_art", feature = "cover")))]
        {
            let _ = (url, key, cached_pixels, cached_image);
            Err(ArtworkError::FeatureDisabled)
        }
    }

    fn cached(&self, key: &CacheKey) -> Option<Vec<String>> {
        let mut entries = self.entries.lock().unwrap();
        let entry = entries.iter_mut().find(|entry| entry.key == *key)?;
        entry.used_at = Instant::now();
        Some(entry.pixels.clone())
    }

    fn cached_image(&self, url: &str) -> Option<ArtworkImage> {
        let mut images = self.images.lock().unwrap();
        let entry = images.iter_mut().find(|entry| entry.key == url)?;
        entry.used_at = Instant::now();
        Some(entry.image.clone())
    }

    fn remember_image(&self, url: &str, image: ArtworkImage) {
        let bytes = image.data.len();
        if bytes > MAX_IMAGE_CACHE_BYTES {
            return;
        }

        let mut images = self.images.lock().unwrap();
        let now = Instant::now();
        let mut total = images.iter().map(|entry| entry.bytes).sum::<usize>();
        if let Some(index) = images.iter().position(|entry| entry.key == url) {
            total = total.saturating_sub(images.remove(index).bytes);
        }

        while (images.len() >= MAX_IMAGE_CACHE_ENTRIES
            || total.saturating_add(bytes) > MAX_IMAGE_CACHE_BYTES)
            && !images.is_empty()
        {
            let Some(oldest) = images
                .iter()
                .enumerate()
                .min_by_key(|(_, entry)| entry.used_at)
                .map(|(index, _)| index)
            else {
                break;
            };
            total = total.saturating_sub(images[oldest].bytes);
            images.remove(oldest);
        }
        if total.saturating_add(bytes) <= MAX_IMAGE_CACHE_BYTES {
            images.push(ImageCacheEntry {
                key: url.to_owned(),
                image,
                bytes,
                used_at: now,
            });
        }
    }

    #[cfg(any(test, feature = "album_art", feature = "cover"))]
    fn remember(&self, key: CacheKey, pixels: Vec<String>) {
        let mut entries = self.entries.lock().unwrap();
        let now = Instant::now();
        if let Some(entry) = entries.iter_mut().find(|entry| entry.key == key) {
            entry.pixels = pixels;
            entry.used_at = now;
            return;
        }
        if entries.len() >= MAX_CACHE_ENTRIES
            && let Some(oldest) = entries
                .iter()
                .enumerate()
                .min_by_key(|(_, entry)| entry.used_at)
                .map(|(index, _)| index)
        {
            entries.remove(oldest);
        }
        entries.push(CacheEntry {
            key,
            pixels,
            used_at: now,
        });
    }

    #[cfg(test)]
    pub(crate) fn remember_for_test(
        &self,
        url: &str,
        width: usize,
        height: usize,
        pixels: Vec<String>,
    ) {
        self.remember(
            CacheKey {
                url: url.to_owned(),
                width,
                height,
            },
            pixels,
        );
    }

    #[cfg(test)]
    fn len_for_test(&self) -> usize {
        self.entries.lock().unwrap().len()
    }

    #[cfg(test)]
    pub(crate) fn remember_image_for_test(&self, url: &str, image: ArtworkImage) {
        self.remember_image(url, image);
    }

    #[cfg(test)]
    fn image_len_for_test(&self) -> usize {
        self.images.lock().unwrap().len()
    }

    #[cfg(test)]
    fn image_bytes_for_test(&self) -> usize {
        self.images
            .lock()
            .unwrap()
            .iter()
            .map(|entry| entry.bytes)
            .sum()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArtworkError {
    InvalidDimensions,
    #[cfg(any(feature = "album_art", feature = "cover"))]
    InvalidUrl,
    #[cfg(any(feature = "album_art", feature = "cover"))]
    UnsupportedUrl,
    #[cfg(not(any(feature = "album_art", feature = "cover")))]
    FeatureDisabled,
    #[cfg(any(feature = "album_art", feature = "cover"))]
    TooLarge,
    #[cfg(any(feature = "album_art", feature = "cover"))]
    OfflineUnavailable,
    #[cfg(any(feature = "album_art", feature = "cover"))]
    DecodeFailed,
    #[cfg(any(feature = "album_art", feature = "cover"))]
    CacheUnavailable,
}

impl ArtworkError {
    pub const fn reason(self) -> &'static str {
        match self {
            Self::InvalidDimensions => "invalid_dimensions",
            #[cfg(any(feature = "album_art", feature = "cover"))]
            Self::InvalidUrl => "invalid_cover_url",
            #[cfg(any(feature = "album_art", feature = "cover"))]
            Self::UnsupportedUrl => "unsupported_cover_url",
            #[cfg(not(any(feature = "album_art", feature = "cover")))]
            Self::FeatureDisabled => "image_support_unavailable",
            #[cfg(any(feature = "album_art", feature = "cover"))]
            Self::TooLarge => "image_too_large",
            #[cfg(any(feature = "album_art", feature = "cover"))]
            Self::OfflineUnavailable => "offline_unavailable",
            #[cfg(any(feature = "album_art", feature = "cover"))]
            Self::DecodeFailed => "image_decode_failed",
            #[cfg(any(feature = "album_art", feature = "cover"))]
            Self::CacheUnavailable => "cover_cache_unavailable",
        }
    }
}

fn validate_dimensions(width: usize, height: usize) -> Result<(), ArtworkError> {
    if (1..=MAX_WIDTH).contains(&width) && (1..=MAX_HEIGHT).contains(&height) {
        Ok(())
    } else {
        Err(ArtworkError::InvalidDimensions)
    }
}

#[cfg(any(feature = "album_art", feature = "cover"))]
fn load_source(url: &str) -> Result<(Vec<u8>, bool), ArtworkError> {
    let path = cover_path(url)?;
    let (bytes, downloaded) = match read_cached(&path) {
        Ok(bytes) => (bytes, false),
        Err(ArtworkError::CacheUnavailable) if !path.exists() => (download(url)?, true),
        Err(error) => return Err(error),
    };
    Ok((bytes, downloaded))
}

#[cfg(any(feature = "album_art", feature = "cover"))]
fn read_source_cached(url: &str) -> Result<Vec<u8>, ArtworkError> {
    read_cached(&cover_path(url)?)
}

#[cfg(any(feature = "album_art", feature = "cover"))]
fn cover_path(url: &str) -> Result<PathBuf, ArtworkError> {
    let parsed = Url::parse(url).map_err(|_| ArtworkError::InvalidUrl)?;
    if parsed.scheme() != "https" {
        return Err(ArtworkError::UnsupportedUrl);
    }
    // Existing files are trusted as cache entries even when their URL came
    // from an older metadata source.  The CDN allowlist below is applied only
    // on a cache miss, before network I/O.
    let path = crate::utils::cache_path_for_url(url.to_owned());
    let filename = path
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty() && *name != "." && *name != "..")
        .ok_or(ArtworkError::InvalidUrl)?;
    // `cache_path_for_url` currently uses the final URL component.  Keep the
    // check explicit so a future path helper cannot accidentally permit a
    // traversal component here.
    if filename.contains(std::path::MAIN_SEPARATOR) {
        return Err(ArtworkError::InvalidUrl);
    }
    Ok(path)
}

#[cfg(any(feature = "album_art", feature = "cover"))]
fn read_cached(path: &Path) -> Result<Vec<u8>, ArtworkError> {
    let metadata = fs::metadata(path).map_err(|_| ArtworkError::CacheUnavailable)?;
    if !metadata.is_file() || metadata.len() > MAX_SOURCE_BYTES {
        return Err(ArtworkError::TooLarge);
    }
    let file = fs::File::open(path).map_err(|_| ArtworkError::CacheUnavailable)?;
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(MAX_SOURCE_BYTES.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|_| ArtworkError::CacheUnavailable)?;
    if bytes.len() as u64 > MAX_SOURCE_BYTES {
        return Err(ArtworkError::TooLarge);
    }
    Ok(bytes)
}

#[cfg(any(feature = "album_art", feature = "cover"))]
fn download(url: &str) -> Result<Vec<u8>, ArtworkError> {
    let parsed = Url::parse(url).map_err(|_| ArtworkError::InvalidUrl)?;
    let host = parsed.host_str().ok_or(ArtworkError::InvalidUrl)?;
    if !(host.ends_with(".scdn.co") || host.ends_with(".spotifycdn.com")) {
        return Err(ArtworkError::UnsupportedUrl);
    }
    let client = reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_secs(3))
        .timeout(REQUEST_TIMEOUT)
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|_| ArtworkError::OfflineUnavailable)?;
    let response = client
        .get(url)
        .send()
        .map_err(|_| ArtworkError::OfflineUnavailable)?;
    if !response.status().is_success() {
        return Err(ArtworkError::OfflineUnavailable);
    }
    if response
        .content_length()
        .is_some_and(|length| length > MAX_SOURCE_BYTES)
    {
        return Err(ArtworkError::TooLarge);
    }
    let mut bytes = Vec::new();
    response
        .take(MAX_SOURCE_BYTES.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|_| ArtworkError::OfflineUnavailable)?;
    if bytes.len() as u64 > MAX_SOURCE_BYTES {
        return Err(ArtworkError::TooLarge);
    }
    Ok(bytes)
}

#[cfg(any(feature = "album_art", feature = "cover"))]
fn decode_pixels(bytes: &[u8], width: usize, height: usize) -> Result<Vec<String>, ArtworkError> {
    let image = decode_image(bytes)?;
    let resized = image.resize_exact(
        width as u32,
        (height * 2) as u32,
        image::imageops::FilterType::Triangle,
    );
    Ok(resized
        .to_rgb8()
        .pixels()
        .map(|pixel| {
            let [red, green, blue] = pixel.0;
            format!("#{red:02X}{green:02X}{blue:02X}")
        })
        .collect())
}

#[cfg(any(feature = "album_art", feature = "cover"))]
fn decode_image(bytes: &[u8]) -> Result<image::DynamicImage, ArtworkError> {
    let mut reader = image::ImageReader::new(Cursor::new(bytes));
    reader = reader
        .with_guessed_format()
        .map_err(|_| ArtworkError::DecodeFailed)?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_SOURCE_DIMENSION);
    limits.max_image_height = Some(MAX_SOURCE_DIMENSION);
    limits.max_alloc = Some(MAX_DECODE_BYTES);
    reader.limits(limits);
    reader.decode().map_err(|error| match error {
        image::ImageError::Limits(_) => ArtworkError::TooLarge,
        _ => ArtworkError::DecodeFailed,
    })
}

#[cfg(any(feature = "album_art", feature = "cover"))]
fn encode_png(bytes: &[u8]) -> Result<ArtworkImage, ArtworkError> {
    let original = decode_image(bytes)?;
    let image = if original.width() > MAX_IMAGE_DIMENSION || original.height() > MAX_IMAGE_DIMENSION
    {
        original.resize(
            MAX_IMAGE_DIMENSION,
            MAX_IMAGE_DIMENSION,
            image::imageops::FilterType::Lanczos3,
        )
    } else {
        original
    };
    let (width, height) = (image.width(), image.height());
    let mut encoded = Cursor::new(Vec::new());
    image
        .write_to(&mut encoded, image::ImageFormat::Png)
        .map_err(|_| ArtworkError::DecodeFailed)?;
    Ok(ArtworkImage {
        width,
        height,
        data: base64::engine::general_purpose::STANDARD.encode(encoded.into_inner()),
    })
}

#[cfg(any(feature = "album_art", feature = "cover"))]
fn persist_cached(path: &Path, bytes: &[u8]) {
    let Some(parent) = path.parent() else {
        return;
    };
    if fs::create_dir_all(parent).is_err() {
        return;
    }
    static TEMP_ID: AtomicU64 = AtomicU64::new(0);
    let id = TEMP_ID.fetch_add(1, Ordering::Relaxed);
    let Some(filename) = path.file_name().and_then(|name| name.to_str()) else {
        return;
    };
    let temporary = parent.join(format!(".{filename}.tmp-{}-{id}", std::process::id()));
    if fs::write(&temporary, bytes).is_ok()
        && let Err(error) = fs::rename(&temporary, path)
    {
        debug!("could not persist cover cache {path:?}: {error}");
        let _ = fs::remove_file(temporary);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dimensions_are_strictly_bounded() {
        assert!(validate_dimensions(1, 1).is_ok());
        assert!(validate_dimensions(MAX_WIDTH, MAX_HEIGHT).is_ok());
        assert_eq!(
            validate_dimensions(0, 1),
            Err(ArtworkError::InvalidDimensions)
        );
        assert_eq!(
            validate_dimensions(MAX_WIDTH + 1, 1),
            Err(ArtworkError::InvalidDimensions)
        );
        assert_eq!(
            validate_dimensions(1, MAX_HEIGHT + 1),
            Err(ArtworkError::InvalidDimensions)
        );
    }

    #[test]
    fn response_cache_is_bounded_and_dimension_aware() {
        let cache = ArtworkCache::new();
        for index in 0..(MAX_CACHE_ENTRIES + 4) {
            cache.remember_for_test(
                &format!("https://i.scdn.co/image/{index}"),
                1,
                1,
                vec![format!("#{index:06X}")],
            );
        }
        assert_eq!(cache.len_for_test(), MAX_CACHE_ENTRIES);
        cache.remember_for_test(
            "https://example.invalid/image/same",
            1,
            1,
            vec!["#010203".into()],
        );
        cache.remember_for_test(
            "https://example.invalid/image/same",
            2,
            1,
            vec!["#040506".into(); 4],
        );
        assert_eq!(
            cache
                .get_or_fetch("https://example.invalid/image/same", 1, 1)
                .unwrap(),
            vec!["#010203"]
        );
        assert_eq!(
            cache
                .get_or_fetch("https://example.invalid/image/same", 2, 1)
                .unwrap(),
            vec!["#040506"; 4]
        );
    }

    #[test]
    fn image_cache_is_bounded_by_entries_and_encoded_bytes() {
        let cache = ArtworkCache::new();
        for index in 0..(MAX_IMAGE_CACHE_ENTRIES + 3) {
            cache.remember_image_for_test(
                &format!("https://i.scdn.co/image/{index}"),
                ArtworkImage {
                    width: 1,
                    height: 1,
                    data: format!("png-{index}"),
                },
            );
        }
        assert_eq!(cache.image_len_for_test(), MAX_IMAGE_CACHE_ENTRIES);
        assert!(cache.image_bytes_for_test() <= MAX_IMAGE_CACHE_BYTES);

        cache.remember_image_for_test(
            "https://i.scdn.co/image/too-large",
            ArtworkImage {
                width: 1,
                height: 1,
                data: "x".repeat(MAX_IMAGE_CACHE_BYTES + 1),
            },
        );
        assert_eq!(cache.image_len_for_test(), MAX_IMAGE_CACHE_ENTRIES);
        assert!(cache.image_bytes_for_test() <= MAX_IMAGE_CACHE_BYTES);
    }

    #[cfg(any(feature = "album_art", feature = "cover"))]
    #[test]
    fn deterministic_png_decodes_to_exact_rgb_grid() {
        let source = image::RgbImage::from_fn(2, 2, |x, y| {
            if x == 0 && y == 0 {
                image::Rgb([255, 0, 0])
            } else if x == 1 && y == 0 {
                image::Rgb([0, 255, 0])
            } else if x == 0 {
                image::Rgb([0, 0, 255])
            } else {
                image::Rgb([255, 255, 255])
            }
        });
        let mut bytes = Cursor::new(Vec::new());
        image::DynamicImage::ImageRgb8(source)
            .write_to(&mut bytes, image::ImageFormat::Png)
            .unwrap();

        let pixels = decode_pixels(bytes.get_ref(), 2, 1).unwrap();
        assert_eq!(pixels.len(), 4);
        assert_eq!(pixels[0], "#FF0000");
        assert_eq!(pixels[1], "#00FF00");
        assert_eq!(pixels[2], "#0000FF");
        assert_eq!(pixels[3], "#FFFFFF");
    }

    #[cfg(any(feature = "album_art", feature = "cover"))]
    #[test]
    fn deterministic_png_normalizes_without_upscaling_and_keeps_detail() {
        let source = image::RgbImage::from_fn(1280, 320, |x, y| {
            if x < 640 {
                image::Rgb([255, y as u8, 0])
            } else {
                image::Rgb([0, y as u8, 255])
            }
        });
        let mut bytes = Cursor::new(Vec::new());
        image::DynamicImage::ImageRgb8(source)
            .write_to(&mut bytes, image::ImageFormat::Png)
            .unwrap();

        let image = encode_png(bytes.get_ref()).unwrap();
        assert_eq!((image.width, image.height), (640, 160));
        let encoded = base64::engine::general_purpose::STANDARD
            .decode(&image.data)
            .unwrap();
        let decoded = image::load_from_memory(&encoded).unwrap().to_rgb8();
        assert_eq!((decoded.width(), decoded.height()), (640, 160));
        assert_eq!(decoded.get_pixel(0, 0).0[0], 255);
        assert!(decoded.get_pixel(0, 0).0[1] <= 2);
        assert_eq!(decoded.get_pixel(0, 0).0[2], 0);
        assert_eq!(decoded.get_pixel(639, 0).0[0], 0);
        assert!(decoded.get_pixel(639, 0).0[1] <= 2);
        assert_eq!(decoded.get_pixel(639, 0).0[2], 255);

        let cache = ArtworkCache::new();
        cache.remember_for_test(
            "https://i.scdn.co/image/deterministic",
            1,
            1,
            vec!["#010203".into()],
        );
        cache.remember_image_for_test("https://i.scdn.co/image/deterministic", image.clone());
        let cached = cache
            .get_or_fetch_with_image("https://i.scdn.co/image/deterministic", 1, 1, true)
            .unwrap();
        assert_eq!(cached.image, Some(image));

        let small = image::RgbImage::from_pixel(2, 3, image::Rgb([12, 34, 56]));
        let mut bytes = Cursor::new(Vec::new());
        image::DynamicImage::ImageRgb8(small)
            .write_to(&mut bytes, image::ImageFormat::Png)
            .unwrap();
        let image = encode_png(bytes.get_ref()).unwrap();
        assert_eq!((image.width, image.height), (2, 3));
    }
}
