//! Deterministic, cache-only ranking for local radio.
//!
//! The engine deliberately uses metadata relationships rather than text matching.  A title can
//! be a useful display value, but it is a particularly poor proxy for musical similarity (and it
//! makes a radio station surprisingly easy to game).  The signals below are all derived from the
//! cached playlist, artist, album, genre, and listening-history metadata supplied by the caller.

use std::collections::{HashMap, HashSet};
use std::time::Instant;

use rand::{RngExt, SeedableRng, rngs::StdRng};
use serde::{Deserialize, Serialize};

use crate::model::track::Track;
use crate::recommendations::history::Feedback;

/// The maximum number of tracks returned by one ranking pass.
///
/// The interactive station normally asks for twenty tracks, while the service also uses the pure
/// engine to keep a larger deterministic shortlist that can be reranked after playback history
/// and queue exclusions are applied.
pub const MAX_RECOMMENDATIONS: usize = 256;

/// Cached data available to the recommendation engine.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Catalog {
    pub tracks: Vec<Track>,
    /// Playlists represented by their track URIs.
    pub playlists: Vec<Vec<String>>,
    /// Saved track URIs.
    pub saved: HashSet<String>,
    /// Artist ID to genre names.
    pub artist_genres: HashMap<String, Vec<String>>,
}

/// Playback state and user history used to rank a station.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Context {
    pub seed: Track,
    pub queued: HashSet<String>,
    pub recent: Vec<String>,
    pub feedback: HashMap<String, Feedback>,
    pub rng_seed: u64,
    pub limit: usize,
}

/// Feature values used to produce a candidate's score.
///
/// Positive values are normalized evidence in the range `0..=1`, with the exception of
/// `feedback`, which is signed.  Penalty fields are negative contributions to the score.  The
/// diversity penalty is filled in only for selected tracks because it depends on the tracks that
/// preceded the candidate in this particular deterministic sample.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ScoreComponents {
    pub playlist_overlap: f64,
    pub playlist_affinity: f64,
    pub artist_affinity: f64,
    pub album_affinity: f64,
    pub genre_overlap: f64,
    pub feedback: f64,
    pub track_feedback: f64,
    pub artist_feedback: f64,
    pub duration: f64,
    pub recent_penalty: f64,
    pub saved_boost: f64,
    pub diversity_penalty: f64,
    pub fallback_penalty: f64,
}

/// A selected recommendation.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Selection {
    pub track: Track,
    pub score: f64,
    pub reasons: Vec<String>,
    pub components: ScoreComponents,
}

/// Per-track diagnostics.  Diagnostics include excluded tracks so callers can explain why a
/// cache item did not make it into a station.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CandidateDiagnostic {
    pub track_uri: String,
    pub score: f64,
    pub selected: bool,
    pub excluded: bool,
    pub reasons: Vec<String>,
    pub components: ScoreComponents,
}

/// A complete reproducible ranking report.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Report {
    pub seed_uri: String,
    pub rng_seed: u64,
    pub catalog_count: usize,
    pub candidate_count: usize,
    pub elapsed_us: u64,
    pub selected: Vec<Selection>,
    pub candidates: Vec<CandidateDiagnostic>,
    /// A compact explanation of the evidence available for this station as a whole.
    pub reasons: Vec<String>,
    /// A normalized confidence estimate.  Broader-cache fallback lowers confidence when the
    /// requested limit is larger than the strong evidence pool.
    pub confidence: f64,
    /// Whether the broader-cache sparse fallback was used.
    pub fallback: bool,
}

#[derive(Clone)]
struct ScoredCandidate {
    track: Track,
    artists: Vec<String>,
    album: Option<String>,
    score: f64,
    strong: bool,
    reasons: Vec<String>,
    components: ScoreComponents,
}

#[derive(Clone)]
struct ExcludedCandidate {
    track: Track,
    reasons: Vec<String>,
}

struct ScoreInputs<'a> {
    seed: &'a Track,
    seed_artists: &'a HashSet<String>,
    seed_genres: &'a HashSet<String>,
    playlist_index: &'a PlaylistIndex,
    artist_genres: &'a HashMap<String, Vec<String>>,
    saved: &'a HashSet<String>,
    feedback: &'a HashMap<String, Feedback>,
    artist_feedback: &'a HashMap<String, f64>,
    recent: &'a HashMap<String, usize>,
}

