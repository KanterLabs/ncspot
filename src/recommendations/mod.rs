//! Cached, explainable local recommendations. Only enrichment may access the
//! network; ranking and replay have no Spotify dependency.

pub mod engine;
pub mod enrichment;
pub mod history;
pub mod tracker;

use std::collections::{HashMap, HashSet, VecDeque};
use std::hash::{Hash, Hasher};
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::config;
use crate::events::EventManager;
use crate::library::Library;
use crate::model::{
    album::Album, artist::Artist, playable::Playable, playlist::Playlist, track::Track,
};
use crate::queue::Queue;
use crate::traits::ListItem;
use engine::{Catalog, Context, Report};

const SHORTLIST_SIZE: usize = 128;
const SHORTLISTS: usize = 32;
const SHORTLIST_TTL: Duration = Duration::from_secs(600);

struct Cached {
    seed: String,
    fingerprint: u64,
    at: Instant,
    tracks: Vec<Track>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Replay {
    version: u32,
    catalog: Catalog,
    context: Context,
}

#[derive(Clone, Serialize)]
pub struct Diagnostic {
    pub algorithm: &'static str,
    pub source_count: usize,
    pub shortlist_hit: bool,
    pub total_us: u128,
    pub applied: bool,
    pub history_status: String,
    pub enrichment_status: String,
    pub report: Report,
}

#[derive(Default)]
struct Service {
    shortlists: Mutex<VecDeque<Cached>>,
    last: Mutex<Option<Diagnostic>>,
    #[cfg(not(test))]
    persist: Mutex<()>,
}

fn service() -> &'static Service {
    static SERVICE: OnceLock<Service> = OnceLock::new();
    SERVICE.get_or_init(Service::default)
}

/// Snapshot only already loaded metadata. Lazy model loaders are deliberately
/// absent here: they can make requests and would stall the selection path.
pub fn catalog(queue: &Queue, library: &Library) -> Catalog {
    let saved_tracks = library.tracks.read().unwrap().clone();
    let saved = saved_tracks.iter().map(|track| track.uri.clone()).collect();
    let mut tracks = saved_tracks;
    let playlists = library.playlists.read().unwrap().clone();
    let mut memberships = Vec::new();
    for playlist in playlists {
        if let Some(items) = playlist.tracks {
            memberships.push(
                items
                    .iter()
                    .filter_map(|item| item.track())
                    .map(|track| track.uri)
                    .collect(),
            );
            tracks.extend(items.into_iter().filter_map(|item| item.track()));
        }
    }
    for album in library.albums.read().unwrap().iter() {
        if let Some(items) = &album.tracks {
            tracks.extend(items.clone());
        }
    }
    for artist in library.artists.read().unwrap().iter() {
        if let Some(items) = &artist.tracks {
            tracks.extend(items.clone());
        }
    }
    tracks.extend(
        queue
            .queue
            .read()
            .unwrap()
            .iter()
            .filter_map(|item| item.track()),
    );
    tracks.extend(crate::search_cache::shared().snapshot());
    let (discovered, artist_genres) = enrichment::shared().snapshot();
    tracks.extend(discovered);
    Catalog {
        tracks,
        playlists: memberships,
        saved,
        artist_genres,
    }
}

fn fingerprint(catalog: &Catalog) -> u64 {
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    // Canonical ordering prevents cache churn when a refreshed library is sorted.
    let mut tracks: Vec<_> = catalog.tracks.iter().collect();
    tracks.sort_by(|a, b| a.uri.cmp(&b.uri).then_with(|| a.title.cmp(&b.title)));
    for track in tracks {
        track.uri.hash(&mut hash);
        track.artist_ids.hash(&mut hash);
        track.artists.hash(&mut hash);
        track.album_id.hash(&mut hash);
        track.album.hash(&mut hash);
        track.duration.hash(&mut hash);
        track.is_local.hash(&mut hash);
        track.is_playable.hash(&mut hash);
    }
    let mut lists = catalog.playlists.clone();
    for list in &mut lists {
        list.sort();
        list.dedup();
    }
    lists.sort();
    lists.hash(&mut hash);
    let mut genres: Vec<_> = catalog.artist_genres.iter().collect();
    genres.sort_by(|a, b| a.0.cmp(b.0));
    for (artist, tags) in genres {
        artist.hash(&mut hash);
        tags.hash(&mut hash);
    }
    let mut saved: Vec<_> = catalog.saved.iter().collect();
    saved.sort();
    saved.hash(&mut hash);
    hash.finish()
}

