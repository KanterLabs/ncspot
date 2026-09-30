//! Background catalogue enrichment for radio.
//!
//! Radio is deliberately independent from this store.  A refresh only adds
//! useful catalogue data to the cache; it never changes the queue or waits on
//! a radio request.  The on-disk representation is owned by this module so a
//! schema change here cannot invalidate any of ncspot's other caches.

use std::collections::{HashMap, HashSet};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock, RwLock};
#[cfg(not(test))]
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use log::{error, warn};
#[cfg(not(test))]
use rspotify::model::{SearchResult, SearchType};

#[cfg(not(test))]
use crate::config;
use crate::events::EventManager;
use crate::model::track::Track;
use crate::spotify::Spotify;

#[cfg(not(test))]
const CACHE_FILE: &str = "radio-catalog.json";
const CACHE_VERSION: u16 = 1;
const MAX_TRACKS: usize = 5_000;
#[cfg(not(test))]
const MAX_REQUESTS_PER_REFRESH: usize = 6;
const MAX_RELATED_ARTISTS: usize = 2;
#[cfg(not(test))]
const MAX_SEARCH_RESULTS: u32 = 10;
const MAX_QUERY_CHARS: usize = 200;
const ARTIST_TTL: Duration = Duration::from_secs(24 * 60 * 60);
const FAILURE_BACKOFF: Duration = Duration::from_secs(10 * 60);

/// A persisted cache entry.  Timestamps are kept as unix seconds so the file
/// stays simple, portable, and independent of chrono's wire representation.
#[derive(Clone, Debug, Deserialize, Serialize)]
struct StoredCatalog {
    version: u16,
    #[serde(default)]
    tracks: Vec<Track>,
    #[serde(default)]
    artist_genres: HashMap<String, Vec<String>>,
    #[serde(default)]
    artist_refreshed_at: HashMap<String, i64>,
    #[serde(default)]
    artist_failed_at: HashMap<String, i64>,
}

#[derive(Debug)]
struct CatalogState {
    tracks: Vec<Track>,
    artist_genres: HashMap<String, Vec<String>>,
    artist_refreshed_at: HashMap<String, i64>,
    artist_failed_at: HashMap<String, i64>,
    /// A malformed or unknown-version file must be left alone.  Once this is
    /// set, this process will not overwrite it with a newer schema.
    persistence_disabled: Option<&'static str>,
    refreshing: bool,
    last_refresh: Option<RefreshSummary>,
}

#[derive(Clone, Debug, Default)]
struct RefreshSummary {
    at: i64,
    requests: usize,
    artist_requests: usize,
    search_requests: usize,
    tracks_added: usize,
    tracks_updated: usize,
    failed: usize,
    rate_limited: usize,
    saved: bool,
}

#[cfg(not(test))]
#[derive(Clone, Debug)]
struct RefreshResult {
    tracks: Vec<Track>,
    artist_genres: Vec<(String, Vec<String>)>,
    refreshed_artists: Vec<String>,
    failed_artists: Vec<String>,
    summary: RefreshSummary,
}

/// Shared, bounded radio catalogue.
pub struct Enrichment {
    state: RwLock<CatalogState>,
    refresh_lock: Mutex<()>,
    pending_callbacks: Mutex<Vec<Box<dyn FnOnce() + Send + 'static>>>,
    cache_path: PathBuf,
}

static SHARED: OnceLock<Arc<Enrichment>> = OnceLock::new();

/// Return the process-wide radio catalogue.
pub fn shared() -> Arc<Enrichment> {
    SHARED.get_or_init(|| Arc::new(Enrichment::new())).clone()
}

impl Enrichment {
    /// Open the radio catalogue in ncspot's cache directory.
    pub fn new() -> Self {
        #[cfg(test)]
        {
            Self::memory_only()
        }
        #[cfg(not(test))]
        {
            let path = config::cache_path(CACHE_FILE);
            Self::from_path(path)
        }
    }

    #[cfg(test)]
    fn memory_only() -> Self {
        Self {
            state: RwLock::new(CatalogState {
                tracks: Vec::new(),
                artist_genres: HashMap::new(),
                artist_refreshed_at: HashMap::new(),
                artist_failed_at: HashMap::new(),
                persistence_disabled: Some("memory-only test cache"),
                refreshing: false,
                last_refresh: None,
            }),
            refresh_lock: Mutex::new(()),
            pending_callbacks: Mutex::new(Vec::new()),
            cache_path: PathBuf::new(),
        }
    }