/// Rank cached tracks around `context.seed`.
///
/// The ranking is pure with respect to its inputs.  The only apparent time-dependent field is
/// `elapsed_us` in the debug report; it is measurement metadata and does not participate in
/// scoring or ordering.  All iteration that can affect output order is sorted by URI and stable
/// metadata keys before it is sampled with the caller-provided seed.
pub fn rank(catalog: &Catalog, context: &Context) -> Report {
    let started = Instant::now();
    let limit = context.limit.min(MAX_RECOMMENDATIONS);
    let seed_uri = context.seed.uri.clone();

    let canonical_tracks = canonical_tracks(&catalog.tracks);
    let seed_artists = artist_keys(&context.seed);
    let playlist_index = PlaylistIndex::new(
        &catalog.playlists,
        &canonical_tracks,
        &seed_uri,
        &seed_artists,
    );
    let recent_index = recent_index(&context.recent);
    let seed_genres = genres_for(&context.seed, &catalog.artist_genres);
    let artist_feedback = artist_feedback_index(&canonical_tracks, &context.feedback);
    let score_inputs = ScoreInputs {
        seed: &context.seed,
        seed_artists: &seed_artists,
        seed_genres: &seed_genres,
        playlist_index: &playlist_index,
        artist_genres: &catalog.artist_genres,
        saved: &catalog.saved,
        feedback: &context.feedback,
        artist_feedback: &artist_feedback,
        recent: &recent_index,
    };

    let mut scored = Vec::new();
    let mut excluded = Vec::new();

    for track in canonical_tracks {
        let mut exclusion_reasons = Vec::new();
        if track.uri == seed_uri {
            exclusion_reasons.push("seed track".to_string());
        }
        if context.queued.contains(&track.uri) {
            exclusion_reasons.push("already queued".to_string());
        }
        if track.is_local {
            exclusion_reasons.push("local track".to_string());
        }
        if track.is_playable == Some(false) {
            exclusion_reasons.push("marked unplayable".to_string());
        }

        if !exclusion_reasons.is_empty() {
            excluded.push(ExcludedCandidate {
                track,
                reasons: exclusion_reasons,
            });
            continue;
        }

        scored.push(score_track(&track, &score_inputs));
    }

    // A positive metadata relationship is enough to keep a candidate in the strong pool.  This
    // is intentionally broad: an exact artist match or a shared genre should beat an unrelated
    // cache item even when a small cache has no playlist edge.
    let strong_count = scored.iter().filter(|candidate| candidate.strong).count();
    // A sparse strong pool should be augmented from the broader cache only after every strong
    // candidate has been selected.  This keeps the station seed-anchored while allowing newly
    // fetched tracks with no relationship edge yet to enter a long shortlist.
    let fallback = strong_count < limit && !scored.is_empty();
    if fallback {
        for candidate in &mut scored {
            if !candidate.strong {
                candidate.components.fallback_penalty = -FALLBACK_PENALTY;
                candidate.score -= FALLBACK_PENALTY;
                candidate.reasons.push(
                    "sparse cache fallback: no playlist, artist, album, genre, or positive history affinity"
                        .to_string(),
                );
            }
        }
    }

    let mut rng = StdRng::seed_from_u64(context.rng_seed);
    let selected_indices = select_indices(&mut scored, limit, fallback, &mut rng);
    let selected_set: HashSet<usize> = selected_indices.iter().copied().collect();

    let mut selections = Vec::with_capacity(selected_indices.len());
    for index in &selected_indices {
        let candidate = &scored[*index];
        selections.push(Selection {
            track: candidate.track.clone(),
            score: candidate.score,
            reasons: candidate.reasons.clone(),
            components: candidate.components.clone(),
        });
    }

    // Stable diagnostics make the report useful in tests and in a UI even when the source cache
    // was assembled from HashMaps in a different insertion order.
    let mut candidates = Vec::with_capacity(scored.len() + excluded.len());
    for (index, candidate) in scored.iter().enumerate() {
        candidates.push(CandidateDiagnostic {
            track_uri: candidate.track.uri.clone(),
            score: candidate.score,
            selected: selected_set.contains(&index),
            excluded: false,
            reasons: candidate.reasons.clone(),
            components: candidate.components.clone(),
        });
    }
    for candidate in excluded {
        candidates.push(CandidateDiagnostic {
            track_uri: candidate.track.uri,
            score: 0.0,
            selected: false,
            excluded: true,
            reasons: candidate.reasons,
            components: ScoreComponents::default(),
        });
    }
    candidates.sort_by(|left, right| left.track_uri.cmp(&right.track_uri));

    let confidence = station_confidence(&selections, fallback);
    let reasons = report_reasons(
        scored.len(),
        strong_count,
        selections.len(),
        fallback,
        &seed_uri,
    );

    Report {
        seed_uri,
        rng_seed: context.rng_seed,
        catalog_count: catalog.tracks.len(),
        candidate_count: scored.len(),
        elapsed_us: started.elapsed().as_micros().min(u64::MAX as u128) as u64,
        selected: selections,
        candidates,
        reasons,
        confidence,
        fallback,
    }
}

const PLAYLIST_WEIGHT: f64 = 0.52;
const CROSS_ARTIST_PLAYLIST_WEIGHT: f64 = 0.16;
const ARTIST_WEIGHT: f64 = 0.22;
const ALBUM_WEIGHT: f64 = 0.14;
const GENRE_WEIGHT: f64 = 0.18;
const FEEDBACK_WEIGHT: f64 = 0.13;
const DURATION_WEIGHT: f64 = 0.035;
const SAVED_BOOST: f64 = 0.035;
const FALLBACK_PENALTY: f64 = 0.025;
const STRONG_AFFINITY: f64 = 0.08;

