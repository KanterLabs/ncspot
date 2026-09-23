//! The loudness of a track over its length, learned by listening to it.
//!
//! Nothing in the Spotify API hands out a waveform, so this builds one the only
//! way a player can: while a track plays, the level coming out of the audio tap is
//! written into the bucket for the position it was heard at. The first listen
//! fills the shape in behind the playhead; every later listen has it ready up
//! front, and the progress bar can be drawn as the track's own shape rather than
//! as a plain line.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

#[cfg(not(test))]
use log::error;

#[cfg(not(test))]
use crate::config::{self, CACHE_VERSION};
use crate::queue::Queue;
use crate::spotify::{PlayerEvent, Spotify};

#[cfg(not(test))]
const CACHE_FILE: &str = "waveforms.db";
/// Buckets a track is divided into. At four minutes that is a bucket per second,
/// which is finer than any terminal is wide.
const BUCKETS: usize = 256;
/// Tracks kept. Each is a kilobyte of floats, so this is a small file.
const MAX_TRACKS: usize = 512;
/// How often the shapes learned so far are written out. Often enough that a
/// crash costs one track, rare enough that it is not in the way of playback.
const SAVE_EVERY: Duration = Duration::from_secs(60);
/// A bucket below this counts as never having been heard, and is drawn as unknown
/// rather than as silence.
const UNHEARD: f32 = f32::EPSILON;

#[derive(Serialize, Deserialize)]
struct Entry {
    id: String,
    /// Peak level heard in each bucket, `0.0` where the track has not played yet.
    levels: Vec<f32>,
    /// Bumped whenever the track is played, so trimming drops the least used.
    #[serde(default)]
    used: u64,
}

/// Shapes for every track heard, in memory and on disk.
#[derive(Default)]
pub struct WaveformStore {
    tracks: RwLock<HashMap<String, Entry>>,
    clock: RwLock<u64>,
    last_save: RwLock<Option<Instant>>,
}

/// The process-wide store, loaded from disk the first time it is asked for.
pub fn shared() -> Arc<WaveformStore> {
    static SHARED: std::sync::OnceLock<Arc<WaveformStore>> = std::sync::OnceLock::new();
    SHARED
        .get_or_init(|| {
            let store = Arc::new(WaveformStore::default());
            // Tests share the store, so a view can be shown a shape, but they never
            // read or write the shapes the user has built up.
            #[cfg(not(test))]
            store.load();
            store
        })
        .clone()
}

impl WaveformStore {
    /// Note that `level` was heard `position_ms` into a track of `duration_ms`.
    ///
    /// The loudest level heard in a bucket is the one kept: a waveform is drawn
    /// from peaks, and averaging would flatten every track towards the same line.
    pub fn record(&self, id: &str, position_ms: u128, duration_ms: u32, level: f32) {
        let Some(bucket) = bucket_of(position_ms, duration_ms) else {
            return;
        };

        let mut tracks = self.tracks.write().unwrap();
        let used = {
            let mut clock = self.clock.write().unwrap();
            *clock += 1;
            *clock
        };
        let entry = tracks.entry(id.to_string()).or_insert_with(|| Entry {
            id: id.to_string(),
            levels: vec![0.0; BUCKETS],
            used,
        });
        entry.used = used;
        if entry.levels.len() != BUCKETS {
            entry.levels.resize(BUCKETS, 0.0);
        }
        entry.levels[bucket] = entry.levels[bucket].max(level);
        trim(&mut tracks);
        drop(tracks);

        self.maybe_save();
    }

    /// The shape of a track resampled to `width` columns, each 0 to 1, with `None`
    /// for a column the track has not been heard at yet.
    ///
    /// `None` overall when nothing at all is known, so the caller can draw its
    /// ordinary bar instead of an empty one.
    pub fn shape(&self, id: &str, width: usize) -> Option<Vec<Option<f32>>> {
        if width == 0 {
            return None;
        }
        let tracks = self.tracks.read().unwrap();
        let levels = &tracks.get(id)?.levels;
        if levels.iter().all(|level| *level <= UNHEARD) {
            return None;
        }

        let shape: Vec<Option<f32>> = (0..width)
            .map(|column| {
                let first = column * levels.len() / width;
                let last = (((column + 1) * levels.len()).div_ceil(width)).max(first + 1);
                let peak = levels
                    .get(first..last.min(levels.len()))
                    .unwrap_or_default()
                    .iter()
                    .fold(0.0f32, |peak, level| peak.max(*level));
                (peak > UNHEARD).then_some(peak)
            })
            .collect();
        Some(shape)
    }

    /// Write the shapes out, but no more often than `SAVE_EVERY`.
    fn maybe_save(&self) {
        {
            let mut last = self.last_save.write().unwrap();
            if last.is_some_and(|at| at.elapsed() < SAVE_EVERY) {
                return;
            }
            *last = Some(Instant::now());
        }
        // Off the drawing thread: this is called while a frame is being built.
        let store = shared();
        std::thread::spawn(move || store.save());
    }