    fn from_path(cache_path: PathBuf) -> Self {
        let (tracks, artist_genres, artist_refreshed_at, artist_failed_at, disabled) =
            load_catalog(&cache_path);
        Self {
            state: RwLock::new(CatalogState {
                tracks,
                artist_genres,
                artist_refreshed_at,
                artist_failed_at,
                persistence_disabled: disabled,
                refreshing: false,
                last_refresh: None,
            }),
            refresh_lock: Mutex::new(()),
            pending_callbacks: Mutex::new(Vec::new()),
            cache_path,
        }
    }

    /// Return cached tracks and artist genres without holding the store lock
    /// after this call returns.
    pub fn snapshot(&self) -> (Vec<Track>, HashMap<String, Vec<String>>) {
        let state = self.state.read().unwrap();
        (state.tracks.clone(), state.artist_genres.clone())
    }

    /// Return a short diagnostic string suitable for a status bar or log.
    /// It contains counts and state only; it never includes API queries or
    /// credentials.
    pub fn status(&self) -> String {
        let state = self.state.read().unwrap();
        let last = state.last_refresh.as_ref();
        let last_text = last.map_or_else(
            || "last_refresh=none".to_string(),
            |summary| {
                format!(
                    "last_refresh_at={} requests={} artists={} searches={} added={} updated={} failed={} rate_limited={} saved={}",
                    summary.at,
                    summary.requests,
                    summary.artist_requests,
                    summary.search_requests,
                    summary.tracks_added,
                    summary.tracks_updated,
                    summary.failed,
                    summary.rate_limited,
                    summary.saved,
                )
            },
        );
        let persistence_text = state.persistence_disabled.map_or_else(
            || "persistence=enabled".to_string(),
            |reason| format!("persistence=disabled({reason})"),
        );
        format!(
            "{}; cached_tracks={} cached_artists={}; {}; {}",
            if state.refreshing {
                "refreshing"
            } else {
                "idle"
            },
            state.tracks.len(),
            state.artist_genres.len(),
            last_text,
            persistence_text,
        )
    }

    /// Schedule a refresh and return immediately.  The worker performs all
    /// API calls in the background, so radio selection and playback never
    /// wait on enrichment. A second refresh while one is running is
    /// discarded, preventing request storms when playback changes quickly.
    pub fn refresh(
        self: &Arc<Self>,
        spotify: Spotify,
        seed: Track,
        related_artists: Vec<(String, String)>,
        events: EventManager,
    ) -> bool {
        self.start_refresh_inner(spotify, seed, related_artists, events, None)
    }

    /// Schedule a refresh and invoke `callback` after the worker has merged
    /// and persisted its result. The callback is useful to a cold radio start
    /// that wants to retry selection when new tracks become available.
    pub fn refresh_with_callback<F>(
        self: &Arc<Self>,
        spotify: Spotify,
        seed: Track,
        related_artists: Vec<(String, String)>,
        events: EventManager,
        callback: F,
    ) -> bool
    where
        F: FnOnce() + Send + 'static,
    {
        self.start_refresh_inner(
            spotify,
            seed,
            related_artists,
            events,
            Some(Box::new(callback)),
        )
    }

    fn start_refresh_inner(
        self: &Arc<Self>,
        spotify: Spotify,
        seed: Track,
        related_artists: Vec<(String, String)>,
        events: EventManager,
        callback: Option<Box<dyn FnOnce() + Send + 'static>>,
    ) -> bool {
        if !self.begin_refresh() {
            let Some(callback) = callback else {
                return false;
            };
            // A worker may finish between the first check and this one. Both
            // paths are serialized by refresh_lock: queue the callback while
            // the worker is still active, otherwise deliver it immediately.
            if let Some(callback) = self.queue_callback_if_running(callback) {
                callback();
            }
            return true;
        }

        if !self.refresh_needed(&seed, &related_artists) {
            let callbacks = self.complete_refresh(RefreshSummary {
                at: unix_now(),
                ..RefreshSummary::default()
            });
            if let Some(callback) = callback {
                callback();
            }
            for callback in callbacks {
                callback();
            }
            return true;
        }

        let this = Arc::clone(self);
        #[cfg(test)]
        {
            // Tests must remain deterministic and must never renew a token or
            // touch the network.  Keep the same merge path the worker uses.
            let _ = spotify;
            let _ = related_artists;
            let _ = events;
            let callbacks = this.finish_test_refresh(seed);
            if let Some(callback) = callback {
                callback();
            }
            for callback in callbacks {
                callback();
            }
            true
        }
        #[cfg(not(test))]
        {
            match thread::Builder::new()
                .name("radio-catalog".to_string())
                .spawn(move || {
                    this.run_refresh(spotify, seed, related_artists, events);
                    if let Some(callback) = callback {
                        callback();
                    }
                }) {
                Ok(_) => true,
                Err(error) => {
                    error!("could not start radio catalogue refresh: {error}");
                    for callback in self.abort_refresh() {
                        callback();
                    }
                    false
                }
            }
        }
    }