fn score_track(track: &Track, inputs: &ScoreInputs<'_>) -> ScoredCandidate {
    let candidate_artists = artist_keys(track);
    let same_artist = same_artist(inputs.seed, track, inputs.seed_artists, &candidate_artists);
    let artist_affinity = if same_artist { 1.0 } else { 0.0 };
    let album_affinity = album_affinity(inputs.seed, track, same_artist);

    let playlist_overlap = inputs.playlist_index.overlap(&track.uri);
    let playlist_affinity = if !same_artist {
        inputs
            .playlist_index
            .inferred_artist_overlap(&candidate_artists, playlist_overlap)
    } else {
        0.0
    };
    let genre_overlap = genre_overlap(inputs.seed_genres, track, inputs.artist_genres);
    let (track_feedback, artist_feedback_signal, feedback_signal) =
        feedback_signal(track, inputs.feedback, inputs.artist_feedback);
    let duration_similarity = duration_similarity(inputs.seed.duration, track.duration);
    let recent_penalty = recent_penalty(&track.uri, inputs.recent);
    let saved_boost = if inputs.saved.contains(&track.uri) {
        SAVED_BOOST
    } else {
        0.0
    };

    // Duration is deliberately a tie-breaker.  It has no effect on unrelated tracks, where it
    // would otherwise make a long cache item appear musically similar to the seed.
    let affinity =
        (playlist_overlap + playlist_affinity + artist_affinity + album_affinity + genre_overlap)
            .min(1.0);
    let duration = if affinity > 0.0 {
        duration_similarity
    } else {
        0.0
    };
    let duration_contribution = duration * DURATION_WEIGHT;
    let playlist_contribution = playlist_overlap * PLAYLIST_WEIGHT;
    let cross_artist_contribution = playlist_affinity * CROSS_ARTIST_PLAYLIST_WEIGHT;
    let artist_contribution = artist_affinity * ARTIST_WEIGHT;
    let album_contribution = album_affinity * ALBUM_WEIGHT;
    let genre_contribution = genre_overlap * GENRE_WEIGHT;
    let feedback_contribution = feedback_signal * FEEDBACK_WEIGHT;

    let mut reasons = Vec::new();
    if playlist_overlap > 0.0 {
        reasons.push(format!(
            "shared seed playlist (normalized {:.2})",
            playlist_overlap
        ));
    }
    if playlist_affinity > 0.0 {
        reasons.push("cross-artist playlist affinity".to_string());
    }
    if artist_affinity > 0.0 {
        reasons.push("same artist".to_string());
    }
    if album_affinity > 0.0 {
        reasons.push("same album".to_string());
    }
    if genre_overlap > 0.0 {
        reasons.push(format!(
            "genre overlap (catalog tags/query evidence; {:.2})",
            genre_overlap
        ));
    }
    if feedback_signal > 0.0 {
        reasons.push("positive listening history".to_string());
    } else if feedback_signal < 0.0 {
        reasons.push("negative listening history".to_string());
    }
    if recent_penalty < 0.0 {
        reasons.push("recently played penalty".to_string());
    }
    if saved_boost > 0.0 {
        reasons.push("saved track".to_string());
    }
    if duration_contribution > 0.0 {
        reasons.push("duration tie-break".to_string());
    }
    if reasons.is_empty() {
        reasons.push("broader cache candidate".to_string());
    }

    let components = ScoreComponents {
        playlist_overlap,
        playlist_affinity,
        artist_affinity,
        album_affinity,
        genre_overlap,
        feedback: feedback_signal,
        track_feedback,
        artist_feedback: artist_feedback_signal,
        duration,
        recent_penalty,
        saved_boost,
        diversity_penalty: 0.0,
        fallback_penalty: 0.0,
    };
    let score = playlist_contribution
        + cross_artist_contribution
        + artist_contribution
        + album_contribution
        + genre_contribution
        + feedback_contribution
        + duration_contribution
        + recent_penalty
        + saved_boost;
    let strong = affinity >= STRONG_AFFINITY || feedback_signal >= 0.2;

    ScoredCandidate {
        track: track.clone(),
        artists: {
            let mut artists = candidate_artists.into_iter().collect::<Vec<_>>();
            artists.sort_unstable();
            artists
        },
        album: album_key(track),
        score,
        strong,
        reasons,
        components,
    }
}

/// Playlist evidence indexed once per rank call.  The weight of a playlist is inversely
/// proportional to its square-root size, so one enormous playlist cannot drown out several small
/// focused playlists.  The artist index lets a track inherit an affinity when another track by
/// that artist appears beside the seed; this is kept separate from exact URI membership so the
/// direct signal is never counted twice as cross-artist evidence.
struct PlaylistIndex {
    weighted_membership: HashMap<String, f64>,
    weighted_artist_membership: HashMap<String, f64>,
    denominator: f64,
}

impl PlaylistIndex {
    fn new(
        playlists: &[Vec<String>],
        tracks: &[Track],
        seed_uri: &str,
        seed_artists: &HashSet<String>,
    ) -> Self {
        let mut weighted_membership = HashMap::new();
        let mut weighted_artist_membership = HashMap::new();
        let mut denominator = 0.0;
        let mut artists_by_uri = HashMap::with_capacity(tracks.len());
        for track in tracks {
            artists_by_uri.insert(track.uri.as_str(), artist_keys(track));
        }

        let mut relevant_playlists = Vec::new();
        for playlist in playlists {
            let mut members = playlist.clone();
            members.sort_unstable();
            members.dedup();
            if members.is_empty() {
                continue;
            }
            let contains_seed = members.iter().any(|uri| uri == seed_uri);
            let contains_seed_artist = members.iter().any(|uri| {
                artists_by_uri
                    .get(uri.as_str())
                    .is_some_and(|artists| !artists.is_disjoint(seed_artists))
            });
            if !contains_seed && !contains_seed_artist {
                continue;
            }

            relevant_playlists.push(members);
        }
        // Playlist insertion order is a cache implementation detail.  Sorting before summing
        // keeps floating-point accumulation and all resulting ties stable across refreshes.
        relevant_playlists.sort();
        for members in relevant_playlists {
            let weight = 1.0 / (members.len() as f64).sqrt();
            denominator += weight;
            let mut playlist_artists = HashSet::new();
            for uri in members {
                if let Some(artists) = artists_by_uri.get(uri.as_str()) {
                    playlist_artists.extend(artists.iter().cloned());
                }
                *weighted_membership.entry(uri).or_insert(0.0) += weight;
            }
            for artist in playlist_artists {
                *weighted_artist_membership.entry(artist).or_insert(0.0) += weight;
            }
        }

        Self {
            weighted_membership,
            weighted_artist_membership,
            denominator,
        }
    }

    fn overlap(&self, uri: &str) -> f64 {
        if self.denominator <= f64::EPSILON {
            0.0
        } else {
            (self.weighted_membership.get(uri).copied().unwrap_or(0.0) / self.denominator)
                .clamp(0.0, 1.0)
        }
    }

    fn inferred_artist_overlap(&self, artists: &HashSet<String>, direct_overlap: f64) -> f64 {
        if self.denominator <= f64::EPSILON {
            return 0.0;
        }
        let artist_overlap = artists
            .iter()
            .filter_map(|artist| self.weighted_artist_membership.get(artist))
            .copied()
            .fold(0.0, f64::max)
            / self.denominator;
        // Exact URI membership already contributes to playlist_overlap.  Only the additional
        // artist graph evidence belongs in the cross-artist component.
        (artist_overlap - direct_overlap).max(0.0).clamp(0.0, 1.0)
    }
}

