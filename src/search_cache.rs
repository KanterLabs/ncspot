use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

#[cfg(not(test))]
use log::debug;
use log::error;
use serde::{Deserialize, Serialize};

use crate::config::{self, CACHE_VERSION};
use crate::model::track::Track;

const CACHE_FILE: &str = "searches.db";
/// Queries kept on disk and in memory. Each holds a page of results, so this is a
/// few hundred kilobytes at worst.
const MAX_QUERIES: usize = 256;
/// How long a cached result is served without asking Spotify again. Track metadata
/// barely moves, so this only has to be short enough that a new release shows up.
const FRESH_FOR: Duration = Duration::from_secs(600);

#[derive(Serialize, Deserialize)]
struct Entry {
    query: String,
    tracks: Vec<Track>,
    /// Bumped on every read, so trimming can drop the least recently used query.
    #[serde(default)]
    used: u64,
    /// Entries loaded from disk have no age and are always refreshed once.
    #[serde(skip)]
    fetched: Option<Instant>,
}

/// Remembered search results, in memory and on disk.
///
/// Quick search draws from this before it asks the network, so a repeated or
/// re-typed query lands instantly and a growing query always has the previous
/// prefix's results on screen instead of an empty list.
#[derive(Default)]
pub struct SearchCache {
    entries: RwLock<HashMap<String, Entry>>,
    clock: RwLock<u64>,
}

/// The process-wide cache, loaded from disk the first time it is asked for.
pub fn shared() -> Arc<SearchCache> {
    // Tests get their own empty cache, so they never read or write the user's.
    #[cfg(test)]
    return Arc::new(SearchCache::default());

    #[cfg(not(test))]
    {
        static SHARED: std::sync::OnceLock<Arc<SearchCache>> = std::sync::OnceLock::new();
        SHARED
            .get_or_init(|| {
                let cache = Arc::new(SearchCache::default());
                cache.load();
                cache
            })
            .clone()
    }
}

pub fn normalize(query: &str) -> String {
    query.trim().to_lowercase()
}

impl SearchCache {
    /// Results for exactly this query, if they were fetched recently enough to be
    /// served without asking again.
    pub fn fresh(&self, query: &str) -> Option<Vec<Track>> {
        let key = normalize(query);
        let tick = self.tick();
        let mut entries = self.entries.write().unwrap();
        let entry = entries.get_mut(&key)?;
        entry.used = tick;
        match entry.fetched {
            Some(fetched) if fetched.elapsed() < FRESH_FOR => Some(entry.tracks.clone()),
            _ => None,
        }
    }

    /// Anything already known about this query: an exact hit however old, or failing
    /// that the results of the longest prefix of it that was searched before.
    pub fn best_effort(&self, query: &str) -> Vec<Track> {
        let key = normalize(query);
        if key.is_empty() {
            return vec![];
        }
        let entries = self.entries.read().unwrap();
        if let Some(entry) = entries.get(&key) {
            return entry.tracks.clone();
        }
        entries
            .values()
            .filter(|entry| key.starts_with(&entry.query))
            .max_by_key(|entry| entry.query.len())
            .map(|entry| entry.tracks.clone())
            .unwrap_or_default()
    }

    pub fn store(&self, query: &str, tracks: Vec<Track>) {
        let key = normalize(query);
        if key.is_empty() {
            return;
        }
        let tick = self.tick();
        let mut entries = self.entries.write().unwrap();
        entries.insert(
            key.clone(),
            Entry {
                query: key,
                tracks,
                used: tick,
                fetched: Some(Instant::now()),
            },
        );
        trim(&mut entries);
    }

    fn tick(&self) -> u64 {
        let mut clock = self.clock.write().unwrap();
        *clock += 1;
        *clock
    }

    #[cfg(not(test))]
    fn load(&self) {
        let path = config::cache_path(CACHE_FILE);
        let Ok(contents) = std::fs::read_to_string(&path) else {
            return;
        };
        match serde_json::from_str::<StoredOwned>(&contents) {
            Ok(stored) if stored.version == CACHE_VERSION => {
                debug!("loaded {} cached searches", stored.entries.len());
                let mut entries = self.entries.write().unwrap();
                *self.clock.write().unwrap() = stored.entries.len() as u64;
                for entry in stored.entries {
                    entries.insert(entry.query.clone(), entry);
                }
            }
            Ok(_) => debug!("search cache version changed, ignoring it"),
            Err(e) => error!("can't parse search cache: {e}"),
        }
    }

    /// Write the cache out. Called off the UI thread, after a search lands.
    pub fn save(&self) {
        let entries = self.entries.read().unwrap();
        let stored = Stored {
            version: CACHE_VERSION,
            entries: entries.values().collect(),
        };
        let path = config::cache_path(CACHE_FILE);
        match std::fs::File::create(&path) {
            Ok(file) => {
                if let Err(e) = serde_json::to_writer(file, &stored) {
                    error!("could not write search cache: {e}");
                }
            }
            Err(e) => error!("could not open search cache: {e}"),
        }
    }
}

/// Drop the least recently used queries once the cache outgrows its budget.
fn trim(entries: &mut HashMap<String, Entry>) {
    while entries.len() > MAX_QUERIES {
        let Some(oldest) = entries
            .values()
            .min_by_key(|entry| entry.used)
            .map(|entry| entry.query.clone())
        else {
            return;
        };
        entries.remove(&oldest);
    }
}

#[derive(Serialize)]
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
    use super::{MAX_QUERIES, SearchCache};
    use crate::model::track::Track;

    fn track(title: &str) -> Track {
        Track {
            id: Some(title.to_string()),
            uri: format!("spotify:track:{title}"),
            title: title.to_string(),
            track_number: 1,
            disc_number: 1,
            duration: 1000,
            artists: vec![],
            artist_ids: vec![],
            album: None,
            album_id: None,
            album_artists: vec![],
            cover_url: None,
            url: String::new(),
            added_at: None,
            list_index: 0,
            is_local: false,
            is_playable: Some(true),
        }
    }

    #[test]
    fn a_stored_query_comes_back_for_any_casing() {
        let cache = SearchCache::default();
        cache.store("Juno Reactor", vec![track("Solaris")]);
        assert_eq!(cache.fresh("  juno reactor ").unwrap().len(), 1);
    }

    #[test]
    fn a_longer_query_falls_back_to_its_longest_known_prefix() {
        let cache = SearchCache::default();
        cache.store("ju", vec![track("short")]);
        cache.store("juno", vec![track("long")]);

        let found = cache.best_effort("juno rea");
        assert_eq!(found.first().map(|t| t.title.clone()), Some("long".into()));
        // An unrelated query shares no prefix, so nothing stale is shown for it.
        assert!(cache.best_effort("orbital").is_empty());
    }

    #[test]
    fn the_cache_stays_within_its_budget() {
        let cache = SearchCache::default();
        for index in 0..MAX_QUERIES + 10 {
            cache.store(&format!("query {index}"), vec![track("result")]);
        }
        assert_eq!(cache.entries.read().unwrap().len(), MAX_QUERIES);
        // The oldest queries are the ones that went.
        assert!(cache.fresh("query 0").is_none());
        assert!(cache.fresh(&format!("query {}", MAX_QUERIES + 9)).is_some());
    }
}