    fn begin_refresh(&self) -> bool {
        let _guard = self.refresh_lock.lock().unwrap();
        let mut state = self.state.write().unwrap();
        if state.refreshing {
            return false;
        }
        state.refreshing = true;
        true
    }

    fn queue_callback_if_running(
        &self,
        callback: Box<dyn FnOnce() + Send + 'static>,
    ) -> Option<Box<dyn FnOnce() + Send + 'static>> {
        let _guard = self.refresh_lock.lock().unwrap();
        if self.state.read().unwrap().refreshing {
            self.pending_callbacks.lock().unwrap().push(callback);
            None
        } else {
            Some(callback)
        }
    }

    #[cfg(not(test))]
    fn abort_refresh(&self) -> Vec<Box<dyn FnOnce() + Send + 'static>> {
        let _guard = self.refresh_lock.lock().unwrap();
        self.state.write().unwrap().refreshing = false;
        std::mem::take(&mut *self.pending_callbacks.lock().unwrap())
    }

    fn refresh_needed(&self, seed: &Track, related_artists: &[(String, String)]) -> bool {
        let has_cached_tracks = !self.state.read().unwrap().tracks.is_empty();
        if !has_cached_tracks {
            return true;
        }
        let candidates = artist_candidates(seed, related_artists);
        if candidates.is_empty() {
            return true;
        }
        // A name without a validated id cannot have a per-artist metadata
        // timestamp, so keep its search eligible for enrichment.
        if related_artists
            .iter()
            .take(MAX_RELATED_ARTISTS)
            .any(|(id, name)| !name.trim().is_empty() && !valid_spotify_id(id))
        {
            return true;
        }
        let now = unix_now();
        candidates
            .iter()
            .any(|candidate| self.artist_needs_refresh(&candidate.id, now))
    }

    #[cfg(test)]
    fn finish_test_refresh(&self, seed: Track) -> Vec<Box<dyn FnOnce() + Send + 'static>> {
        let mut summary = RefreshSummary {
            at: unix_now(),
            ..RefreshSummary::default()
        };
        let (added, updated) = self.merge_tracks(&[seed]);
        summary.tracks_added = added;
        summary.tracks_updated = updated;
        self.complete_refresh(summary)
    }

    #[cfg(not(test))]
    fn run_refresh(
        &self,
        spotify: Spotify,
        seed: Track,
        related_artists: Vec<(String, String)>,
        events: EventManager,
    ) {
        let result = self.fetch_refresh(&spotify, &seed, &related_artists);
        let (added, updated) = self.merge_tracks(&result.tracks);
        let mut state = self.state.write().unwrap();
        for (artist_id, genres) in result.artist_genres {
            if !genres.is_empty() {
                merge_genre_entry(&mut state.artist_genres, artist_id, genres);
            }
        }
        let now = unix_now();
        for artist_id in result.refreshed_artists {
            state.artist_refreshed_at.insert(artist_id.clone(), now);
            // A later success means the failure backoff is no longer relevant.
            state.artist_failed_at.remove(&artist_id);
        }
        for artist_id in result.failed_artists {
            state.artist_failed_at.insert(artist_id, now);
        }
        let mut summary = result.summary;
        summary.tracks_added = added;
        summary.tracks_updated = updated;
        drop(state);

        let callbacks = self.complete_refresh(summary);
        events.try_trigger();
        for callback in callbacks {
            callback();
        }
    }

    fn complete_refresh(
        &self,
        mut summary: RefreshSummary,
    ) -> Vec<Box<dyn FnOnce() + Send + 'static>> {
        summary.saved = self.save();
        let _guard = self.refresh_lock.lock().unwrap();
        let mut state = self.state.write().unwrap();
        state.refreshing = false;
        state.last_refresh = Some(summary);
        std::mem::take(&mut *self.pending_callbacks.lock().unwrap())
    }

    #[cfg(not(test))]
    fn fetch_refresh(
        &self,
        spotify: &Spotify,
        seed: &Track,
        related_artists: &[(String, String)],
    ) -> RefreshResult {
        let now = unix_now();
        let mut budget = RequestBudget::new(MAX_REQUESTS_PER_REFRESH);
        let mut summary = RefreshSummary {
            at: now,
            ..RefreshSummary::default()
        };
        let candidates = artist_candidates(seed, related_artists);
        let mut genres_for_seed = self.seed_genres(seed);
        let mut artist_genres = Vec::new();
        let mut refreshed_artists = Vec::new();
        let mut failed_artists = Vec::new();

        // Artist metadata is the only source of genre tags.  Query at most
        // the seed and two related artists, and skip entries inside their TTL
        // or failure backoff.
        for candidate in candidates.iter().take(1 + MAX_RELATED_ARTISTS) {
            if !valid_spotify_id(&candidate.id) || !self.artist_needs_refresh(&candidate.id, now) {
                continue;
            }
            if spotify.api.rate_limit_wait().is_some() {
                summary.rate_limited += 1;
                break;
            }
            if !budget.take() {
                break;
            }
            summary.requests += 1;
            summary.artist_requests += 1;
            #[allow(deprecated)]
            let result = spotify.api.artist(&candidate.id);
            match result {
                Ok(artist) => {
                    #[allow(deprecated)]
                    let genres = artist.genres;
                    if candidate.is_seed {
                        genres_for_seed = genres.clone();
                    }
                    artist_genres.push((candidate.id.clone(), genres));
                    refreshed_artists.push(candidate.id.clone());
                }
                Err(()) => {
                    failed_artists.push(candidate.id.clone());
                    summary.failed += 1;
                }
            }
        }

        // Search artist names inferred from playlists.  Keep one request in
        // reserve for a genre search when a seed tag is available.
        let names = search_names(seed, related_artists);
        let reserve_genre = !genres_for_seed.is_empty();
        let mut tracks = Vec::new();
        for name in names {
            if reserve_genre && budget.remaining() <= 1 {
                break;
            }
            if spotify.api.rate_limit_wait().is_some() {
                summary.rate_limited += 1;
                break;
            }
            if !budget.take() {
                break;
            }
            summary.requests += 1;
            summary.search_requests += 1;
            let query = artist_search_query(&name);
            match spotify
                .api
                .search(SearchType::Track, &query, MAX_SEARCH_RESULTS, 0)
            {
                Ok(SearchResult::Tracks(page)) => {
                    tracks.extend(page.items.iter().map(Track::from));
                }
                Ok(_) => {}
                Err(()) => summary.failed += 1,
            }
        }

        if reserve_genre && budget.remaining() > 0 {
            if spotify.api.rate_limit_wait().is_some() {
                summary.rate_limited += 1;
            } else if budget.take() {
                summary.requests += 1;
                summary.search_requests += 1;
                // One genre is enough to bring unfamiliar artists into the
                // catalogue while keeping the refresh bounded.
                let genre = &genres_for_seed[0];
                let query = genre_search_query(genre);
                match spotify
                    .api
                    .search(SearchType::Track, &query, MAX_SEARCH_RESULTS, 0)
                {
                    Ok(SearchResult::Tracks(page)) => {
                        for item in &page.items {
                            let track = Track::from(item);
                            add_track_genre_evidence(&mut artist_genres, &track, genre);
                            tracks.push(track);
                        }
                    }
                    Ok(_) => {}
                    Err(()) => summary.failed += 1,
                }
            }
        }

        RefreshResult {
            tracks,
            artist_genres,
            refreshed_artists,
            failed_artists,
            summary,
        }
    }

    #[cfg(not(test))]
    fn seed_genres(&self, seed: &Track) -> Vec<String> {
        let state = self.state.read().unwrap();
        seed.artist_ids
            .iter()
            .find_map(|artist_id| state.artist_genres.get(artist_id).cloned())
            .unwrap_or_default()
    }

    fn artist_needs_refresh(&self, artist_id: &str, now: i64) -> bool {
        let state = self.state.read().unwrap();
        if state
            .artist_refreshed_at
            .get(artist_id)
            .is_some_and(|at| elapsed_since(*at, now) < ARTIST_TTL)
        {
            return false;
        }
        !state
            .artist_failed_at
            .get(artist_id)
            .is_some_and(|at| elapsed_since(*at, now) < FAILURE_BACKOFF)
    }

    fn merge_tracks(&self, incoming: &[Track]) -> (usize, usize) {
        let mut state = self.state.write().unwrap();
        let mut added = 0;
        let mut updated = 0;
        for track in incoming {
            let key = track_key(track);
            if let Some(index) = state.tracks.iter().position(|old| track_key(old) == key) {
                if should_replace(&state.tracks[index], track) {
                    state.tracks[index] = track.clone();
                    updated += 1;
                }
            } else {
                state.tracks.push(track.clone());
                added += 1;
            }
        }
        while state.tracks.len() > MAX_TRACKS {
            state.tracks.remove(0);
        }
        let CatalogState {
            tracks,
            artist_genres,
            ..
        } = &mut *state;
        prune_track_genres(artist_genres, tracks);
        (added, updated)
    }

    fn save(&self) -> bool {
        let state = self.state.read().unwrap();
        if state.persistence_disabled.is_some() {
            return false;
        }
        let stored = StoredCatalog {
            version: CACHE_VERSION,
            tracks: state.tracks.clone(),
            artist_genres: state.artist_genres.clone(),
            artist_refreshed_at: state.artist_refreshed_at.clone(),
            artist_failed_at: state.artist_failed_at.clone(),
        };
        drop(state);
        atomic_save(&self.cache_path, &stored)
    }
}