fn canonical_tracks(tracks: &[Track]) -> Vec<Track> {
    let mut sorted = tracks.to_vec();
    sorted.sort_by(|left, right| {
        let left_preference = canonical_preference(left);
        let right_preference = canonical_preference(right);
        left.uri
            .cmp(&right.uri)
            .then_with(|| left_preference.0.cmp(&right_preference.0))
            .then_with(|| left_preference.1.cmp(&right_preference.1))
            .then_with(|| right_preference.2.cmp(&left_preference.2))
            .then_with(|| track_key(left).cmp(&track_key(right)))
    });

    let mut seen = HashSet::new();
    sorted
        .into_iter()
        .filter(|track| seen.insert(track.uri.clone()))
        .collect()
}

fn canonical_preference(track: &Track) -> (bool, bool, usize) {
    // Prefer a usable remote copy when a cache assembled from several sources contains duplicate
    // URI rows with different availability metadata, then prefer the copy with richer artist and
    // album metadata.  The preference is deterministic and keeps one stale sparse row from hiding
    // a useful copy.
    let richness = track.artist_ids.len()
        + track.artists.len()
        + track.album_id.is_some() as usize
        + track.album.is_some() as usize
        + track.album_artists.len()
        + track.id.is_some() as usize;
    (track.is_local, track.is_playable != Some(true), richness)
}

fn track_key(track: &Track) -> String {
    format!(
        "{:?}",
        (
            (
                track.uri.as_str(),
                track.id.as_deref().unwrap_or_default(),
                track.title.as_str(),
                &track.artist_ids,
                &track.artists,
                &track.album_id,
                &track.album,
                &track.album_artists,
                &track.cover_url,
            ),
            (
                track.url.as_str(),
                track.duration,
                track.disc_number,
                track.track_number,
                track.is_playable,
                track.is_local,
                track.list_index,
                &track.added_at,
            ),
        )
    )
}

fn artist_keys(track: &Track) -> HashSet<String> {
    let mut keys: HashSet<String> = track
        .artist_ids
        .iter()
        .filter(|id| !id.is_empty())
        .cloned()
        .collect();
    if keys.is_empty() {
        keys.extend(
            track
                .artists
                .iter()
                .filter(|name| !name.is_empty())
                .cloned(),
        );
    }
    keys
}

fn same_artist(
    seed: &Track,
    candidate: &Track,
    seed_keys: &HashSet<String>,
    candidate_keys: &HashSet<String>,
) -> bool {
    if !seed_keys.is_disjoint(candidate_keys) {
        return true;
    }

    // IDs are the preferred identity, but cached simplified tracks can be missing one side's
    // IDs.  Names provide an exact metadata fallback in that case; they are never normalized or
    // fuzzy-matched.
    seed.artists.iter().any(|artist| {
        candidate
            .artists
            .iter()
            .any(|candidate_artist| candidate_artist == artist)
    })
}

fn album_affinity(seed: &Track, candidate: &Track, same_artist: bool) -> f64 {
    if let (Some(seed_id), Some(candidate_id)) = (&seed.album_id, &candidate.album_id)
        && !seed_id.is_empty()
        && seed_id == candidate_id
    {
        return 1.0;
    }
    // Album names are not globally unique ("Greatest Hits" is common), so the title fallback is
    // only trusted when the artist relationship is also exact.
    match (&seed.album, &candidate.album) {
        (Some(seed_album), Some(candidate_album))
            if same_artist && !seed_album.is_empty() && seed_album == candidate_album =>
        {
            1.0
        }
        _ => 0.0,
    }
}

fn genre_overlap(
    seed_genres: &HashSet<String>,
    candidate: &Track,
    artist_genres: &HashMap<String, Vec<String>>,
) -> f64 {
    let candidate_genres = genres_for(candidate, artist_genres);
    if seed_genres.is_empty() || candidate_genres.is_empty() {
        return 0.0;
    }
    let intersection = seed_genres.intersection(&candidate_genres).count() as f64;
    let union = seed_genres.union(&candidate_genres).count() as f64;
    if union == 0.0 {
        0.0
    } else {
        intersection / union
    }
}

fn genres_for(track: &Track, artist_genres: &HashMap<String, Vec<String>>) -> HashSet<String> {
    let mut genres = HashSet::new();
    for artist_id in &track.artist_ids {
        if let Some(values) = artist_genres.get(artist_id) {
            genres.extend(values.iter().cloned());
        }
    }
    // Enrichment may have learned a genre from a track search rather than an artist fact.  Keep
    // that evidence under a reserved URI key so it can help newly fetched tracks without
    // pretending that the genre belongs to every artist on the track.
    if let Some(values) = artist_genres.get(&format!("track:{}", track.uri)) {
        genres.extend(values.iter().cloned());
    }
    genres
}

fn duration_similarity(seed_duration: u32, candidate_duration: u32) -> f64 {
    if seed_duration == 0 || candidate_duration == 0 {
        return 0.0;
    }
    let difference = seed_duration.abs_diff(candidate_duration) as f64;
    let scale = seed_duration.max(candidate_duration) as f64;
    (1.0 - difference / scale).clamp(0.0, 1.0)
}

fn recent_index(recent: &[String]) -> HashMap<String, usize> {
    let mut index: HashMap<String, usize> = HashMap::new();
    for (position, uri) in recent.iter().enumerate() {
        index
            .entry(uri.clone())
            .and_modify(|current| *current = (*current).min(position))
            .or_insert(position);
    }
    index
}

fn recent_penalty(uri: &str, recent: &HashMap<String, usize>) -> f64 {
    let Some(position) = recent.get(uri).copied() else {
        return 0.0;
    };
    // The history store returns newest first.  The penalty decays quickly enough that a track
    // played a long time ago can still be recommended.
    -0.24 * (-(position as f64) / 4.0).exp()
}

