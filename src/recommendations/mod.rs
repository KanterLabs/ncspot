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
use crate::model::{album::Album, artist::Artist, playlist::Playlist, track::Track};
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
    pub session_played_count: usize,
    pub total_us: u128,
    pub applied: bool,
    pub applied_count: usize,
    pub discarded_reason: Option<String>,
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
        familiar_artists: HashSet::new(),
    }
}

fn fingerprint(catalog: &Catalog) -> u64 {
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    // Canonical ordering prevents cache churn when a refreshed library is sorted.
    let mut tracks: Vec<_> = catalog.tracks.iter().collect();
    tracks.sort_by(|a, b| a.uri.cmp(&b.uri).then_with(|| a.title.cmp(&b.title)));
    for track in tracks {
        track.uri.hash(&mut hash);
        engine::song_key(track).hash(&mut hash);
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

fn shortlist(
    catalog: &Catalog,
    seed: &Track,
    feedback: &HashMap<String, history::Feedback>,
) -> (Catalog, bool) {
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    fingerprint(catalog).hash(&mut hash);
    // History changes invalidate the pools, but changing the dial reuses them.
    let mut feedback_entries: Vec<_> = feedback.iter().collect();
    feedback_entries.sort_by(|a, b| a.0.cmp(b.0));
    serde_json::to_vec(&feedback_entries)
        .unwrap_or_default()
        .hash(&mut hash);
    let key = hash.finish();
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
                    familiar_artists: catalog.familiar_artists.clone(),
                },
                true,
            );
        }
    }
    let mut seen = HashSet::new();
    let mut tracks = Vec::new();
    // Keep both ends of the dial warm without multiplying the cache by level.
    for level in [0, 100] {
        let context = Context {
            seed: seed.clone(),
            queued: HashSet::new(),
            session_played: HashSet::new(),
            session_song_keys: HashSet::new(),
            recent: Vec::new(),
            feedback: feedback.clone(),
            rng_seed: 42,
            limit: SHORTLIST_SIZE / 2,
            discovery: Some(level),
        };
        for selection in engine::rank(catalog, &context).selected {
            if seen.insert(selection.track.uri.clone()) {
                tracks.push(selection.track);
            }
        }
    }
    tracks.push(seed.clone());
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
            familiar_artists: catalog.familiar_artists.clone(),
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
        session_played: queue.session_played(),
        session_song_keys: queue.session_played_song_keys(),
        recent: history.recent(),
        feedback: history.snapshot(),
        rng_seed: rand::random(),
        limit: 50,
        discovery: Some(library.cfg.discovery()),
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
    let mut catalog = catalog(queue, library);
    let source_count = catalog
        .tracks
        .iter()
        .map(|track| &track.uri)
        .collect::<HashSet<_>>()
        .len();
    let history = history::shared();
    // Parked playback context is still a metadata source, but only actual
    // upcoming entries reserve songs in the active station.
    let queued = queue.recommendation_queued();
    let context = Context {
        seed: seed.clone(),
        queued,
        session_played: queue.session_played(),
        session_song_keys: queue.session_played_song_keys(),
        recent: history.recent(),
        feedback: history.snapshot(),
        rng_seed,
        limit,
        discovery: Some(library.cfg.discovery()),
    };
    catalog.familiar_artists = engine::familiar_artists(&catalog, &context.feedback);
    let (short, shortlist_hit) = shortlist(&catalog, &seed, &context.feedback);
    let mut replay = Replay {
        version: 3,
        catalog: short,
        context,
    };
    let mut report = engine::rank(&replay.catalog, &replay.context);
    // A long queue can consume a shortlist; regenerate from the full pool rather
    // than pretending the catalog is empty or calling a remote recommender.
    if (report.selected.len() < limit || report.discovery.as_ref().is_some_and(|d| d.shortfall > 0))
        && replay.catalog.tracks.len() < catalog.tracks.len()
    {
        let full = engine::rank(&catalog, &replay.context);
        if full.selected.len() > report.selected.len()
            || (full.selected.len() == report.selected.len()
                && full.discovery.as_ref().map(|d| d.shortfall)
                    < report.discovery.as_ref().map(|d| d.shortfall))
        {
            replay.catalog = catalog;
            report = full;
        }
    }
    let diagnostic = Diagnostic {
        algorithm: "local-radio-v3",
        source_count,
        shortlist_hit,
        session_played_count: replay.context.session_played.len(),
        total_us: started.elapsed().as_micros(),
        applied: false,
        applied_count: 0,
        discarded_reason: None,
        history_status: history.status(),
        enrichment_status: enrichment::shared().status(),
        report,
    };
    (diagnostic, replay)
}