#[derive(Clone, Debug)]
struct ArtistCandidate {
    id: String,
    #[cfg(not(test))]
    is_seed: bool,
}

fn artist_candidates(seed: &Track, related: &[(String, String)]) -> Vec<ArtistCandidate> {
    let mut seen = HashSet::new();
    let mut candidates = Vec::new();
    if let Some(id) = seed.artist_ids.iter().find(|id| valid_spotify_id(id)) {
        seen.insert(id.clone());
        candidates.push(ArtistCandidate {
            id: id.clone(),
            #[cfg(not(test))]
            is_seed: true,
        });
    }
    for (id, _) in related
        .iter()
        .filter(|(id, _)| valid_spotify_id(id))
        .take(MAX_RELATED_ARTISTS)
    {
        if valid_spotify_id(id) && seen.insert(id.clone()) {
            candidates.push(ArtistCandidate {
                id: id.clone(),
                #[cfg(not(test))]
                is_seed: false,
            });
        }
    }
    candidates
}

#[cfg(not(test))]
fn search_names(seed: &Track, related: &[(String, String)]) -> Vec<String> {
    let mut names = Vec::new();
    let mut seen = HashSet::new();
    for name in seed.artists.iter().take(1) {
        add_name(name, &mut seen, &mut names);
    }
    for (_, name) in related.iter().take(MAX_RELATED_ARTISTS) {
        add_name(name, &mut seen, &mut names);
    }
    names
}