/// Infer artist history once from URI-keyed track history.  The persisted history intentionally
/// stores one entry per track, so ranking joins those entries with cached artist metadata instead
/// of requiring a second, synthetic artist key in `Context::feedback`.
fn artist_feedback_index(
    tracks: &[Track],
    feedback: &HashMap<String, Feedback>,
) -> HashMap<String, f64> {
    let mut totals: HashMap<String, (f64, usize)> = HashMap::new();
    for track in tracks {
        let Some(entry) = feedback_entry(track, feedback) else {
            continue;
        };
        let signal = feedback_value(entry, track.duration);
        for artist in artist_keys(track) {
            let value = totals.entry(artist).or_insert((0.0, 0));
            value.0 += signal;
            value.1 += 1;
        }
    }
    totals
        .into_iter()
        .map(|(artist, (total, count))| (artist, total / count.max(1) as f64))
        .collect()
}

fn feedback_entry<'a>(
    track: &Track,
    feedback: &'a HashMap<String, Feedback>,
) -> Option<&'a Feedback> {
    feedback
        .get(&track.uri)
        .or_else(|| track.id.as_ref().and_then(|id| feedback.get(id)))
}

fn feedback_signal(
    track: &Track,
    feedback: &HashMap<String, Feedback>,
    artist_feedback: &HashMap<String, f64>,
) -> (f64, f64, f64) {
    let track_signal = feedback_entry(track, feedback)
        .map(|entry| feedback_value(entry, track.duration))
        .unwrap_or(0.0);

    let mut artist_keys = track
        .artist_ids
        .iter()
        .flat_map(|id| [id.clone(), format!("artist:{id}")])
        .chain(
            track
                .artists
                .iter()
                .flat_map(|name| [name.clone(), format!("artist:{name}")]),
        )
        .collect::<Vec<_>>();
    artist_keys.sort_unstable();
    artist_keys.dedup();
    let mut artist_values = artist_keys
        .iter()
        .filter_map(|key| feedback.get(key))
        .map(|entry| feedback_value(entry, track.duration))
        .collect::<Vec<_>>();
    if artist_values.is_empty() {
        artist_values.extend(
            artist_keys
                .iter()
                .filter_map(|key| artist_feedback.get(key).copied()),
        );
    }
    let artist_signal = if artist_values.is_empty() {
        0.0
    } else {
        artist_values.iter().sum::<f64>() / artist_values.len() as f64
    };

    // Track history is intentionally dominant; an artist-level skip pattern should influence a
    // candidate, but should not erase strong playlist evidence for an individual track.
    let combined = (track_signal * 0.75 + artist_signal * 0.25).clamp(-1.0, 1.0);
    (track_signal, artist_signal, combined)
}

fn feedback_value(feedback: &Feedback, duration_ms: u32) -> f64 {
    let plays = feedback
        .plays
        .max(feedback.completed.saturating_add(feedback.skips)) as f64;
    if plays <= 0.0 {
        return 0.0;
    }
    let completion = feedback.completed as f64 / plays;
    let skip_rate = feedback.skips as f64 / plays;
    let listened = if duration_ms == 0 {
        0.0
    } else {
        (feedback.listened_ms as f64 / (duration_ms as f64 * plays)).clamp(0.0, 1.0)
    };
    (0.55 * completion + 0.45 * listened - 0.8 * skip_rate).clamp(-1.0, 1.0)
}

fn select_indices(
    candidates: &mut [ScoredCandidate],
    limit: usize,
    fallback: bool,
    rng: &mut StdRng,
) -> Vec<usize> {
    if limit == 0 || candidates.is_empty() {
        return Vec::new();
    }

    let mut available = candidates
        .iter()
        .enumerate()
        .filter(|(_, candidate)| fallback || candidate.strong)
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    // The fallback mode is the only case in which unrelated cache items are considered.  Strong
    // candidates are still selected as a phase of their own, before any broader-cache items.
    let target = limit.min(available.len());
    if target == 0 {
        return Vec::new();
    }
    let strong_target = candidates
        .iter()
        .filter(|candidate| candidate.strong)
        .count()
        .min(target);

    available.sort_by(|left, right| {
        candidates[*right]
            .strong
            .cmp(&candidates[*left].strong)
            .then_with(|| candidates[*right].score.total_cmp(&candidates[*left].score))
            .then_with(|| {
                candidates[*left]
                    .track
                    .uri
                    .cmp(&candidates[*right].track.uri)
            })
    });

    let first = available.remove(0);
    let mut selected = vec![first];
    let mut artist_counts: HashMap<String, usize> = HashMap::new();
    let mut album_counts: HashMap<String, usize> = HashMap::new();
    add_identity_counts(&candidates[first], &mut artist_counts, &mut album_counts);

    while selected.len() < target && !available.is_empty() {
        let strong_phase = selected.len() < strong_target;
        let eligible = available
            .iter()
            .enumerate()
            .filter(|(_, index)| !strong_phase || candidates[**index].strong)
            .map(|(position, _)| position)
            .collect::<Vec<_>>();
        if eligible.is_empty() {
            break;
        }

        let mut weights = Vec::with_capacity(eligible.len());
        let mut total = 0.0;
        let min_score = eligible
            .iter()
            .map(|position| {
                let index = available[*position];
                candidates[index].score
                    + diversity_penalty(&candidates[index], &artist_counts, &album_counts)
            })
            .fold(f64::INFINITY, f64::min);
        for position in &eligible {
            let index = available[*position];
            let penalty = diversity_penalty(&candidates[index], &artist_counts, &album_counts);
            let effective = candidates[index].score + penalty;
            let weight = ((effective - min_score).max(0.0) + 0.02).powi(2);
            total += weight;
            weights.push((effective, weight));
        }

        let chosen_weight_position = if total <= f64::EPSILON {
            0
        } else {
            let mut draw = rng.random_range(0.0..total);
            let mut position = 0;
            for (_, weight) in &weights {
                if draw <= *weight {
                    break;
                }
                draw -= *weight;
                position += 1;
            }
            position.min(eligible.len() - 1)
        };
        let chosen_position = eligible[chosen_weight_position];
        let chosen = available.swap_remove(chosen_position);
        let penalty = diversity_penalty(&candidates[chosen], &artist_counts, &album_counts);
        candidates[chosen].components.diversity_penalty = penalty;
        candidates[chosen].score += penalty;
        if penalty < 0.0 {
            candidates[chosen]
                .reasons
                .push("artist/album diversity penalty".to_string());
        }
        selected.push(chosen);
        add_identity_counts(&candidates[chosen], &mut artist_counts, &mut album_counts);
    }

    selected
}