fn shortlist(catalog: &Catalog, seed: &Track) -> (Catalog, bool) {
    let key = fingerprint(catalog);
    {
        let mut cache = service().shortlists.lock().unwrap();
        if let Some(index) = cache.iter().position(|entry| {
            entry.seed == seed.uri && entry.fingerprint == key && entry.at.elapsed() < SHORTLIST_TTL
        }) {
            let entry = cache.remove(index).unwrap();
            let tracks = entry.tracks.clone();
            cache.push_back(entry);
            return (
                Catalog {
                    tracks,
                    playlists: catalog.playlists.clone(),
                    saved: catalog.saved.clone(),
                    artist_genres: catalog.artist_genres.clone(),
                },
                true,
            );
        }
    }
    let context = Context {
        seed: seed.clone(),
        queued: HashSet::new(),
        recent: Vec::new(),
        feedback: HashMap::new(),
        rng_seed: 42,
        limit: SHORTLIST_SIZE,
    };
    let base = engine::rank(catalog, &context);
    let tracks: Vec<_> = base
        .selected
        .into_iter()
        .map(|selection| selection.track)
        .chain(std::iter::once(seed.clone()))
        .collect();
    let mut cache = service().shortlists.lock().unwrap();
    cache.retain(|entry| entry.seed != seed.uri);
    cache.push_back(Cached {
        seed: seed.uri.clone(),
        fingerprint: key,
        at: Instant::now(),
        tracks: tracks.clone(),
    });
    while cache.len() > SHORTLISTS {
        cache.pop_front();
    }
    (
        Catalog {
            tracks,
            playlists: catalog.playlists.clone(),
            saved: catalog.saved.clone(),
            artist_genres: catalog.artist_genres.clone(),
        },
        false,
    )
}

/// Browse local recommendations without excluding songs already in the queue.
pub fn preview(queue: &Queue, library: &Library, seed: Track) -> Vec<Track> {
    let history = history::shared();
    let context = Context {
        seed,
        queued: HashSet::new(),
        recent: history.recent(),
        feedback: history.snapshot(),
        rng_seed: rand::random(),
        limit: 50,
    };
    engine::rank(&catalog(queue, library), &context)
        .selected
        .into_iter()
        .map(|selection| selection.track)
        .collect()
}

pub fn recommend(
    queue: &Queue,
    library: &Library,
    seed: Track,
    rng_seed: u64,
    limit: usize,
) -> (Diagnostic, Replay) {
    let started = Instant::now();
    let catalog = catalog(queue, library);
    let source_count = catalog
        .tracks
        .iter()
        .map(|track| &track.uri)
        .collect::<HashSet<_>>()
        .len();
    let history = history::shared();
    let queued = queue
        .queue
        .read()
        .unwrap()
        .iter()
        .map(Playable::uri)
        .collect();
    let context = Context {
        seed: seed.clone(),
        queued,
        recent: history.recent(),
        feedback: history.snapshot(),
        rng_seed,
        limit,
    };
    let (short, shortlist_hit) = shortlist(&catalog, &seed);
    let mut replay = Replay {
        version: 1,
        catalog: short,
        context,
    };
    let mut report = engine::rank(&replay.catalog, &replay.context);
    // A long queue can consume a shortlist; regenerate from the full pool rather
    // than pretending the catalog is empty or calling a remote recommender.
    if report.selected.len() < limit && replay.catalog.tracks.len() < catalog.tracks.len() {
        let full = engine::rank(&catalog, &replay.context);
        if full.selected.len() > report.selected.len() {
            replay.catalog = catalog;
            report = full;
        }
    }
    let diagnostic = Diagnostic {
        algorithm: "local-radio-v1",
        source_count,
        shortlist_hit,
        total_us: started.elapsed().as_micros(),
        applied: false,
        history_status: history.status(),
        enrichment_status: enrichment::shared().status(),
        report,
    };
    (diagnostic, replay)
}