#[cfg(not(test))]
fn add_name(name: &str, seen: &mut HashSet<String>, names: &mut Vec<String>) {
    let name = name.trim();
    if name.is_empty() {
        return;
    }
    let key = name.to_lowercase();
    if seen.insert(key) {
        names.push(name.to_string());
    }
}

/// Spotify ids are 22-character base62 values.  Checking the shape here is
/// essential because some fixture tracks and local files carry arbitrary ids,
/// while lower layers historically used `unwrap()` around `from_id`.
fn valid_spotify_id(id: &str) -> bool {
    id.len() == 22 && id.bytes().all(|byte| byte.is_ascii_alphanumeric())
}

fn truncate_query(value: &str) -> String {
    value.chars().take(MAX_QUERY_CHARS).collect()
}

fn escape_query_component(value: &str) -> String {
    truncate_query(value)
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
}

fn artist_search_query(name: &str) -> String {
    format!("artist:\"{}\"", escape_query_component(name))
}

fn genre_search_query(genre: &str) -> String {
    format!("genre:\"{}\"", escape_query_component(genre))
}

fn track_key(track: &Track) -> String {
    if let Some(id) = track.id.as_deref().filter(|id| !id.is_empty()) {
        return format!("id:{id}");
    }
    if let Some(id) = track.uri.strip_prefix("spotify:track:")
        && !id.is_empty()
    {
        return format!("id:{id}");
    }
    if !track.uri.is_empty() {
        return format!("uri:{}", track.uri);
    }
    format!(
        "fallback:{}\u{1f}{}\u{1f}{}",
        track.title.to_lowercase(),
        track.artists.join("\u{1f}").to_lowercase(),
        track.album.as_deref().unwrap_or_default().to_lowercase(),
    )
}