fn add_identity_counts(
    candidate: &ScoredCandidate,
    artist_counts: &mut HashMap<String, usize>,
    album_counts: &mut HashMap<String, usize>,
) {
    for artist in &candidate.artists {
        *artist_counts.entry(artist.clone()).or_insert(0) += 1;
    }
    if let Some(album) = &candidate.album {
        *album_counts.entry(album.clone()).or_insert(0) += 1;
    }
}

fn album_key(track: &Track) -> Option<String> {
    track
        .album_id
        .as_ref()
        .filter(|album| !album.is_empty())
        .cloned()
        .or_else(|| {
            track
                .album
                .as_ref()
                .filter(|album| !album.is_empty())
                .cloned()
        })
}

fn diversity_penalty(
    candidate: &ScoredCandidate,
    artist_counts: &HashMap<String, usize>,
    album_counts: &HashMap<String, usize>,
) -> f64 {
    let artist_repeat = candidate
        .artists
        .iter()
        .filter_map(|artist| artist_counts.get(artist))
        .copied()
        .max()
        .unwrap_or(0);
    let album_repeat = candidate
        .album
        .as_ref()
        .and_then(|album| album_counts.get(album))
        .copied()
        .unwrap_or(0);
    -(artist_repeat as f64 * 0.11 + album_repeat as f64 * 0.07)
}

fn station_confidence(selections: &[Selection], fallback: bool) -> f64 {
    if selections.is_empty() {
        return 0.0;
    }
    let mean = selections
        .iter()
        .map(|selection| {
            let components = &selection.components;
            (components.playlist_overlap * 0.45
                + components.playlist_affinity * 0.15
                + components.artist_affinity * 0.20
                + components.album_affinity * 0.10
                + components.genre_overlap * 0.10)
                .clamp(0.0, 1.0)
        })
        .sum::<f64>()
        / selections.len() as f64;
    if fallback {
        (mean * 0.35).clamp(0.0, 1.0)
    } else {
        mean.clamp(0.0, 1.0)
    }
}