pub fn remember(diagnostic: Diagnostic, replay: Replay) {
    log::debug!(
        "radio: seed={} rng={} shortlist_hit={} sources={} selected={} session_played={} elapsed_us={} applied={}",
        diagnostic.report.seed_uri,
        diagnostic.report.rng_seed,
        diagnostic.shortlist_hit,
        diagnostic.source_count,
        diagnostic.report.selected.len(),
        diagnostic.session_played_count,
        diagnostic.total_us,
        diagnostic.applied
    );
    if let Some(discovery) = &diagnostic.report.discovery {
        log::debug!("radio: discovery={discovery:?}");
    }
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

pub fn diagnostics(queue: &Queue) -> String {
    let station = if queue.radio_active() {
        if queue.radio_waiting() {
            "waiting for related metadata"
        } else {
            "active"
        }
    } else {
        "off"
    };
    let last = service().last.lock().unwrap().clone();
    let report = last
        .and_then(|report| serde_json::to_string_pretty(&report).ok())
        .unwrap_or_else(|| {
            "No radio run yet. Start Radio to capture scores and exclusions.".into()
        });
    format!(
        "{report}\n\nRadio: {station}\nStation seed: {:?}\nSession played/attempted: {}\nNext discovery: {}%\n\nHistory: {}\nEnrichment: {}\n\nReport: {}\nReplay: {}\nOffline replay: {} radio-debug --replay <replay path>\n",
        queue.radio_seed(),
        queue.session_played().len(),
        queue.get_library().cfg.discovery(),
        history::shared().status(),
        enrichment::shared().status(),
        config::cache_path("radio-debug.json").display(),
        config::cache_path("radio-replay.json").display(),
        ncspot::BIN_NAME
    )
}

/// Prepare the seed's candidate cache and catalog enrichment off the UI thread.
pub fn prewarm(queue: Arc<Queue>, library: Arc<Library>, events: EventManager) {
    let seed = queue
        .radio_seed_track()
        .or_else(|| queue.get_current().and_then(|item| item.track()));
    let Some(seed) = seed else {
        return;
    };
    if seed.is_local || seed.id.is_none() {
        return;
    }
    #[cfg(not(test))]
    std::thread::spawn(move || {
        let mut catalog = catalog(&queue, &library);
        let feedback = history::shared().snapshot();
        catalog.familiar_artists = engine::familiar_artists(&catalog, &feedback);
        let _ = shortlist(&catalog, &seed, &feedback);
        let related = related_artists(&catalog, &seed);
        enrichment::shared().refresh(queue.get_spotify(), seed, related, events);
    });
    #[cfg(test)]
    let _ = (library, events);
}

pub(crate) fn related_artists(catalog: &Catalog, seed: &Track) -> Vec<(String, String)> {
    // Enrichment must follow the same seed evidence as selection: a mixed
    // playlist containing one seed-artist song is not a license to search for
    // arbitrary artists from that playlist.
    let context = Context {
        seed: seed.clone(),
        queued: HashSet::new(),
        session_played: HashSet::new(),
        session_song_keys: HashSet::new(),
        recent: Vec::new(),
        feedback: HashMap::new(),
        rng_seed: 42,
        limit: SHORTLIST_SIZE,
        discovery: Some(50),
    };
    let report = engine::rank(catalog, &context);
    let mut seen = HashSet::new();
    report
        .selected
        .into_iter()
        .flat_map(|selection| {
            selection
                .track
                .artist_ids
                .into_iter()
                .zip(selection.track.artists)
        })
        .filter(|(id, _)| !seed.artist_ids.contains(id) && seen.insert(id.clone()))
        .take(2)
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
pub fn offline_report(
    seed: Option<String>,
    rng_seed: u64,
    limit: usize,
    discovery: u8,
) -> Result<String, String> {
    let tracks = cached::<Vec<Track>>("tracks.db")?.unwrap_or_default();
    let saved = tracks.iter().map(|track| track.uri.clone()).collect();
    let mut catalog = Catalog {
        tracks,
        playlists: Vec::new(),
        saved,
        artist_genres: HashMap::new(),
        familiar_artists: HashSet::new(),
    };
    // The persisted queue is another cached metadata source, including songs
    // from earlier playback contexts. Read it without creating a Config or
    // altering provenance/history; online workers snapshot the live queue instead.
    let state_path = config::config_path(ncspot::USER_STATE_FILE_NAME);
    match std::fs::read(&state_path) {
        Ok(bytes) => {
            let state: config::UserState = serde_cbor::from_slice(&bytes)
                .map_err(|error| format!("Can't read cached queue state: {error}"))?;
            catalog.tracks.extend(
                state
                    .queuestate
                    .queue
                    .into_iter()
                    .filter_map(|item| item.track()),
            );
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("Can't read cached queue state: {error}")),
    }
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
        session_played: HashSet::new(),
        session_song_keys: HashSet::new(),
        recent: history.recent(),
        feedback: history.snapshot(),
        rng_seed,
        limit,
        discovery: Some(discovery.min(100)),
    };
    catalog.familiar_artists = engine::familiar_artists(&catalog, &context.feedback);
    serde_json::to_string_pretty(&engine::rank(&catalog, &context))
        .map_err(|error| error.to_string())
}

pub fn replay_report(path: &Path) -> Result<String, String> {
    let bytes = std::fs::read(path)
        .map_err(|error| format!("Can't read replay {}: {error}", path.display()))?;
    let mut replay: Replay =
        serde_json::from_slice(&bytes).map_err(|error| format!("Invalid replay: {error}"))?;
    if !matches!(replay.version, 1..=3) {
        return Err("Unsupported radio replay version; file left unchanged".into());
    }
    if replay.version == 1 {
        replay.context.discovery = None;
        replay.context.session_played.clear();
        replay.context.session_song_keys.clear();
    } else if replay.context.discovery.is_none() {
        return Err(format!(
            "Version {} replay is missing its discovery level",
            replay.version
        ));
    }
    serde_json::to_string_pretty(&engine::rank(&replay.catalog, &replay.context))
        .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::model::playable::Playable;

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
    fn restored_mixed_queue_is_metadata_without_reserving_every_candidate() {
        let cfg = Config::new_for_test();
        let events = EventManager::new_for_test();
        let spotify = crate::spotify::Spotify::new_for_test(cfg.clone(), events.clone());
        let library = Library::new_for_test(events, spotify.clone(), cfg.clone());
        let mut seed = track("RestoredCountrySeed");
        seed.artists = vec!["Country artist".into()];
        seed.artist_ids = vec!["CountryArtist".into()];
        seed.album_id = Some("CountrySeedAlbum".into());
        let related: Vec<_> = (0..4)
            .map(|index| {
                let mut song = seed.clone();
                song.id = Some(format!("RestoredRelated{index}"));
                song.uri = format!("spotify:track:RestoredRelated{index}");
                song.title = format!("Related Song {index}");
                song.album_id = Some(format!("CountryAlbum{index}"));
                song
            })
            .collect();
        let mut entries: Vec<_> = related.iter().cloned().map(Playable::Track).collect();
        entries.extend((0..1000).map(|index| {
            let mut song = track(&format!("RestoredUnrelated{index}"));
            song.artists = vec![format!("Other artist {index}")];
            song.artist_ids = vec![format!("OtherArtist{index}")];
            song.album_id = Some(format!("OtherAlbum{index}"));
            Playable::Track(song)
        }));
        let current = entries.len();
        entries.push(Playable::Track(seed.clone()));
        // Related tracks are old cached context, not tracks played in this process.
        let queue = Queue::new_for_test(entries, Some(current), spotify, cfg, library.clone());
        queue.start_radio_track(&seed);
        assert_eq!(
            queue.recommendation_queued(),
            HashSet::from([seed.uri.clone()])
        );
        for level in [0, 50, 100] {
            library.cfg.set_discovery(level);
            let (diagnostic, replay) = recommend(&queue, &library, seed.clone(), 42, 20);
            assert_eq!(diagnostic.report.selected.len(), related.len());
            assert!(
                diagnostic
                    .report
                    .selected
                    .iter()
                    .all(|pick| pick.track.artist_ids == seed.artist_ids)
            );
            assert_eq!(replay.version, 3);
            assert_eq!(replay.context.seed.uri, seed.uri);
            assert_eq!(replay.context.queued.len(), 1);
        }
        assert_eq!(queue.queue.read().unwrap().len(), 1005);
    }

    #[test]
    fn enrichment_does_not_search_artists_from_a_giant_mixed_seed_playlist() {
        let seed = track("EnrichmentQualitySeed");
        let mut tracks = vec![seed.clone()];
        tracks.extend((0..500).map(|index| {
            let mut song = track(&format!("EnrichmentUnrelated{index}"));
            song.artist_ids = vec![format!("OtherEnrichmentArtist{index}")];
            song.artists = vec![format!("Other enrichment artist {index}")];
            song.album_id = Some(format!("OtherEnrichmentAlbum{index}"));
            song
        }));
        let memberships = tracks.iter().map(|song| song.uri.clone()).collect();
        let catalog = Catalog {
            tracks,
            playlists: vec![memberships],
            saved: HashSet::new(),
            artist_genres: HashMap::new(),
            familiar_artists: HashSet::new(),
        };
        assert!(related_artists(&catalog, &seed).is_empty());
    }

    #[test]
    fn shortlist_reuses_cache_and_invalidates_metadata_changes() {
        let seed = track("cache-test-seed");
        let mut catalog = Catalog {
            tracks: vec![seed.clone(), track("cache-test-next")],
            playlists: vec![],
            saved: HashSet::new(),
            artist_genres: HashMap::new(),
            familiar_artists: HashSet::new(),
        };
        assert!(!shortlist(&catalog, &seed, &HashMap::new()).1);
        assert!(shortlist(&catalog, &seed, &HashMap::new()).1);
        catalog.tracks[1].is_playable = Some(false);
        let (updated, hit) = shortlist(&catalog, &seed, &HashMap::new());
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
                familiar_artists: HashSet::new(),
            },
            context: Context {
                seed,
                queued: HashSet::new(),
                session_played: HashSet::new(),
                session_song_keys: HashSet::new(),
                recent: vec![],
                feedback: HashMap::new(),
                rng_seed: 42,
                limit: 20,
                discovery: None,
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
    fn shortlist_reuses_both_modes_and_invalidates_feedback() {
        let seed = track("discovery-pools-seed");
        let tracks: Vec<_> = (0..200)
            .map(|id| track(&format!("pool-{id}")))
            .chain(std::iter::once(seed.clone()))
            .collect();
        let mut feedback = HashMap::new();
        for song in tracks.iter().take(100) {
            feedback.insert(
                song.uri.clone(),
                history::Feedback {
                    completed: 2,
                    plays: 2,
                    ..Default::default()
                },
            );
        }
        let mut catalog = Catalog {
            tracks,
            playlists: vec![],
            saved: HashSet::new(),
            artist_genres: HashMap::new(),
            familiar_artists: HashSet::new(),
        };
        catalog.familiar_artists = engine::familiar_artists(&catalog, &feedback);
        let (pool, hit) = shortlist(&catalog, &seed, &feedback);
        assert!(!hit);
        assert!(pool.tracks.len() <= SHORTLIST_SIZE + 1);
        for level in [0, 50, 100] {
            let context = Context {
                seed: seed.clone(),
                queued: HashSet::new(),
                session_played: HashSet::new(),
                session_song_keys: HashSet::new(),
                recent: vec![],
                feedback: feedback.clone(),
                rng_seed: 42,
                limit: 20,
                discovery: Some(level),
            };
            let report = engine::rank(&pool, &context);
            assert_eq!(report.selected.len(), 20);
            assert_eq!(report.discovery.as_ref().unwrap().shortfall, 0);
            assert!(shortlist(&catalog, &seed, &feedback).1);
        }
        feedback.insert(
            "spotify:track:pool-150".into(),
            history::Feedback {
                skips: 3,
                plays: 3,
                ..Default::default()
            },
        );
        assert!(!shortlist(&catalog, &seed, &feedback).1);
    }

    #[test]
    fn legacy_json_defaults_preserve_ranking_and_new_replay_captures_level() {
        let seed = track("json-replay-seed");
        let replay = Replay {
            version: 1,
            catalog: Catalog {
                tracks: vec![seed.clone(), track("json-replay-next")],
                playlists: vec![],
                saved: HashSet::new(),
                artist_genres: HashMap::new(),
                familiar_artists: HashSet::new(),
            },
            context: Context {
                seed,
                queued: HashSet::new(),
                session_played: HashSet::new(),
                session_song_keys: HashSet::new(),
                recent: vec![],
                feedback: HashMap::new(),
                rng_seed: 99,
                limit: 10,
                discovery: None,
            },
        };
        let mut legacy = serde_json::to_value(&replay).unwrap();
        legacy["context"]
            .as_object_mut()
            .unwrap()
            .remove("discovery");
        legacy["catalog"]
            .as_object_mut()
            .unwrap()
            .remove("familiar_artists");
        let loaded: Replay = serde_json::from_value(legacy).unwrap();
        assert_eq!(
            serde_json::to_value(engine::rank(&replay.catalog, &replay.context).selected).unwrap(),
            serde_json::to_value(engine::rank(&loaded.catalog, &loaded.context).selected).unwrap()
        );
        let mut current = replay;
        current.version = 2;
        current.context.discovery = Some(75);
        let loaded: Replay =
            serde_json::from_slice(&serde_json::to_vec(&current).unwrap()).unwrap();
        assert_eq!(loaded.context.discovery, Some(75));
        assert_eq!(
            engine::rank(&loaded.catalog, &loaded.context)
                .discovery
                .unwrap()
                .level,
            75
        );
    }

    #[test]
    fn replay_versions_are_read_only_and_legacy_ignores_new_level() {
        let seed = track("version-replay-seed");
        let mut replay = Replay {
            version: 1,
            catalog: Catalog {
                tracks: vec![seed.clone(), track("version-replay-next")],
                playlists: vec![],
                saved: HashSet::new(),
                artist_genres: HashMap::new(),
                familiar_artists: HashSet::new(),
            },
            context: Context {
                seed,
                queued: HashSet::new(),
                session_played: HashSet::new(),
                session_song_keys: HashSet::new(),
                recent: vec![],
                feedback: HashMap::new(),
                rng_seed: 99,
                limit: 10,
                discovery: Some(100),
            },
        };
        let path = std::env::temp_dir().join(format!(
            "resonance-replay-{}-{}.json",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        for version in [1, 2, 3, 99] {
            replay.version = version;
            let bytes = serde_json::to_vec(&replay).unwrap();
            std::fs::write(&path, &bytes).unwrap();
            let result = replay_report(&path);
            if version == 99 {
                assert!(result.unwrap_err().contains("Unsupported"));
            } else {
                let report: serde_json::Value = serde_json::from_str(&result.unwrap()).unwrap();
                if version == 1 {
                    assert!(report.get("discovery").is_none());
                } else {
                    assert_eq!(report["discovery"]["level"], 100);
                }
            }
            assert_eq!(std::fs::read(&path).unwrap(), bytes);
        }
        replay.version = 2;
        replay.context.discovery = None;
        let bytes = serde_json::to_vec(&replay).unwrap();
        std::fs::write(&path, &bytes).unwrap();
        assert!(replay_report(&path).unwrap_err().contains("missing"));
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    #[ignore = "manual warm-cache benchmark"]
    fn warm_shortlist_10k() {
        let seed = track("benchmark-seed");
        let mut catalog = Catalog {
            tracks: (0..10_000)
                .map(|id| track(&id.to_string()))
                .chain(std::iter::once(seed.clone()))
                .collect(),
            playlists: vec![],
            saved: HashSet::new(),
            artist_genres: HashMap::new(),
            familiar_artists: HashSet::new(),
        };
        let feedback: HashMap<_, _> = catalog
            .tracks
            .iter()
            .take(5_000)
            .map(|song| {
                (
                    song.uri.clone(),
                    history::Feedback {
                        completed: 2,
                        plays: 2,
                        ..Default::default()
                    },
                )
            })
            .collect();
        catalog.familiar_artists = engine::familiar_artists(&catalog, &feedback);
        shortlist(&catalog, &seed, &feedback);
        let start = Instant::now();
        let (short, hit) = shortlist(&catalog, &seed, &feedback);
        let context = Context {
            seed,
            queued: HashSet::new(),
            session_played: HashSet::new(),
            session_song_keys: HashSet::new(),
            recent: vec![],
            feedback,
            rng_seed: 42,
            limit: 20,
            discovery: Some(75),
        };
        let report = engine::rank(&short, &context);
        eprintln!(
            "10k warm shortlist+rank: {:?}; {} picks",
            start.elapsed(),
            report.selected.len()
        );
        assert!(hit);
        assert_eq!(report.selected.len(), 20);
        assert_eq!(report.discovery.unwrap().selected_unplayed, 15);
    }
}