/// A reserved catalogue key carrying evidence from a `genre:"..."` search.
/// It is deliberately keyed by track URI rather than artist ID: a genre
/// search says that this result matched the query, not that every track by
/// its artist has that genre.
fn track_genre_key(uri: &str) -> String {
    format!("track:{uri}")
}

fn add_track_genre_evidence(
    artist_genres: &mut Vec<(String, Vec<String>)>,
    track: &Track,
    genre: &str,
) {
    if !track.uri.is_empty() {
        artist_genres.push((track_genre_key(&track.uri), vec![genre.to_string()]));
    }
}

fn merge_genre_entry(
    artist_genres: &mut HashMap<String, Vec<String>>,
    key: String,
    genres: Vec<String>,
) {
    if key.starts_with("track:") {
        let entry = artist_genres.entry(key).or_default();
        for genre in genres {
            if !genre.is_empty() && !entry.contains(&genre) {
                entry.push(genre);
            }
        }
    } else {
        artist_genres.insert(key, genres);
    }
}

fn prune_track_genres(artist_genres: &mut HashMap<String, Vec<String>>, tracks: &[Track]) {
    let track_uris: HashSet<&str> = tracks
        .iter()
        .filter_map(|track| (!track.uri.is_empty()).then_some(track.uri.as_str()))
        .collect();
    artist_genres.retain(|key, _| !key.starts_with("track:") || track_uris.contains(&key[6..]));
}

fn richness(track: &Track) -> usize {
    usize::from(track.id.is_some())
        + usize::from(!track.uri.is_empty())
        + usize::from(!track.url.is_empty())
        + usize::from(track.album.is_some())
        + usize::from(track.album_id.is_some())
        + usize::from(!track.album_artists.is_empty())
        + usize::from(track.cover_url.is_some())
        + usize::from(track.duration > 0)
        + usize::from(track.is_playable.is_some())
}

fn should_replace(old: &Track, new: &Track) -> bool {
    let old_score = richness(old);
    let new_score = richness(new);
    new_score > old_score || (new_score == old_score && old.title != new.title)
}

fn elapsed_since(previous: i64, now: i64) -> Duration {
    if now <= previous {
        Duration::ZERO
    } else {
        Duration::from_secs((now - previous) as u64)
    }
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or_default()
}

type LoadedCatalog = (
    Vec<Track>,
    HashMap<String, Vec<String>>,
    HashMap<String, i64>,
    HashMap<String, i64>,
    Option<&'static str>,
);

fn load_catalog(path: &PathBuf) -> LoadedCatalog {
    let contents = match fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(_) => {
            return (
                Vec::new(),
                HashMap::new(),
                HashMap::new(),
                HashMap::new(),
                None,
            );
        }
    };
    match serde_json::from_str::<StoredCatalog>(&contents) {
        Ok(stored) if stored.version == CACHE_VERSION => {
            let tracks = prune_tracks(stored.tracks);
            let mut artist_genres = stored.artist_genres;
            prune_track_genres(&mut artist_genres, &tracks);
            (
                tracks,
                artist_genres,
                stored.artist_refreshed_at,
                stored.artist_failed_at,
                None,
            )
        }
        Ok(_) => {
            warn!("ignoring radio catalogue with an unknown cache version");
            (
                Vec::new(),
                HashMap::new(),
                HashMap::new(),
                HashMap::new(),
                Some("unsupported cache version"),
            )
        }
        Err(error) => {
            warn!("ignoring corrupt radio catalogue: {error}");
            (
                Vec::new(),
                HashMap::new(),
                HashMap::new(),
                HashMap::new(),
                Some("corrupt cache file"),
            )
        }
    }
}

fn prune_tracks(mut tracks: Vec<Track>) -> Vec<Track> {
    if tracks.len() > MAX_TRACKS {
        let remove = tracks.len() - MAX_TRACKS;
        tracks.drain(..remove);
    }
    tracks
}

fn atomic_save(path: &PathBuf, stored: &StoredCatalog) -> bool {
    let bytes = match serde_json::to_vec_pretty(stored) {
        Ok(bytes) => bytes,
        Err(error) => {
            error!("could not serialize radio catalogue: {error}");
            return false;
        }
    };
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    let tmp = path.with_extension(format!("json.tmp-{}-{stamp}", std::process::id()));
    let write_result = (|| {
        let mut file = OpenOptions::new().write(true).create_new(true).open(&tmp)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        fs::rename(&tmp, path)?;
        Ok::<(), std::io::Error>(())
    })();
    if let Err(error) = write_result {
        let _ = fs::remove_file(&tmp);
        error!("could not save radio catalogue: {error}");
        return false;
    }
    true
}