pub fn remember(diagnostic: Diagnostic, replay: Replay) {
    log::debug!(
        "radio: seed={} rng={} shortlist_hit={} sources={} selected={} elapsed_us={} applied={}",
        diagnostic.report.seed_uri,
        diagnostic.report.rng_seed,
        diagnostic.shortlist_hit,
        diagnostic.source_count,
        diagnostic.report.selected.len(),
        diagnostic.total_us,
        diagnostic.applied
    );
    for selection in &diagnostic.report.selected {
        log::debug!(
            "radio: pick {} score={:.3} reasons={:?} components={:?}",
            selection.track.uri,
            selection.score,
            selection.reasons,
            selection.components
        );
    }
    *service().last.lock().unwrap() = Some(diagnostic.clone());
    #[cfg(not(test))]
    {
        // Called on the radio worker. Serialize both outputs under one lock so
        // overlapping runs cannot interleave or share temporary filenames.
        let _guard = service().persist.lock().unwrap();
        for (name, json) in [
            ("radio-debug.json", serde_json::to_vec_pretty(&diagnostic)),
            ("radio-replay.json", serde_json::to_vec(&replay)),
        ] {
            if let Ok(bytes) = json {
                let path = config::cache_path(name);
                let temp = path.with_extension("json.tmp");
                if let Err(error) =
                    std::fs::write(&temp, bytes).and_then(|_| std::fs::rename(&temp, &path))
                {
                    log::warn!("radio: unable to write {name}: {error}");
                }
            }
        }
    }
    #[cfg(test)]
    let _ = replay;
}

pub fn diagnostics() -> String {
    let last = service().last.lock().unwrap().clone();
    let report = last
        .and_then(|report| serde_json::to_string_pretty(&report).ok())
        .unwrap_or_else(|| {
            "No radio run yet. Start Radio to capture scores and exclusions.".into()
        });
    format!(
        "{report}\n\nHistory: {}\nEnrichment: {}\n\nReport: {}\nReplay: {}\nOffline replay: {} radio-debug --replay <replay path>\n",
        history::shared().status(),
        enrichment::shared().status(),
        config::cache_path("radio-debug.json").display(),
        config::cache_path("radio-replay.json").display(),
        ncspot::BIN_NAME
    )
}

/// Prepare the seed's candidate cache and catalog enrichment off the UI thread.
pub fn prewarm(queue: Arc<Queue>, library: Arc<Library>, events: EventManager) {
    let Some(Playable::Track(seed)) = queue.get_current() else {
        return;
    };
    if seed.is_local || seed.id.is_none() {
        return;
    }
    #[cfg(not(test))]
    std::thread::spawn(move || {
        let catalog = catalog(&queue, &library);
        let _ = shortlist(&catalog, &seed);
        let related = related_artists(&catalog, &seed);
        enrichment::shared().refresh(queue.get_spotify(), seed, related, events);
    });
    #[cfg(test)]
    let _ = (library, events);
}