fn report_reasons(
    candidate_count: usize,
    strong_count: usize,
    selected_count: usize,
    fallback: bool,
    seed_uri: &str,
) -> Vec<String> {
    if candidate_count == 0 {
        return vec![format!("no eligible cached candidates for seed {seed_uri}")];
    }
    if fallback {
        if strong_count == 0 {
            return vec![format!(
                "sparse cache for {seed_uri}: no affinity evidence; broadened to unrelated cached tracks"
            )];
        }
        return vec![format!(
            "sparse strong pool for {seed_uri}: selected {strong_count} anchored tracks before broader-cache fallback"
        )];
    }
    let mut reasons = vec![format!(
        "{strong_count} cached candidates have playlist, artist, album, genre, or positive history affinity"
    )];
    if selected_count < candidate_count {
        reasons.push(
            "kept the station anchored to strong evidence instead of padding with unrelated cache items"
                .to_string(),
        );
    }
    reasons
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(uri: &str, artist_id: &str, album_id: &str) -> Track {
        Track {
            id: Some(uri.rsplit(':').next().unwrap_or(uri).to_string()),
            uri: uri.to_string(),
            title: uri.to_string(),
            track_number: 1,
            disc_number: 1,
            duration: 180_000,
            artists: vec![artist_id.to_string()],
            artist_ids: vec![artist_id.to_string()],
            album: Some(album_id.to_string()),
            album_id: Some(album_id.to_string()),
            album_artists: vec![artist_id.to_string()],
            cover_url: None,
            url: String::new(),
            added_at: None,
            list_index: 0,
            is_local: false,
            is_playable: Some(true),
        }
    }

    fn context(seed: Track) -> Context {
        Context {
            seed,
            queued: HashSet::new(),
            recent: Vec::new(),
            feedback: HashMap::new(),
            rng_seed: 7,
            limit: 20,
        }
    }

    fn catalog(seed: &Track, tracks: Vec<Track>) -> Catalog {
        Catalog {
            tracks,
            playlists: vec![vec![seed.uri.clone()]],
            saved: HashSet::new(),
            artist_genres: HashMap::new(),
        }
    }

    #[test]
    fn cross_artist_playlist_candidate_beats_unrelated_track() {
        let seed = track("spotify:track:seed", "artist-a", "album-a");
        let related = track("spotify:track:related", "artist-b", "album-b");
        let unrelated = track("spotify:track:unrelated", "artist-c", "album-c");
        let mut data = catalog(
            &seed,
            vec![seed.clone(), related.clone(), unrelated.clone()],
        );
        data.playlists
            .push(vec![seed.uri.clone(), related.uri.clone()]);

        let report = rank(&data, &context(seed));
        assert_eq!(
            report.selected.first().map(|s| s.track.uri.as_str()),
            Some(related.uri.as_str())
        );
        assert!(
            report.selected[0]
                .reasons
                .iter()
                .any(|reason| reason.contains("playlist"))
        );
    }

    #[test]
    fn genre_overlap_is_used_when_playlist_evidence_is_missing() {
        let seed = track("spotify:track:seed", "artist-a", "album-a");
        let same_genre = track("spotify:track:genre", "artist-b", "album-b");
        let other = track("spotify:track:other", "artist-c", "album-c");
        let mut data = catalog(&seed, vec![seed.clone(), same_genre.clone(), other]);
        data.artist_genres
            .insert("artist-a".into(), vec!["ambient".into()]);
        data.artist_genres.insert(
            "artist-b".into(),
            vec!["ambient".into(), "electronic".into()],
        );

        let report = rank(&data, &context(seed));
        assert_eq!(
            report.selected.first().map(|s| s.track.uri.as_str()),
            Some(same_genre.uri.as_str())
        );
        assert!(report.selected[0].components.genre_overlap > 0.0);
    }

    #[test]
    fn playlist_artist_graph_infers_affinity_without_exact_seed_uri() {
        let seed = track("spotify:track:seed", "artist-a", "album-a");
        let seed_copy = track("spotify:track:seed-copy", "artist-a", "album-a2");
        let bridge = track("spotify:track:bridge", "artist-b", "album-b");
        let inferred = track("spotify:track:inferred", "artist-b", "album-c");
        let data = Catalog {
            tracks: vec![seed.clone(), seed_copy.clone(), bridge, inferred.clone()],
            playlists: vec![vec![seed_copy.uri, "spotify:track:bridge".into()]],
            saved: HashSet::new(),
            artist_genres: HashMap::new(),
        };

        let report = rank(&data, &context(seed));
        let candidate = report
            .candidates
            .iter()
            .find(|candidate| candidate.track_uri == inferred.uri)
            .unwrap();
        assert!(candidate.components.playlist_affinity > 0.0);
        assert!(
            candidate
                .reasons
                .iter()
                .any(|reason| reason.contains("cross-artist"))
        );
    }

    #[test]
    fn artist_feedback_is_joined_from_other_track_history() {
        let seed = track("spotify:track:seed", "artist-a", "album-a");
        let listened = track("spotify:track:listened", "artist-b", "album-b");
        let candidate = track("spotify:track:candidate", "artist-b", "album-c");
        let mut data = catalog(&seed, vec![seed.clone(), listened, candidate.clone()]);
        data.playlists.clear();
        let mut ctx = context(seed);
        ctx.feedback.insert(
            "spotify:track:listened".into(),
            Feedback {
                completed: 3,
                skips: 0,
                plays: 3,
                listened_ms: 540_000,
                last_played: 1,
            },
        );
        let report = rank(&data, &ctx);
        let diagnostic = report
            .candidates
            .iter()
            .find(|diagnostic| diagnostic.track_uri == candidate.uri)
            .unwrap();
        assert!(diagnostic.components.artist_feedback > 0.0);
        assert!(
            diagnostic
                .reasons
                .iter()
                .any(|reason| reason.contains("positive") && reason.contains("history"))
        );
    }

    #[test]
    fn album_title_fallback_requires_matching_artist() {
        let seed = track("spotify:track:seed", "artist-a", "Greatest Hits");
        let mut unrelated = track("spotify:track:unrelated", "artist-b", "Greatest Hits");
        unrelated.album_id = Some("album-other".into());
        let report = rank(
            &catalog(&seed, vec![seed.clone(), unrelated.clone()]),
            &context(seed),
        );
        let diagnostic = report
            .candidates
            .iter()
            .find(|diagnostic| diagnostic.track_uri == unrelated.uri)
            .unwrap();
        assert_eq!(diagnostic.components.album_affinity, 0.0);
    }

    #[test]
    fn track_keyed_genre_evidence_helps_new_artist_candidates() {
        let seed = track("spotify:track:seed", "artist-a", "album-a");
        let candidate = track("spotify:track:new", "artist-new", "album-new");
        let mut data = catalog(&seed, vec![seed.clone(), candidate.clone()]);
        data.artist_genres
            .insert("artist-a".into(), vec!["ambient".into()]);
        data.artist_genres
            .insert(format!("track:{}", candidate.uri), vec!["ambient".into()]);

        let report = rank(&data, &context(seed));
        let diagnostic = report
            .candidates
            .iter()
            .find(|diagnostic| diagnostic.track_uri == candidate.uri)
            .unwrap();
        assert!(diagnostic.components.genre_overlap > 0.0);
        assert!(
            report
                .selected
                .iter()
                .any(|selection| selection.track.uri == candidate.uri)
        );
    }

    #[test]
    fn recent_and_negative_feedback_lower_a_candidate() {
        let seed = track("spotify:track:seed", "artist-a", "album-a");
        let fresh = track("spotify:track:fresh", "artist-b", "album-b");
        let stale = track("spotify:track:stale", "artist-c", "album-c");
        let mut data = catalog(&seed, vec![seed.clone(), fresh.clone(), stale.clone()]);
        data.playlists
            .push(vec![seed.uri.clone(), fresh.uri.clone()]);
        data.playlists
            .push(vec![seed.uri.clone(), stale.uri.clone()]);
        let mut ctx = context(seed);
        ctx.recent = vec![fresh.uri.clone()];
        ctx.feedback.insert(
            stale.uri.clone(),
            Feedback {
                completed: 0,
                skips: 3,
                plays: 3,
                listened_ms: 2_000,
                last_played: 1,
            },
        );
        let report = rank(&data, &ctx);
        let fresh_candidate = report
            .candidates
            .iter()
            .find(|candidate| candidate.track_uri == fresh.uri)
            .unwrap();
        let stale_candidate = report
            .candidates
            .iter()
            .find(|candidate| candidate.track_uri == stale.uri)
            .unwrap();
        assert!(fresh_candidate.components.recent_penalty < 0.0);
        assert!(stale_candidate.components.feedback < 0.0);
        assert!(
            stale_candidate
                .reasons
                .iter()
                .any(|reason| reason.contains("negative"))
        );
    }

    #[test]
    fn diversity_penalizes_repeated_artist_and_excludes_invalid_tracks() {
        let seed = track("spotify:track:seed", "artist-a", "album-a");
        let first = track("spotify:track:first", "artist-b", "album-b");
        let second = track("spotify:track:second", "artist-b", "album-c");
        let third = track("spotify:track:third", "artist-c", "album-d");
        let mut local = track("spotify:track:local", "artist-d", "album-e");
        local.is_local = true;
        let mut unavailable = track("spotify:track:unavailable", "artist-e", "album-f");
        unavailable.is_playable = Some(false);
        let mut data = catalog(
            &seed,
            vec![
                seed.clone(),
                first.clone(),
                second.clone(),
                third.clone(),
                local.clone(),
                unavailable,
            ],
        );
        data.playlists.extend([
            vec![seed.uri.clone(), first.uri.clone(), second.uri.clone()],
            vec![seed.uri.clone(), third.uri.clone()],
        ]);
        let mut ctx = context(seed);
        ctx.limit = 3;
        let report = rank(&data, &ctx);
        assert!(
            report
                .selected
                .iter()
                .any(|selection| selection.track.uri == third.uri)
        );
        assert!(
            report
                .candidates
                .iter()
                .filter(|candidate| candidate.excluded)
                .all(|candidate| candidate.track_uri != first.uri)
        );
        assert!(report.candidates.iter().any(|candidate| {
            candidate.track_uri == local.uri
                && candidate
                    .reasons
                    .iter()
                    .any(|reason| reason == "local track")
        }));
    }

    #[test]
    fn ordering_is_stable_across_track_and_genre_insertion_order() {
        let seed = track("spotify:track:seed", "artist-a", "album-a");
        let first = track("spotify:track:first", "artist-b", "album-b");
        let second = track("spotify:track:second", "artist-c", "album-c");
        let mut left = catalog(&seed, vec![seed.clone(), first.clone(), second.clone()]);
        left.playlists = vec![vec![
            seed.uri.clone(),
            first.uri.clone(),
            second.uri.clone(),
        ]];
        left.artist_genres.insert(
            "artist-a".into(),
            vec!["ambient".into(), "electronic".into()],
        );
        left.artist_genres
            .insert("artist-b".into(), vec!["ambient".into()]);

        let mut right = catalog(&seed, vec![second, seed.clone(), first]);
        right.playlists = vec![vec![
            seed.uri.clone(),
            "spotify:track:second".into(),
            "spotify:track:first".into(),
        ]];
        right
            .artist_genres
            .insert("artist-b".into(), vec!["ambient".into()]);
        right.artist_genres.insert(
            "artist-a".into(),
            vec!["electronic".into(), "ambient".into()],
        );

        let left_report = rank(&left, &context(seed.clone()));
        let right_report = rank(&right, &context(seed));
        let left_order = left_report
            .selected
            .iter()
            .map(|selection| selection.track.uri.clone())
            .collect::<Vec<_>>();
        let right_order = right_report
            .selected
            .iter()
            .map(|selection| selection.track.uri.clone())
            .collect::<Vec<_>>();
        assert_eq!(left_order, right_order);
    }

    #[test]
    fn empty_cache_reports_no_candidates() {
        let seed = track("spotify:track:seed", "artist-a", "album-a");
        let report = rank(
            &Catalog {
                tracks: Vec::new(),
                playlists: Vec::new(),
                saved: HashSet::new(),
                artist_genres: HashMap::new(),
            },
            &context(seed),
        );
        assert!(report.selected.is_empty());
        assert_eq!(report.candidate_count, 0);
        assert!(
            report
                .reasons
                .iter()
                .any(|reason| reason.contains("no eligible"))
        );
    }

    #[test]
    fn duplicate_uri_is_stably_deduplicated_and_prefers_playable_copy() {
        let seed = track("spotify:track:seed", "artist-a", "album-a");
        let mut stale = track("spotify:track:duplicate", "artist-b", "album-b");
        stale.is_playable = Some(false);
        let playable = track("spotify:track:duplicate", "artist-b", "album-b");
        let data = catalog(&seed, vec![seed.clone(), stale, playable.clone()]);

        let report = rank(&data, &context(seed));
        assert_eq!(report.candidate_count, 1);
        assert_eq!(report.selected[0].track.uri, playable.uri);
        assert_eq!(report.selected[0].track.is_playable, Some(true));
    }

    #[test]
    fn duplicate_uri_prefers_richer_artist_and_album_metadata() {
        let seed = track("spotify:track:seed", "artist-a", "album-a");
        let mut sparse = track("spotify:track:duplicate", "artist-b", "album-b");
        sparse.artist_ids.clear();
        sparse.artists.clear();
        sparse.album_id = None;
        sparse.album = None;
        let rich = track("spotify:track:duplicate", "artist-b", "album-b");
        let report = rank(
            &catalog(&seed, vec![seed.clone(), sparse, rich.clone()]),
            &context(seed),
        );
        assert_eq!(report.selected[0].track.artist_ids, rich.artist_ids);
        assert_eq!(report.selected[0].track.album_id, rich.album_id);
    }

    #[test]
    #[ignore = "manual benchmark; run with --ignored when tuning the cache index"]
    fn ranks_ten_thousand_cached_tracks_quickly() {
        let seed = track("spotify:track:seed", "artist-a", "album-a");
        let mut tracks = vec![seed.clone()];
        let mut playlist = vec![seed.uri.clone()];
        for index in 0..10_000 {
            let candidate = track(
                &format!("spotify:track:{index:05}"),
                &format!("artist-{}", index % 500),
                &format!("album-{}", index % 1_000),
            );
            playlist.push(candidate.uri.clone());
            tracks.push(candidate);
        }
        let mut benchmark_context = context(seed.clone());
        benchmark_context.limit = MAX_RECOMMENDATIONS;
        let report = rank(
            &Catalog {
                tracks,
                playlists: vec![playlist],
                saved: HashSet::new(),
                artist_genres: HashMap::new(),
            },
            &benchmark_context,
        );
        eprintln!("10k rank elapsed_us={}", report.elapsed_us);
        assert_eq!(report.selected.len(), MAX_RECOMMENDATIONS);
    }
}