#[cfg(not(test))]
struct RequestBudget {
    remaining: usize,
}

#[cfg(not(test))]
impl RequestBudget {
    fn new(limit: usize) -> Self {
        Self { remaining: limit }
    }

    fn take(&mut self) -> bool {
        if self.remaining == 0 {
            return false;
        }
        self.remaining -= 1;
        true
    }

    fn remaining(&self) -> usize {
        self.remaining
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::events::EventManager;
    use crate::spotify::Spotify;
    use std::path::Path;

    fn track(id: Option<&str>, rich: bool) -> Track {
        Track {
            id: id.map(str::to_string),
            uri: id
                .map(|id| format!("spotify:track:{id}"))
                .unwrap_or_default(),
            title: "Song".to_string(),
            track_number: 1,
            disc_number: 1,
            duration: if rich { 10_000 } else { 0 },
            artists: vec!["Artist".to_string()],
            artist_ids: vec![],
            album: rich.then(|| "Album".to_string()),
            album_id: rich.then(|| "album".to_string()),
            album_artists: if rich {
                vec!["Artist".to_string()]
            } else {
                Vec::new()
            },
            cover_url: rich.then(|| "https://img".to_string()),
            url: if rich {
                "https://track".to_string()
            } else {
                String::new()
            },
            added_at: None,
            list_index: 0,
            is_local: false,
            is_playable: Some(true),
        }
    }

    fn temp_path(name: &str) -> PathBuf {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or_default();
        let unique = format!("ncspot-{name}-{}-{stamp}.json", std::process::id());
        std::env::temp_dir().join(unique)
    }

    fn store(path: &Path) -> Enrichment {
        Enrichment::from_path(path.to_path_buf())
    }

    #[test]
    fn cache_roundtrip_preserves_tracks_genres_and_timestamps() {
        let path = temp_path("roundtrip");
        let enrichment = store(&path);
        {
            let mut state = enrichment.state.write().unwrap();
            state
                .tracks
                .push(track(Some("1234567890123456789012"), true));
            state
                .artist_genres
                .insert("artist".to_string(), vec!["ambient".to_string()]);
            state.artist_refreshed_at.insert("artist".to_string(), 42);
        }
        assert!(enrichment.save());
        let loaded = store(&path);
        let (tracks, genres) = loaded.snapshot();
        assert_eq!(tracks.len(), 1);
        assert_eq!(genres.get("artist"), Some(&vec!["ambient".to_string()]));
        assert_eq!(
            loaded
                .state
                .read()
                .unwrap()
                .artist_refreshed_at
                .get("artist"),
            Some(&42)
        );
        let _ = fs::remove_file(path);
    }

    #[test]
    fn genre_query_evidence_is_track_scoped_and_persists() {
        let path = temp_path("genre-evidence");
        let enrichment = store(&path);
        let result = track(Some("1234567890123456789012"), true);
        enrichment.merge_tracks(std::slice::from_ref(&result));
        let mut evidence = Vec::new();
        add_track_genre_evidence(&mut evidence, &result, "ambient");
        let key = track_genre_key(&result.uri);
        {
            let mut state = enrichment.state.write().unwrap();
            for (key, genres) in evidence {
                merge_genre_entry(&mut state.artist_genres, key, genres);
            }
        }
        assert_eq!(
            enrichment.snapshot().1.get(&key),
            Some(&vec!["ambient".to_string()])
        );
        assert!(enrichment.save());

        let loaded = store(&path);
        assert_eq!(
            loaded.snapshot().1.get(&key),
            Some(&vec!["ambient".to_string()])
        );

        loaded.state.write().unwrap().tracks.clear();
        loaded.merge_tracks(&[]);
        assert!(!loaded.snapshot().1.contains_key(&key));
        let _ = fs::remove_file(path);
    }

    #[test]
    fn corrupt_or_unknown_cache_is_preserved_and_disables_save() {
        let path = temp_path("preserve");
        fs::write(&path, "{ definitely not json").unwrap();
        let enrichment = store(&path);
        let before = fs::read(&path).unwrap();
        assert!(
            enrichment
                .status()
                .contains("persistence=disabled(corrupt cache file)")
        );
        enrichment.merge_tracks(&[track(Some("1234567890123456789012"), true)]);
        assert!(!enrichment.save());
        assert_eq!(fs::read(&path).unwrap(), before);

        fs::write(&path, r#"{"version":999,"tracks":[]}"#).unwrap();
        let enrichment = store(&path);
        let before = fs::read(&path).unwrap();
        assert!(
            enrichment
                .status()
                .contains("persistence=disabled(unsupported cache version)")
        );
        enrichment.merge_tracks(&[track(Some("1234567890123456789012"), true)]);
        assert!(!enrichment.save());
        assert_eq!(fs::read(&path).unwrap(), before);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn merge_prefers_richer_duplicate_and_prunes_oldest_tracks() {
        let path = temp_path("merge");
        let enrichment = store(&path);
        let id = "1234567890123456789012";
        assert_eq!(enrichment.merge_tracks(&[track(Some(id), false)]), (1, 0));
        assert_eq!(enrichment.merge_tracks(&[track(Some(id), true)]), (0, 1));
        let (tracks, _) = enrichment.snapshot();
        assert_eq!(tracks[0].album.as_deref(), Some("Album"));

        let many = (0..(MAX_TRACKS + 1))
            .map(|number| {
                let id = format!("{number:022}");
                track(Some(&id), false)
            })
            .collect::<Vec<_>>();
        enrichment.merge_tracks(&many);
        let (tracks, _) = enrichment.snapshot();
        assert_eq!(tracks.len(), MAX_TRACKS);
        assert!(!tracks.iter().any(|item| item.id.as_deref() == Some(id)));
        let newest = format!("{:022}", 5_000);
        assert_eq!(
            tracks.last().and_then(|item| item.id.as_deref()),
            Some(newest.as_str())
        );
        let _ = fs::remove_file(path);
    }

    #[test]
    fn search_queries_escape_quotes_backslashes_and_limit_input() {
        let name = format!("a\\b\"{}", "x".repeat(MAX_QUERY_CHARS));
        let query = artist_search_query(&name);
        assert!(query.starts_with("artist:\"a\\\\b\\\""));
        assert!(query.len() <= MAX_QUERY_CHARS + 16);
        assert_eq!(genre_search_query("dream pop"), "genre:\"dream pop\"");
    }

    #[test]
    fn candidates_skip_invalid_ids_and_bound_related_artists() {
        let seed = track(None, false);
        let related = vec![
            ("bad".to_string(), "one".to_string()),
            ("1234567890123456789012".to_string(), "two".to_string()),
            ("2345678901234567890123".to_string(), "three".to_string()),
            ("3456789012345678901234".to_string(), "four".to_string()),
        ];
        let candidates = artist_candidates(&seed, &related);
        assert_eq!(candidates.len(), 2);
        assert!(
            candidates
                .iter()
                .all(|candidate| valid_spotify_id(&candidate.id))
        );
    }

    #[test]
    fn refresh_in_tests_merges_seed_without_network() {
        let path = temp_path("refresh");
        let enrichment = Arc::new(store(&path));
        let cfg = Config::new_for_test();
        let events = EventManager::new_for_test();
        let spotify = Spotify::new_for_test(cfg, events.clone());
        let seed = track(Some("1234567890123456789012"), true);
        assert!(enrichment.refresh(spotify, seed.clone(), Vec::new(), events,));
        assert_eq!(enrichment.snapshot().0.len(), 1);
        assert_eq!(enrichment.snapshot().0[0].id, seed.id);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn shared_is_memory_only_under_tests() {
        assert_eq!(
            shared().state.read().unwrap().persistence_disabled,
            Some("memory-only test cache")
        );
    }

    #[test]
    fn callback_waits_for_an_active_refresh_to_finish() {
        use std::sync::atomic::{AtomicBool, Ordering};

        let path = temp_path("callback");
        let enrichment = Arc::new(store(&path));
        enrichment.state.write().unwrap().refreshing = true;
        let fired = Arc::new(AtomicBool::new(false));
        let callback_fired = Arc::clone(&fired);
        let cfg = Config::new_for_test();
        let events = EventManager::new_for_test();
        let spotify = Spotify::new_for_test(cfg, events.clone());
        assert!(enrichment.refresh_with_callback(
            spotify,
            track(Some("1234567890123456789012"), true),
            Vec::new(),
            events,
            move || callback_fired.store(true, Ordering::SeqCst),
        ));
        assert!(!fired.load(Ordering::SeqCst));
        let callbacks = enrichment.complete_refresh(RefreshSummary {
            at: unix_now(),
            ..RefreshSummary::default()
        });
        for callback in callbacks {
            callback();
        }
        assert!(fired.load(Ordering::SeqCst));
        let _ = fs::remove_file(path);
    }
}