pub(crate) fn related_artists(catalog: &Catalog, seed: &Track) -> Vec<(String, String)> {
    let index: HashMap<_, _> = catalog
        .tracks
        .iter()
        .map(|track| (track.uri.as_str(), track))
        .collect();
    let mut affinity: HashMap<(String, String), f64> = HashMap::new();
    for list in &catalog.playlists {
        if !list.iter().any(|uri| {
            uri == &seed.uri
                || index.get(uri.as_str()).is_some_and(|track| {
                    track
                        .artist_ids
                        .iter()
                        .any(|artist| seed.artist_ids.contains(artist))
                })
        }) {
            continue;
        }
        let weight = 1.0 / (list.len().max(2) as f64).sqrt();
        let mut seen = HashSet::new();
        for uri in list {
            if let Some(track) = index.get(uri.as_str()) {
                for (id, name) in track.artist_ids.iter().zip(&track.artists) {
                    if !seed.artist_ids.contains(id) && seen.insert(id) {
                        *affinity.entry((id.clone(), name.clone())).or_default() += weight;
                    }
                }
            }
        }
    }
    let mut artists: Vec<_> = affinity.into_iter().collect();
    artists.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    artists
        .into_iter()
        .take(2)
        .map(|(artist, _)| artist)
        .collect()
}

fn cached<T: serde::de::DeserializeOwned>(name: &str) -> Result<Option<T>, String> {
    let path = config::cache_path(name);
    match std::fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|error| format!("Can't read {}: {error}", path.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!("Can't read {}: {error}", path.display())),
    }
}

/// A read-only offline snapshot. No Config creation, login, or lazy API loading.
pub fn offline_report(seed: Option<String>, rng_seed: u64, limit: usize) -> Result<String, String> {
    let tracks = cached::<Vec<Track>>("tracks.db")?.unwrap_or_default();
    let saved = tracks.iter().map(|track| track.uri.clone()).collect();
    let mut catalog = Catalog {
        tracks,
        playlists: Vec::new(),
        saved,
        artist_genres: HashMap::new(),
    };
    for playlist in cached::<Vec<Playlist>>("playlists.db")?.unwrap_or_default() {
        if let Some(items) = playlist.tracks {
            catalog.playlists.push(
                items
                    .iter()
                    .filter_map(|item| item.track())
                    .map(|track| track.uri)
                    .collect(),
            );
            catalog
                .tracks
                .extend(items.into_iter().filter_map(|item| item.track()));
        }
    }
    for album in cached::<Vec<Album>>("albums.db")?.unwrap_or_default() {
        catalog.tracks.extend(album.tracks.unwrap_or_default());
    }
    for artist in cached::<Vec<Artist>>("artists.db")?.unwrap_or_default() {
        catalog.tracks.extend(artist.tracks.unwrap_or_default());
    }
    if let Some(searches) = cached::<serde_json::Value>("searches.db")? {
        if searches.get("version").and_then(serde_json::Value::as_u64)
            != Some(config::CACHE_VERSION as u64)
        {
            return Err("Search cache has an unsupported version; it was left unchanged".into());
        }
        if let Some(entries) = searches
            .get("entries")
            .and_then(serde_json::Value::as_array)
        {
            for entry in entries {
                if let Some(tracks) = entry.get("tracks") {
                    catalog.tracks.extend(
                        serde_json::from_value::<Vec<Track>>(tracks.clone())
                            .map_err(|error| format!("Invalid cached search tracks: {error}"))?,
                    );
                }
            }
        }
    }
    let (extra, genres) = enrichment::shared().snapshot();
    catalog.tracks.extend(extra);
    catalog.artist_genres = genres;
    catalog.tracks.sort_by(|a, b| a.uri.cmp(&b.uri));
    let track = match seed {
        Some(seed) => catalog
            .tracks
            .iter()
            .find(|track| track.uri == seed || track.id.as_ref() == Some(&seed))
            .cloned()
            .ok_or_else(|| format!("Seed {seed} isn't in the local catalog"))?,
        None => catalog
            .tracks
            .iter()
            .find(|track| !track.is_local && track.id.is_some())
            .cloned()
            .ok_or("No cached songs; play or search for music first")?,
    };
    let history = history::shared();
    let context = Context {
        seed: track,
        queued: HashSet::new(),
        recent: history.recent(),
        feedback: history.snapshot(),
        rng_seed,
        limit,
    };
    serde_json::to_string_pretty(&engine::rank(&catalog, &context))
        .map_err(|error| error.to_string())
}