    #[cfg(not(test))]
    fn load(&self) {
        let path = config::cache_path(CACHE_FILE);
        let Ok(file) = std::fs::File::open(&path) else {
            return;
        };
        match serde_json::from_reader::<_, StoredOwned>(file) {
            Ok(stored) if stored.version == CACHE_VERSION => {
                let mut tracks = self.tracks.write().unwrap();
                let mut clock = self.clock.write().unwrap();
                for entry in stored.entries {
                    *clock = (*clock).max(entry.used);
                    tracks.insert(entry.id.clone(), entry);
                }
            }
            Ok(_) => log::debug!("ignoring waveforms from an older cache version"),
            Err(e) => error!("could not read waveform cache: {e}"),
        }
    }

    /// Tests never touch the user's cache file.
    #[cfg(test)]
    pub fn save(&self) {}

    /// Write every shape out, replacing whatever is on disk.
    #[cfg(not(test))]
    pub fn save(&self) {
        let tracks = self.tracks.read().unwrap();
        let stored = Stored {
            version: CACHE_VERSION,
            entries: tracks.values().collect(),
        };
        let path = config::cache_path(CACHE_FILE);
        match std::fs::File::create(&path) {
            Ok(file) => {
                if let Err(e) = serde_json::to_writer(file, &stored) {
                    error!("could not write waveform cache: {e}");
                }
            }
            Err(e) => error!("could not open waveform cache: {e}"),
        }
    }
}

/// Note how loud the playing track is at the position it is at now.
///
/// Called from the views that draw often, since what they are really doing is
/// sampling: the more frames a view draws, the finer the shape it learns.
pub fn observe(spotify: &Spotify, queue: &Queue) {
    if !matches!(spotify.get_current_status(), PlayerEvent::Playing(_)) {
        return;
    }
    let tap = spotify.audio_tap();
    if !tap.is_live() {
        return;
    }
    let Some(playable) = queue.get_current() else {
        return;
    };
    let Some(id) = playable.id() else {
        return;
    };
    shared().record(
        &id,
        spotify.get_current_progress().as_millis(),
        playable.duration(),
        tap.level(),
    );
}

/// Which bucket a position falls in, or `None` for a track with no length, which
/// is a live stream or an item still loading.
fn bucket_of(position_ms: u128, duration_ms: u32) -> Option<usize> {
    if duration_ms == 0 {
        return None;
    }
    let bucket = position_ms.saturating_mul(BUCKETS as u128) / duration_ms as u128;
    Some((bucket as usize).min(BUCKETS - 1))
}

/// Drop the least recently played tracks once the store outgrows its budget.
fn trim(tracks: &mut HashMap<String, Entry>) {
    while tracks.len() > MAX_TRACKS {
        let Some(oldest) = tracks
            .values()
            .min_by_key(|entry| entry.used)
            .map(|entry| entry.id.clone())
        else {
            return;
        };
        tracks.remove(&oldest);
    }
}

#[derive(Serialize)]
#[cfg(not(test))]
struct Stored<'a> {
    version: u16,
    entries: Vec<&'a Entry>,
}

#[derive(Deserialize)]
#[cfg(not(test))]
struct StoredOwned {
    version: u16,
    entries: Vec<Entry>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_track_is_only_shaped_where_it_has_been_heard() {
        let store = WaveformStore::default();
        assert!(store.shape("solaris", 8).is_none());

        // Heard a quarter of the way in, and nowhere else.
        store.record("solaris", 25_000, 100_000, 0.8);
        let shape = store.shape("solaris", 4).expect("one bucket is known");
        assert_eq!(shape[0], None);
        assert_eq!(shape[1], Some(0.8));
        assert_eq!(shape[2], None);
    }

    #[test]
    fn a_bucket_keeps_the_loudest_level_it_heard() {
        let store = WaveformStore::default();
        store.record("solaris", 0, 100_000, 0.4);
        store.record("solaris", 100, 100_000, 0.9);
        store.record("solaris", 200, 100_000, 0.2);
        assert_eq!(store.shape("solaris", 1).unwrap()[0], Some(0.9));
    }

    #[test]
    fn a_track_with_no_length_is_not_recorded() {
        let store = WaveformStore::default();
        store.record("stream", 5_000, 0, 0.5);
        assert!(store.shape("stream", 4).is_none());
        assert_eq!(bucket_of(5_000, 0), None);
        // The end of a track lands in the last bucket rather than past it.
        assert_eq!(bucket_of(100_000, 100_000), Some(BUCKETS - 1));
    }

    #[test]
    fn the_store_stays_within_its_budget() {
        let store = WaveformStore::default();
        for index in 0..MAX_TRACKS + 5 {
            store.record(&format!("track {index}"), 0, 1000, 0.5);
        }
        assert_eq!(store.tracks.read().unwrap().len(), MAX_TRACKS);
        assert!(store.shape("track 0", 4).is_none());
        assert!(
            store
                .shape(&format!("track {}", MAX_TRACKS + 4), 4)
                .is_some()
        );
    }
}