pub fn replay_report(path: &Path) -> Result<String, String> {
    let bytes = std::fs::read(path)
        .map_err(|error| format!("Can't read replay {}: {error}", path.display()))?;
    let replay: Replay =
        serde_json::from_slice(&bytes).map_err(|error| format!("Invalid replay: {error}"))?;
    if replay.version != 1 {
        return Err("Unsupported radio replay version; file left unchanged".into());
    }
    serde_json::to_string_pretty(&engine::rank(&replay.catalog, &replay.context))
        .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(id: &str) -> Track {
        Track {
            id: Some(id.into()),
            uri: format!("spotify:track:{id}"),
            title: id.into(),
            track_number: 1,
            disc_number: 1,
            duration: 180_000,
            artists: vec!["artist".into()],
            artist_ids: vec!["artist".into()],
            album: Some("album".into()),
            album_id: Some("album".into()),
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
    fn shortlist_reuses_cache_and_invalidates_metadata_changes() {
        let seed = track("cache-test-seed");
        let mut catalog = Catalog {
            tracks: vec![seed.clone(), track("cache-test-next")],
            playlists: vec![],
            saved: HashSet::new(),
            artist_genres: HashMap::new(),
        };
        assert!(!shortlist(&catalog, &seed).1);
        assert!(shortlist(&catalog, &seed).1);
        catalog.tracks[1].is_playable = Some(false);
        let (updated, hit) = shortlist(&catalog, &seed);
        assert!(!hit);
        assert!(
            !updated
                .tracks
                .iter()
                .any(|track| track.uri == "spotify:track:cache-test-next")
        );
    }

    #[test]
    fn replay_round_trip_preserves_picks_and_scores() {
        let seed = track("replay-seed");
        let replay = Replay {
            version: 1,
            catalog: Catalog {
                tracks: vec![seed.clone(), track("replay-next"), track("replay-another")],
                playlists: vec![],
                saved: HashSet::new(),
                artist_genres: HashMap::new(),
            },
            context: Context {
                seed,
                queued: HashSet::new(),
                recent: vec![],
                feedback: HashMap::new(),
                rng_seed: 42,
                limit: 20,
            },
        };
        let loaded: Replay = serde_json::from_slice(&serde_json::to_vec(&replay).unwrap()).unwrap();
        let first = engine::rank(&replay.catalog, &replay.context);
        let second = engine::rank(&loaded.catalog, &loaded.context);
        assert_eq!(
            serde_json::to_value(first.selected).unwrap(),
            serde_json::to_value(second.selected).unwrap()
        );
    }

    #[test]
    #[ignore = "manual warm-cache benchmark"]
    fn warm_shortlist_10k() {
        let seed = track("benchmark-seed");
        let catalog = Catalog {
            tracks: (0..10_000)
                .map(|id| track(&id.to_string()))
                .chain(std::iter::once(seed.clone()))
                .collect(),
            playlists: vec![],
            saved: HashSet::new(),
            artist_genres: HashMap::new(),
        };
        shortlist(&catalog, &seed);
        let start = Instant::now();
        let (short, hit) = shortlist(&catalog, &seed);
        let context = Context {
            seed,
            queued: HashSet::new(),
            recent: vec![],
            feedback: HashMap::new(),
            rng_seed: 42,
            limit: 20,
        };
        let report = engine::rank(&short, &context);
        eprintln!(
            "10k warm shortlist+rank: {:?}; {} picks",
            start.elapsed(),
            report.selected.len()
        );
        assert!(hit);
        assert_eq!(report.selected.len(), 20);
    }
}
