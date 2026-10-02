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
    /// Artist keys known from saved tracks or positive listening history.
    ///
    /// The service normally fills this from a full catalog before handing a cached shortlist to
    /// the engine.  Keeping it on the catalog lets a shortlist retain artist familiarity even
    /// when the corresponding tracks are no longer present in that shortlist.
    #[serde(default, skip_serializing_if = "HashSet::is_empty")]
    pub familiar_artists: HashSet<String>,
}

/// Playback state and user history used to rank a station.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Context {
    pub seed: Track,
    pub queued: HashSet<String>,
    /// Tracks already played during this terminal session.
    #[serde(default, skip_serializing_if = "HashSet::is_empty")]
    pub session_played: HashSet<String>,
    /// Song identities already played during this terminal session.
    ///
    /// URI history is retained for exact replay compatibility, while this set lets callers keep
    /// a song excluded after its cached URI has disappeared or when another Spotify release of
    /// the same song is encountered.
    #[serde(default, skip_serializing_if = "HashSet::is_empty")]
    pub session_song_keys: HashSet<String>,
    pub recent: Vec<String>,
    pub feedback: HashMap<String, Feedback>,
    pub rng_seed: u64,
    pub limit: usize,
    /// Optional discovery dial.  `None` selects the original v1 ranking exactly.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub discovery: Option<u8>,
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
    /// Bounded discovery-dial contribution; zero for the legacy ranking path.
    #[serde(default, skip_serializing_if = "is_zero_f64")]
    pub discovery: f64,
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
    /// Discovery diagnostics, present only when a discovery level was requested.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub discovery: Option<DiscoveryReport>,
}

/// Diagnostics for a discovery-ranked station.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscoveryReport {
    pub level: u8,
    pub target_unplayed: usize,
    pub selected_unplayed: usize,
    pub unfamiliar_artists: usize,
    pub shortfall: usize,
    pub history_sparse: bool,
    /// Human-readable details about sparse history, quota shortfalls, or artist evidence.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reasons: Vec<String>,
}

#[derive(Clone)]
struct ScoredCandidate {
    track: Track,
    artists: Vec<String>,
    album: Option<String>,
    score: f64,
    /// Whether the candidate has enough seed-derived evidence for discovery radio.
    ///
    /// This intentionally excludes feedback-only affinity.  A globally liked track is useful to
    /// the legacy ranking path, but it is not a seed relationship for an explicit discovery
    /// station.
    seed_related: bool,
    strong: bool,
    reasons: Vec<String>,
    components: ScoreComponents,
}

#[derive(Clone)]
struct ExcludedCandidate {
    track: Track,
    score: f64,
    reasons: Vec<String>,
    components: ScoreComponents,
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

struct DiscoveryInputs<'a> {
    level: u8,
    target_unplayed: usize,
    history_sparse: bool,
    saved: &'a HashSet<String>,
    feedback: &'a HashMap<String, Feedback>,
    familiar_artists: HashSet<String>,
}

fn is_zero_f64(value: &f64) -> bool {
    *value == 0.0
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
    let discovery_requested = context.discovery.is_some();

    let canonical_tracks = canonical_tracks(&catalog.tracks);
    let catalog_song_keys = song_keys_by_uri(&canonical_tracks);
    let seed_song_key = song_key(&context.seed);
    let mut queued_song_keys = song_keys_for_uris(&catalog_song_keys, &context.queued);
    queued_song_keys.extend(song_keys_for_uris(
        &catalog_song_keys,
        &context.session_played,
    ));
    let seed_artists = artist_keys(&context.seed);
    let playlist_index = PlaylistIndex::new(
        &catalog.playlists,
        &canonical_tracks,
        &seed_uri,
        &seed_artists,
        discovery_requested,
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
    let mut eligible_tracks = Vec::new();

    for track in canonical_tracks {
        let mut exclusion_reasons = Vec::new();
        if if discovery_requested {
            same_track_identity(&track, &context.seed)
        } else {
            track.uri == seed_uri
        } {
            exclusion_reasons.push("seed track".to_string());
        }
        if if discovery_requested {
            matches_identifier(&track, &context.queued)
        } else {
            context.queued.contains(&track.uri)
        } {
            exclusion_reasons.push("already queued".to_string());
        }
        if if discovery_requested {
            matches_identifier(&track, &context.session_played)
        } else {
            context.session_played.contains(&track.uri)
        } {
            exclusion_reasons.push("already played in this terminal session".to_string());
        }
        if discovery_requested {
            let candidate_song_key = song_key(&track);
            if !seed_song_key.is_empty() && candidate_song_key == seed_song_key {
                exclusion_reasons.push("same song as seed".to_string());
            }
            if !candidate_song_key.is_empty()
                && (queued_song_keys.contains(&candidate_song_key)
                    || context.session_song_keys.contains(&candidate_song_key))
            {
                exclusion_reasons.push("same song already queued or played".to_string());
            }
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
                score: 0.0,
                reasons: exclusion_reasons,
                components: ScoreComponents::default(),
            });
            continue;
        }

        eligible_tracks.push(track);
    }

    let eligible_tracks = if discovery_requested {
        canonical_song_tracks(eligible_tracks)
    } else {
        eligible_tracks
    };
    for track in eligible_tracks {
        scored.push(score_track(&track, &score_inputs));
    }

    // Explicit discovery is allowed to select only candidates with seed-derived evidence.  Keep
    // unrelated scored rows in diagnostics, rather than letting them inflate candidate_count or
    // silently disappear, so a short station can explain why its cached pool was exhausted.
    if discovery_requested {
        let mut related = Vec::with_capacity(scored.len());
        for candidate in scored.drain(..) {
            if candidate.seed_related {
                related.push(candidate);
            } else {
                let mut reasons = candidate.reasons;
                if candidate.components.playlist_affinity > 0.0 {
                    reasons.push(
                        "indirect playlist evidence is too weak for this station".to_string(),
                    );
                }
                reasons.push("no seed relationship".to_string());
                excluded.push(ExcludedCandidate {
                    track: candidate.track,
                    score: candidate.score,
                    reasons,
                    components: candidate.components,
                });
            }
        }
        scored = related;
    }

    // A positive metadata relationship is enough to keep a candidate in the strong pool.  This
    // is intentionally broad: an exact artist match or a shared genre should beat an unrelated
    // cache item even when a small cache has no playlist edge.
    let strong_count = if discovery_requested {
        scored
            .iter()
            .filter(|candidate| candidate.seed_related)
            .count()
    } else {
        scored.iter().filter(|candidate| candidate.strong).count()
    };
    // A sparse strong pool should be augmented from the broader cache only after every strong
    // candidate has been selected.  This keeps the station seed-anchored while allowing newly
    // fetched tracks with no relationship edge yet to enter a long shortlist.
    let fallback = !discovery_requested && strong_count < limit && !scored.is_empty();
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

    let discovery_inputs = context.discovery.map(|level| {
        let level = level.min(100);
        let history_sparse = history_is_sparse(&context.feedback);
        let familiar_artists = familiar_artists(catalog, &context.feedback);
        DiscoveryInputs {
            level,
            target_unplayed: discovery_target(limit, level),
            history_sparse,
            saved: &catalog.saved,
            feedback: &context.feedback,
            familiar_artists,
        }
    });

    if let Some(discovery) = discovery_inputs.as_ref() {
        apply_discovery_scores(&mut scored, discovery);
    }

    let mut rng = StdRng::seed_from_u64(context.rng_seed);
    let selected_indices = match discovery_inputs.as_ref() {
        Some(discovery) => select_indices_discovery(&mut scored, limit, &mut rng, discovery),
        None => select_indices(&mut scored, limit, fallback, &mut rng),
    };
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
            score: candidate.score,
            selected: false,
            excluded: true,
            reasons: candidate.reasons,
            components: candidate.components,
        });
    }
    candidates.sort_by(|left, right| left.track_uri.cmp(&right.track_uri));

    let confidence = station_confidence(&selections, fallback);
    let mut reasons = report_reasons(
        scored.len(),
        strong_count,
        selections.len(),
        fallback,
        &seed_uri,
    );
    if discovery_requested && scored.is_empty() {
        reasons = vec![format!(
            "no seed-related cached candidates for seed {seed_uri}"
        )];
    }
    let discovery_report = discovery_inputs.as_ref().map(|discovery| {
        let selected_unplayed = selections
            .iter()
            .filter(|selection| track_is_unplayed(&selection.track, discovery.feedback))
            .count();
        let unfamiliar_artists = selections
            .iter()
            .filter(|selection| track_has_unfamiliar_artist(&selection.track, discovery))
            .count();
        let shortfall = discovery.target_unplayed.saturating_sub(selected_unplayed);
        let mut discovery_reasons = Vec::new();
        if discovery.history_sparse {
            discovery_reasons.push(
                "sparse playback history; saved tracks are used as a familiarity proxy".to_string(),
            );
        }
        if shortfall > 0 {
            discovery_reasons.push(format!(
                "discovery target shortfall: requested {} locally unplayed tracks, selected {}",
                discovery.target_unplayed, selected_unplayed
            ));
        }
        if selections.len() < limit {
            discovery_reasons.push(format!(
                "seed-related cache exhausted: selected {} of requested {} tracks",
                selections.len(),
                limit
            ));
        }
        if unfamiliar_artists > 0 {
            discovery_reasons.push(format!(
                "{} selected tracks come from artists outside saved or positive history",
                unfamiliar_artists
            ));
        }
        reasons.push(format!(
            "discovery level {} targeted {} locally unplayed tracks",
            discovery.level, discovery.target_unplayed
        ));
        reasons.extend(discovery_reasons.iter().cloned());
        DiscoveryReport {
            level: discovery.level,
            target_unplayed: discovery.target_unplayed,
            selected_unplayed,
            unfamiliar_artists,
            shortfall,
            history_sparse: discovery.history_sparse,
            reasons: discovery_reasons,
        }
    });

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
        discovery: discovery_report,
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
    // Artist co-occurrence is indirect evidence. A single mixed playlist
    // containing another song by the seed artist cannot define the station.
    // Require independent playlist corroboration unless direct artist/album,
    // cached genre, or exact seed membership already relates the candidate.
    let seed_related = artist_affinity + album_affinity + genre_overlap + playlist_overlap
        >= STRONG_AFFINITY
        || (playlist_affinity >= STRONG_AFFINITY
            && inputs
                .playlist_index
                .artist_corroborated(&candidate_artists));
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
        if inputs.playlist_index.discovery
            && !inputs
                .playlist_index
                .artist_corroborated(&candidate_artists)
        {
            reasons.push("indirect playlist edge lacks independent corroboration".to_string());
        }
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
        discovery: 0.0,
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
    // Legacy replay strength includes any metadata affinity; explicit
    // discovery separately gates the pool with `seed_related` above.
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
        seed_related,
        strong,
        reasons,
        components,
    }
}

/// Playlist evidence indexed once per rank call.  Discovery ranking gives each member a further
/// inverse-square-root-size share so a focused playlist remains useful while one giant mixed
/// playlist cannot make every member a direct seed match.
/// Artist-only seed playlists feed the weaker artist graph; only playlists containing the exact
/// seed URI feed direct membership in that path.  The legacy path retains its original weighting.
struct PlaylistIndex {
    discovery: bool,
    weighted_membership: HashMap<String, f64>,
    weighted_artist_membership: HashMap<String, f64>,
    artist_playlist_count: HashMap<String, usize>,
    denominator: f64,
}

impl PlaylistIndex {
    fn new(
        playlists: &[Vec<String>],
        tracks: &[Track],
        seed_uri: &str,
        seed_artists: &HashSet<String>,
        discovery: bool,
    ) -> Self {
        let mut weighted_membership = HashMap::new();
        let mut weighted_artist_membership = HashMap::new();
        let mut artist_playlist_count = HashMap::new();
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

            relevant_playlists.push((members, contains_seed));
        }
        // Playlist insertion order is a cache implementation detail.  Sorting before summing
        // keeps floating-point accumulation and all resulting ties stable across refreshes.
        relevant_playlists.sort();
        if discovery {
            relevant_playlists.dedup();
        }
        for (members, contains_seed) in relevant_playlists {
            let weight = 1.0 / (members.len() as f64).sqrt();
            denominator += weight;
            let member_weight = if discovery {
                weight / (members.len() as f64).sqrt()
            } else {
                weight
            };
            let mut playlist_artists = HashSet::new();
            for uri in members {
                if let Some(artists) = artists_by_uri.get(uri.as_str()) {
                    playlist_artists.extend(artists.iter().cloned());
                }
                if !discovery || contains_seed {
                    *weighted_membership.entry(uri.clone()).or_insert(0.0) += member_weight;
                }
            }
            for artist in playlist_artists {
                *artist_playlist_count.entry(artist.clone()).or_insert(0) += 1;
                *weighted_artist_membership.entry(artist).or_insert(0.0) += member_weight;
            }
        }

        Self {
            discovery,
            weighted_membership,
            weighted_artist_membership,
            artist_playlist_count,
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

    fn artist_corroborated(&self, artists: &HashSet<String>) -> bool {
        artists
            .iter()
            .any(|artist| self.artist_playlist_count.get(artist).copied().unwrap_or(0) >= 2)
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

/// Return a stable identity for a cached song.
///
/// Cached tracks can have several Spotify releases with different URIs.  A normalized title plus
/// the sorted normalized performer names groups those releases while keeping explicitly labeled
/// versions such as "Live" or "Acoustic" distinct.  The fallback keeps sparse cache rows
/// addressable without inventing a title or artist relationship.
pub fn song_key(track: &Track) -> String {
    let title = normalize_song_part(&track.title);
    let mut artists = track
        .artists
        .iter()
        .map(|artist| normalize_song_part(artist))
        .filter(|artist| !artist.is_empty())
        .collect::<Vec<_>>();
    artists.sort_unstable();
    artists.dedup();

    if !title.is_empty() && !artists.is_empty() {
        return format!("song:{title}\u{1f}{}", artists.join("\u{1e}"));
    }
    if let Some(id) = track
        .id
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty())
    {
        return format!("spotify-id:{id}");
    }
    let uri = track.uri.trim();
    if uri.is_empty() {
        String::new()
    } else {
        format!("spotify-uri:{uri}")
    }
}

fn normalize_song_part(value: &str) -> String {
    value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

fn song_keys_by_uri(tracks: &[Track]) -> HashMap<String, String> {
    let mut keys = HashMap::new();
    for track in tracks {
        let key = song_key(track);
        if key.is_empty() {
            continue;
        }
        keys.insert(track.uri.clone(), key.clone());
        if let Some(id) = track.id.as_ref().filter(|id| !id.is_empty()) {
            keys.insert(id.clone(), key);
        }
    }
    keys
}

fn song_keys_for_uris(
    song_keys_by_uri: &HashMap<String, String>,
    uris: &HashSet<String>,
) -> HashSet<String> {
    uris.iter()
        .filter_map(|uri| song_keys_by_uri.get(uri).cloned())
        .collect()
}

fn same_track_identity(left: &Track, right: &Track) -> bool {
    left.uri == right.uri
        || left
            .id
            .as_ref()
            .zip(right.id.as_ref())
            .is_some_and(|(left_id, right_id)| !left_id.is_empty() && left_id == right_id)
}

fn matches_identifier(track: &Track, identifiers: &HashSet<String>) -> bool {
    identifiers.contains(&track.uri)
        || track
            .id
            .as_ref()
            .is_some_and(|id| !id.is_empty() && identifiers.contains(id))
}

/// Deduplicate song variants after exact URI exclusions have been applied.
///
/// Filtering first is important: a queued sparse alias must not become the canonical row and hide
/// an eligible, metadata-rich copy of another cached URI.  If every alias is excluded, no copy is
/// reintroduced.
fn canonical_song_tracks(tracks: Vec<Track>) -> Vec<Track> {
    let mut keyed = HashMap::new();
    let mut unkeyed = Vec::new();
    for track in tracks {
        let key = song_key(&track);
        if key.is_empty() {
            unkeyed.push(track);
            continue;
        }
        match keyed.entry(key) {
            std::collections::hash_map::Entry::Vacant(entry) => {
                entry.insert(track);
            }
            std::collections::hash_map::Entry::Occupied(mut entry) => {
                if track_preference_cmp(&track, entry.get()).is_lt() {
                    entry.insert(track);
                }
            }
        }
    }

    let mut canonical = keyed.into_values().chain(unkeyed).collect::<Vec<_>>();
    canonical.sort_by(|left, right| {
        left.uri
            .cmp(&right.uri)
            .then_with(|| track_preference_cmp(left, right))
    });
    canonical
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

fn track_preference_cmp(left: &Track, right: &Track) -> std::cmp::Ordering {
    let left_preference = canonical_preference(left);
    let right_preference = canonical_preference(right);
    left_preference
        .0
        .cmp(&right_preference.0)
        .then_with(|| left_preference.1.cmp(&right_preference.1))
        .then_with(|| right_preference.2.cmp(&left_preference.2))
        .then_with(|| track_key(left).cmp(&track_key(right)))
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

/// Return artist keys that are familiar from saved tracks or positive playback history.
///
/// The helper intentionally uses the same [`artist_keys`] identity rule as scoring: IDs are
/// preferred, with names as the fallback when IDs are absent.  Callers that rank a partial
/// shortlist can compute this over a full catalog and copy the result into
/// [`Catalog::familiar_artists`] first.
pub fn familiar_artists(
    catalog: &Catalog,
    feedback: &HashMap<String, Feedback>,
) -> HashSet<String> {
    let mut familiar = catalog.familiar_artists.clone();
    for track in &catalog.tracks {
        let saved = catalog.saved.contains(&track.uri);
        let positive_history = feedback_entry(track, feedback)
            .map(|entry| feedback_value(entry, track.duration) > 0.0)
            .unwrap_or(false);
        if saved || positive_history {
            familiar.extend(artist_keys(track));
        }
    }
    familiar
}

const SPARSE_HISTORY_PLAYS: usize = 3;

fn history_is_sparse(feedback: &HashMap<String, Feedback>) -> bool {
    feedback.values().filter(|entry| entry.plays > 0).count() < SPARSE_HISTORY_PLAYS
}

fn discovery_target(limit: usize, level: u8) -> usize {
    limit.saturating_mul(level.min(100) as usize) / 100
}

fn track_is_played(track: &Track, feedback: &HashMap<String, Feedback>) -> bool {
    feedback_entry(track, feedback)
        .map(|entry| entry.plays > 0)
        .unwrap_or(false)
}

fn track_is_unplayed(track: &Track, feedback: &HashMap<String, Feedback>) -> bool {
    !track_is_played(track, feedback)
}

fn track_is_familiar(track: &Track, discovery: &DiscoveryInputs<'_>) -> bool {
    track_is_played(track, discovery.feedback)
        || (discovery.history_sparse && discovery.saved.contains(&track.uri))
}

fn track_has_familiar_artist(track: &Track, discovery: &DiscoveryInputs<'_>) -> bool {
    artist_keys(track)
        .iter()
        .any(|artist| discovery.familiar_artists.contains(artist))
}

fn track_has_unfamiliar_artist(track: &Track, discovery: &DiscoveryInputs<'_>) -> bool {
    let artists = artist_keys(track);
    !artists.is_empty() && !track_has_familiar_artist(track, discovery)
}

fn candidate_has_negative_feedback(candidate: &ScoredCandidate) -> bool {
    // The station seed is a fresh artist choice. Inherited artist history
    // still affects the score, but cannot veto all of that artist's unplayed
    // songs. A skip of this particular song continues to count normally.
    candidate.components.track_feedback < -0.05
        || (candidate.components.artist_affinity <= 0.0 && candidate.components.feedback < -0.05)
}

fn apply_discovery_scores(candidates: &mut [ScoredCandidate], discovery: &DiscoveryInputs<'_>) {
    let level = discovery.level.min(100) as f64 / 100.0;
    // Keep the balanced midpoint continuous.  Familiarity fades to zero at 50%, and exploration
    // grows from zero there, instead of leaving a discontinuity where neither side contributes.
    let familiarity_weight = (0.5 - level).max(0.0) * 2.0;
    let exploration_weight = (level - 0.5).max(0.0) * 2.0;
    for candidate in candidates {
        let unplayed = track_is_unplayed(&candidate.track, discovery.feedback);
        let familiar = track_is_familiar(&candidate.track, discovery);
        let unfamiliar_artist = track_has_unfamiliar_artist(&candidate.track, discovery);
        let mut adjustment = 0.0;

        // Quota selection below supplies the main dial.  These small score terms make ordering
        // inside each quota deterministic and visible in diagnostics without overwhelming a
        // strong playlist, artist, album, or genre relationship.
        if familiarity_weight > 0.0 && familiar && !candidate_has_negative_feedback(candidate) {
            adjustment += 0.035 * familiarity_weight;
        }
        if exploration_weight > 0.0 && unplayed {
            adjustment += 0.035 * exploration_weight;
        }
        if exploration_weight > 0.0 && unfamiliar_artist {
            adjustment += 0.045 * exploration_weight;
        }

        if adjustment != 0.0 {
            candidate.score += adjustment;
            candidate.components.discovery += adjustment;
            if unplayed {
                candidate
                    .reasons
                    .push("discovery preference: locally unplayed".to_string());
            } else if familiar {
                candidate
                    .reasons
                    .push("discovery preference: familiar history".to_string());
            }
            if unfamiliar_artist && level > 0.5 {
                candidate
                    .reasons
                    .push("discovery preference: unfamiliar artist".to_string());
            }
        }
    }
}

fn discovery_preferred_indices(
    candidates: &[ScoredCandidate],
    available: &[usize],
    selected_unplayed: usize,
    discovery: &DiscoveryInputs<'_>,
) -> Vec<usize> {
    let needs_unplayed = selected_unplayed < discovery.target_unplayed;
    if needs_unplayed {
        let unplayed = available
            .iter()
            .copied()
            .filter(|index| {
                track_is_unplayed(&candidates[*index].track, discovery.feedback)
                    && !candidate_has_negative_feedback(&candidates[*index])
            })
            .collect::<Vec<_>>();
        if !unplayed.is_empty() {
            return unplayed;
        }
    }

    // A skipped candidate remains eligible when the cache offers no alternative, but it should
    // not displace a neutral or positive candidate merely because it was previously played.
    let familiar = available
        .iter()
        .copied()
        .filter(|index| {
            track_is_familiar(&candidates[*index].track, discovery)
                && !candidate_has_negative_feedback(&candidates[*index])
        })
        .collect::<Vec<_>>();
    if !familiar.is_empty() {
        return familiar;
    }

    let neutral = available
        .iter()
        .copied()
        .filter(|index| !candidate_has_negative_feedback(&candidates[*index]))
        .collect::<Vec<_>>();
    if !neutral.is_empty() {
        return neutral;
    }

    available.to_vec()
}

/// Discovery-aware variant of [`select_indices`].  Discovery candidates are restricted to the
/// seed-related pool; the legacy selector remains separate so the `None` path retains v1's exact
/// score and RNG behavior.
fn select_indices_discovery(
    candidates: &mut [ScoredCandidate],
    limit: usize,
    rng: &mut StdRng,
    discovery: &DiscoveryInputs<'_>,
) -> Vec<usize> {
    if limit == 0 || candidates.is_empty() {
        return Vec::new();
    }

    let mut available = candidates
        .iter()
        .enumerate()
        .filter(|(_, candidate)| candidate.seed_related)
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    let target = limit.min(available.len());
    if target == 0 {
        return Vec::new();
    }
    let strong_target = candidates
        .iter()
        .filter(|candidate| candidate.seed_related)
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

    let mut selected = Vec::with_capacity(target);
    let mut selected_unplayed = 0;
    let mut artist_counts: HashMap<String, usize> = HashMap::new();
    let mut album_counts: HashMap<String, usize> = HashMap::new();

    while selected.len() < target && !available.is_empty() {
        let strong_phase = selected.len() < strong_target;
        let phase = available
            .iter()
            .copied()
            .filter(|index| !strong_phase || candidates[*index].seed_related)
            .collect::<Vec<_>>();
        if phase.is_empty() {
            break;
        }
        let preferred =
            discovery_preferred_indices(candidates, &phase, selected_unplayed, discovery);

        let min_score = preferred
            .iter()
            .map(|index| {
                candidates[*index].score
                    + diversity_penalty(&candidates[*index], &artist_counts, &album_counts)
            })
            .fold(f64::INFINITY, f64::min);
        let mut weights = Vec::with_capacity(preferred.len());
        let mut total = 0.0;
        for index in &preferred {
            let penalty = diversity_penalty(&candidates[*index], &artist_counts, &album_counts);
            let effective = candidates[*index].score + penalty;
            let weight = ((effective - min_score).max(0.0) + 0.02).powi(2);
            total += weight;
            weights.push(weight);
        }

        let chosen_preferred_position = if total <= f64::EPSILON {
            0
        } else {
            let mut draw = rng.random_range(0.0..total);
            let mut position = 0;
            for weight in &weights {
                if draw <= *weight {
                    break;
                }
                draw -= *weight;
                position += 1;
            }
            position.min(preferred.len() - 1)
        };
        let chosen = preferred[chosen_preferred_position];
        let available_position = available
            .iter()
            .position(|index| *index == chosen)
            .expect("discovery candidate must remain available");
        available.swap_remove(available_position);

        let penalty = diversity_penalty(&candidates[chosen], &artist_counts, &album_counts);
        candidates[chosen].components.diversity_penalty = penalty;
        candidates[chosen].score += penalty;
        if penalty < 0.0 {
            candidates[chosen]
                .reasons
                .push("artist/album diversity penalty".to_string());
        }
        if track_is_unplayed(&candidates[chosen].track, discovery.feedback) {
            selected_unplayed += 1;
        }
        selected.push(chosen);
        add_identity_counts(&candidates[chosen], &mut artist_counts, &mut album_counts);
    }

    selected
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
            session_played: HashSet::new(),
            session_song_keys: HashSet::new(),
            recent: Vec::new(),
            feedback: HashMap::new(),
            rng_seed: 7,
            limit: 20,
            discovery: None,
        }
    }

    fn catalog(seed: &Track, tracks: Vec<Track>) -> Catalog {
        Catalog {
            tracks,
            playlists: vec![vec![seed.uri.clone()]],
            saved: HashSet::new(),
            artist_genres: HashMap::new(),
            familiar_artists: HashSet::new(),
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
    fn discovery_none_keeps_legacy_playlist_score_golden_value() {
        let seed = track("spotify:track:seed", "artist-a", "album-a");
        let candidate = track("spotify:track:candidate", "artist-b", "album-b");
        let data = Catalog {
            tracks: vec![seed.clone(), candidate.clone()],
            playlists: vec![vec![seed.uri.clone(), candidate.uri.clone()]],
            saved: HashSet::new(),
            artist_genres: HashMap::new(),
            familiar_artists: HashSet::new(),
        };
        let mut context = context(seed);
        context.limit = 1;
        let report = rank(&data, &context);
        let selection = report.selected.first().unwrap();
        assert_eq!(selection.track.uri, candidate.uri);
        assert!((selection.score - 0.555).abs() < 1e-12);
        assert_eq!(selection.components.playlist_overlap, 1.0);
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
            familiar_artists: HashSet::new(),
        };

        let report = rank(&data, &context(seed));
        let candidate = report
            .candidates
            .iter()
            .find(|candidate| candidate.track_uri == inferred.uri)
            .unwrap();
        assert_eq!(candidate.components.playlist_overlap, 0.0);
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
                familiar_artists: HashSet::new(),
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
    fn song_key_normalizes_title_and_artist_order_without_stripping_versions() {
        let mut first = track("spotify:track:first", "artist-a", "album-a");
        first.title = "  Girls   Girls Girls  ".into();
        first.artists = vec!["Artist B".into(), "Artist A".into()];
        let mut second = track("spotify:track:second", "artist-b", "album-b");
        second.title = "girls girls girls".into();
        second.artists = vec![" artist a ".into(), "artist b".into()];
        assert_eq!(song_key(&first), song_key(&second));

        let mut live = second.clone();
        live.title = "Girls Girls Girls - Live".into();
        assert_ne!(song_key(&first), song_key(&live));

        let mut sparse = second;
        sparse.title.clear();
        sparse.artists.clear();
        assert_eq!(song_key(&sparse), "spotify-id:second");
    }

    #[test]
    fn discovery_rejects_title_matches_and_global_positive_history_without_seed_evidence() {
        let mut seed = track("spotify:track:seed", "zach-bryan", "album-seed");
        seed.title = "Drowning".into();
        seed.artists = vec!["Zach Bryan".into()];
        let mut metal = track("spotify:track:metal", "metal-artist", "album-metal");
        metal.title = "Drowning".into();
        metal.artists = vec!["Metal Artist".into()];
        let mut rap = track("spotify:track:rap", "rap-artist", "album-rap");
        rap.title = "Drowning".into();
        rap.artists = vec!["Rap Artist".into()];
        let mut data = catalog(&seed, vec![seed.clone(), metal.clone(), rap.clone()]);
        data.playlists = vec![vec![seed.uri.clone()]];
        let mut context = discovery_context(seed, 50, 20);
        context
            .feedback
            .insert(metal.uri.clone(), positive_feedback());
        context
            .feedback
            .insert(rap.uri.clone(), positive_feedback());

        let report = rank(&data, &context);
        assert!(report.selected.is_empty());
        assert!(!report.fallback);
        assert_eq!(report.candidate_count, 0);
        for uri in [metal.uri, rap.uri] {
            let diagnostic = report
                .candidates
                .iter()
                .find(|candidate| candidate.track_uri == uri)
                .unwrap();
            assert!(!diagnostic.selected);
            assert!(diagnostic.excluded);
            assert!(
                diagnostic
                    .reasons
                    .iter()
                    .any(|reason| reason == "no seed relationship")
            );
            assert!(diagnostic.score > 0.0);
            assert!(diagnostic.components.feedback > 0.0);
            assert_eq!(diagnostic.components.playlist_overlap, 0.0);
            assert_eq!(diagnostic.components.playlist_affinity, 0.0);
            assert_eq!(diagnostic.components.artist_affinity, 0.0);
            assert_eq!(diagnostic.components.album_affinity, 0.0);
            assert_eq!(diagnostic.components.genre_overlap, 0.0);
        }
    }

    #[test]
    fn playlist_evidence_is_size_diluted_and_artist_only_is_not_direct_membership() {
        let seed = track("spotify:track:seed", "artist-a", "album-seed");
        let bridge = track("spotify:track:bridge", "artist-a", "album-bridge");
        let noisy = track("spotify:track:noisy", "artist-noisy", "album-noisy");

        let mut artist_only = Catalog {
            tracks: vec![seed.clone(), bridge.clone(), noisy.clone()],
            playlists: vec![vec![bridge.uri.clone(), noisy.uri.clone()]],
            saved: HashSet::new(),
            artist_genres: HashMap::new(),
            familiar_artists: HashSet::new(),
        };
        let artist_only_report = rank(&artist_only, &discovery_context(seed.clone(), 50, 20));
        let artist_only_diagnostic = artist_only_report
            .candidates
            .iter()
            .find(|candidate| candidate.track_uri == noisy.uri)
            .unwrap();
        assert_eq!(artist_only_diagnostic.components.playlist_overlap, 0.0);
        assert!(artist_only_diagnostic.components.playlist_affinity > 0.0);

        let mut focused_members = vec![seed.uri.clone(), noisy.uri.clone()];
        focused_members.extend((0..28).map(|index| format!("spotify:track:focused-{index:02}")));
        artist_only.playlists = vec![focused_members];
        let focused_report = rank(&artist_only, &discovery_context(seed.clone(), 50, 20));
        let focused_diagnostic = focused_report
            .candidates
            .iter()
            .find(|candidate| candidate.track_uri == noisy.uri)
            .unwrap();
        assert!(focused_diagnostic.components.playlist_overlap >= STRONG_AFFINITY);
        assert!(
            focused_report
                .selected
                .iter()
                .any(|selection| selection.track.uri == noisy.uri)
        );

        let mut giant_members = vec![seed.uri.clone(), noisy.uri.clone(), bridge.uri.clone()];
        giant_members.extend((0..997).map(|index| format!("spotify:track:filler-{index:03}")));
        artist_only.playlists = vec![giant_members];
        let giant_report = rank(&artist_only, &discovery_context(seed, 50, 20));
        let giant_diagnostic = giant_report
            .candidates
            .iter()
            .find(|candidate| candidate.track_uri == noisy.uri)
            .unwrap();
        assert!(giant_diagnostic.components.playlist_overlap < STRONG_AFFINITY);
        assert!(giant_diagnostic.components.playlist_affinity < STRONG_AFFINITY);
        assert!(
            !giant_report
                .selected
                .iter()
                .any(|selection| selection.track.uri == noisy.uri)
        );
    }

    fn positive_feedback() -> Feedback {
        Feedback {
            completed: 1,
            skips: 0,
            plays: 1,
            listened_ms: 180_000,
            last_played: 1,
        }
    }

    fn discovery_context(seed: Track, level: u8, limit: usize) -> Context {
        let mut context = context(seed);
        context.discovery = Some(level);
        context.limit = limit;
        context
    }

    #[test]
    fn artist_only_playlist_edges_require_independent_corroboration() {
        let seed = track("spotify:track:CorroborationSeed", "country-a", "seed-album");
        let bridge_a = track("spotify:track:CountryBridgeA", "country-a", "album-a");
        let bridge_b = track("spotify:track:CountryBridgeB", "country-a", "album-b");
        let candidate = track(
            "spotify:track:OtherArtistCandidate",
            "other-artist",
            "album-c",
        );
        let mut data = catalog(
            &seed,
            vec![
                seed.clone(),
                bridge_a.clone(),
                bridge_b.clone(),
                candidate.clone(),
            ],
        );
        let first = vec![bridge_a.uri.clone(), candidate.uri.clone()];
        data.playlists = vec![first.clone(), first];
        let context = discovery_context(seed, 50, 20);
        assert!(
            !rank(&data, &context)
                .selected
                .iter()
                .any(|pick| pick.track.uri == candidate.uri)
        );
        // Duplicate memberships are one edge, not independent corroboration.
        data.playlists
            .push(vec![bridge_b.uri, candidate.uri.clone()]);
        assert!(
            rank(&data, &context)
                .selected
                .iter()
                .any(|pick| pick.track.uri == candidate.uri)
        );
    }

    #[test]
    fn discovery_empty_or_sparse_cache_returns_no_unrelated_padding() {
        let seed = track("spotify:track:seed", "artist-a", "album-seed");
        let unrelated = track("spotify:track:unrelated", "artist-z", "album-unrelated");
        let mut sparse = catalog(&seed, vec![seed.clone(), unrelated.clone()]);
        sparse.playlists = vec![vec![seed.uri.clone()]];
        let mut sparse_context = discovery_context(seed.clone(), 50, 20);
        sparse_context
            .feedback
            .insert(unrelated.uri.clone(), positive_feedback());
        let sparse_report = rank(&sparse, &sparse_context);
        assert!(sparse_report.selected.is_empty());
        assert!(!sparse_report.fallback);
        assert_eq!(sparse_report.candidate_count, 0);
        let sparse_diagnostic = sparse_report
            .candidates
            .iter()
            .find(|candidate| candidate.track_uri == unrelated.uri)
            .unwrap();
        assert!(sparse_diagnostic.excluded);
        assert!(
            sparse_diagnostic
                .reasons
                .iter()
                .any(|reason| reason == "no seed relationship")
        );

        let empty_report = rank(
            &Catalog {
                tracks: Vec::new(),
                playlists: Vec::new(),
                saved: HashSet::new(),
                artist_genres: HashMap::new(),
                familiar_artists: HashSet::new(),
            },
            &discovery_context(seed, 50, 20),
        );
        assert!(empty_report.selected.is_empty());
        assert_eq!(empty_report.candidate_count, 0);
        assert!(!empty_report.fallback);
        assert!(
            empty_report
                .reasons
                .iter()
                .any(|reason| reason.contains("seed-related cache exhausted"))
        );
    }

    #[test]
    fn discovery_scores_are_continuous_around_balanced_midpoint() {
        let seed = track("spotify:track:seed", "artist-seed", "album-seed");
        let familiar = track(
            "spotify:track:familiar",
            "artist-familiar",
            "album-familiar",
        );
        let unfamiliar = track(
            "spotify:track:unfamiliar",
            "artist-unfamiliar",
            "album-unfamiliar",
        );
        let mut data = catalog(
            &seed,
            vec![seed.clone(), familiar.clone(), unfamiliar.clone()],
        );
        data.playlists = vec![vec![
            seed.uri.clone(),
            familiar.uri.clone(),
            unfamiliar.uri.clone(),
        ]];
        let mut feedback = HashMap::new();
        feedback.insert(familiar.uri.clone(), positive_feedback());

        let mut familiar_scores = Vec::new();
        let mut unfamiliar_scores = Vec::new();
        for level in [49, 50, 51] {
            let mut context = discovery_context(seed.clone(), level, 2);
            context.feedback = feedback.clone();
            let report = rank(&data, &context);
            familiar_scores.push(
                report
                    .candidates
                    .iter()
                    .find(|candidate| candidate.track_uri == familiar.uri)
                    .unwrap()
                    .components
                    .discovery,
            );
            unfamiliar_scores.push(
                report
                    .candidates
                    .iter()
                    .find(|candidate| candidate.track_uri == unfamiliar.uri)
                    .unwrap()
                    .components
                    .discovery,
            );
        }
        assert!(familiar_scores[0] > familiar_scores[1]);
        assert!(unfamiliar_scores[2] > unfamiliar_scores[1]);
        assert!((familiar_scores[0] - familiar_scores[1]).abs() < 0.002);
        assert!((unfamiliar_scores[2] - unfamiliar_scores[1]).abs() < 0.002);
    }

    #[test]
    fn legacy_replay_keeps_single_indirect_artist_edge_and_uri_exclusions() {
        let seed = track("spotify:track:legacy-seed", "artist-a", "seed-album");
        let bridge_a = track("spotify:track:bridge-a", "artist-a", "bridge-a-album");
        let bridge_b = track("spotify:track:bridge-b", "artist-b", "bridge-b-album");
        let candidate = track(
            "spotify:track:legacy-candidate",
            "artist-b",
            "candidate-album",
        );
        let data = Catalog {
            tracks: vec![
                seed.clone(),
                bridge_a.clone(),
                bridge_b.clone(),
                candidate.clone(),
            ],
            playlists: vec![vec![bridge_a.uri.clone(), bridge_b.uri.clone()]],
            saved: HashSet::new(),
            artist_genres: HashMap::new(),
            familiar_artists: HashSet::new(),
        };
        let mut ctx = context(seed.clone());
        ctx.queued
            .extend([bridge_a.uri, bridge_b.uri, candidate.id.clone().unwrap()]);
        ctx.limit = 1;
        let report = rank(&data, &ctx);
        assert!(!report.fallback);
        assert_eq!(report.selected[0].track.uri, candidate.uri);
        assert_eq!(report.selected[0].components.playlist_affinity, 1.0);
        assert_eq!(report.selected[0].components.fallback_penalty, 0.0);
        assert!((report.selected[0].score - 0.195).abs() < 1e-12);
        assert!(
            !report.selected[0]
                .reasons
                .iter()
                .any(|reason| reason.contains("corroboration"))
        );

        let mut alternate_seed = seed.clone();
        alternate_seed.uri = "spotify:track:alternate-seed-uri".into();
        let alias_data = catalog(&seed, vec![seed.clone(), alternate_seed.clone()]);
        let alias_report = rank(&alias_data, &context(seed));
        assert_eq!(alias_report.selected[0].track.uri, alternate_seed.uri);
    }

    #[test]
    fn discovery_song_variants_prefer_richer_metadata() {
        let seed = track("spotify:track:seed", "artist-a", "seed-album");
        let mut rich = track("spotify:track:rich-release", "artist-a", "song-album");
        rich.title = "The Same Song".into();
        let mut sparse = rich.clone();
        sparse.uri = "spotify:track:sparse-release".into();
        sparse.id = Some("sparse-release".into());
        sparse.artist_ids.clear();
        sparse.album_id = None;
        let data = catalog(&seed, vec![seed.clone(), sparse, rich.clone()]);
        let report = rank(&data, &discovery_context(seed, 50, 20));
        assert_eq!(report.selected.len(), 1);
        assert_eq!(report.selected[0].track.uri, rich.uri);
        assert_eq!(report.selected[0].track.artist_ids, rich.artist_ids);
    }

    #[test]
    fn discovery_deduplicates_song_variants_and_preserves_distinct_artists_and_versions() {
        let seed = track("spotify:track:seed", "artist-seed", "album-seed");
        let mut first = track("spotify:track:release-a", "artist-a", "album-a");
        first.title = "Girls Girls Girls".into();
        first.artists = vec!["Artist A".into()];
        let mut second = track("spotify:track:release-b", "artist-b", "album-b");
        second.title = " girls   girls girls ".into();
        second.artists = vec!["artist a".into()];
        let mut different_artist = track("spotify:track:other-artist", "artist-c", "album-c");
        different_artist.title = "Girls Girls Girls".into();
        different_artist.artists = vec!["Artist C".into()];
        let mut live = track("spotify:track:live", "artist-a", "album-live");
        live.title = "Girls Girls Girls (Live)".into();
        live.artists = vec!["Artist A".into()];
        let mut data = catalog(
            &seed,
            vec![
                seed.clone(),
                first.clone(),
                second,
                different_artist.clone(),
                live.clone(),
            ],
        );
        data.playlists = vec![vec![
            seed.uri.clone(),
            first.uri.clone(),
            "spotify:track:release-b".into(),
            different_artist.uri.clone(),
            live.uri.clone(),
        ]];

        let report = rank(&data, &discovery_context(seed, 50, 20));
        assert_eq!(report.candidate_count, 3);
        assert_eq!(report.selected.len(), 3);
        let selected = report
            .selected
            .iter()
            .map(|selection| selection.track.uri.as_str())
            .collect::<HashSet<_>>();
        assert!(
            selected.contains(first.uri.as_str()) || selected.contains("spotify:track:release-b")
        );
        assert!(selected.contains(different_artist.uri.as_str()));
        assert!(selected.contains(live.uri.as_str()));
    }

    #[test]
    fn discovery_song_keys_exclude_seed_queue_and_deleted_session_variants() {
        let mut seed = track("spotify:track:seed", "artist-seed", "album-seed");
        seed.title = "Girls Girls Girls".into();
        seed.artists = vec!["Artist A".into()];
        let mut seed_release = track("spotify:track:seed-release", "artist-other", "album-other");
        seed_release.title = " girls girls girls ".into();
        seed_release.artists = vec!["artist a".into()];
        let mut queued = track("spotify:track:queued", "artist-b", "album-b");
        queued.title = "Queued Song".into();
        queued.artists = vec!["Artist B".into()];
        let mut queued_release = track("spotify:track:queued-release", "artist-c", "album-c");
        queued_release.title = " queued   song ".into();
        queued_release.artists = vec!["artist b".into()];
        let mut session = track("spotify:track:session", "artist-d", "album-d");
        session.title = "Session Song".into();
        session.artists = vec!["Artist D".into()];
        let mut session_release = track("spotify:track:session-release", "artist-e", "album-e");
        session_release.title = "SESSION SONG".into();
        session_release.artists = vec!["artist d".into()];
        let data = catalog(
            &seed,
            vec![
                seed.clone(),
                seed_release.clone(),
                queued.clone(),
                queued_release.clone(),
                session_release.clone(),
            ],
        );
        let mut context = discovery_context(seed, 50, 20);
        context.queued.insert(queued.uri);
        context.session_song_keys.insert(song_key(&session));

        let report = rank(&data, &context);
        assert!(report.selected.is_empty());
        for uri in [seed_release.uri, queued_release.uri, session_release.uri] {
            assert!(
                report
                    .candidates
                    .iter()
                    .find(|candidate| candidate.track_uri == uri)
                    .is_some_and(|candidate| candidate.excluded)
            );
        }
    }

    #[test]
    fn discovery_derives_alias_exclusions_from_legacy_session_uris_and_queued_ids() {
        let seed = track("spotify:track:LegacyAliasSeed", "Artist", "SeedAlbum");
        let mut original = track("spotify:track:LegacyOriginal", "Artist", "AlbumOne");
        original.title = "Shared song".into();
        let mut alternate = track("spotify:track:LegacyAlternate", "Artist", "AlbumTwo");
        alternate.title = original.title.clone();
        let data = catalog(&seed, vec![seed.clone(), original.clone(), alternate]);
        let mut context = discovery_context(seed, 50, 20);
        context.session_played.insert(original.uri.clone());
        assert!(rank(&data, &context).selected.is_empty());
        context.session_played.clear();
        context.queued.insert(original.id.unwrap());
        assert!(rank(&data, &context).selected.is_empty());
    }

    #[test]
    fn discovery_unplayed_share_tracks_the_dial() {
        let seed = track("spotify:track:seed", "artist-a", "album-seed");
        let mut played = Vec::new();
        let mut unplayed = Vec::new();
        for index in 0..8 {
            played.push(track(
                &format!("spotify:track:played-{index}"),
                "artist-a",
                &format!("album-played-{index}"),
            ));
            unplayed.push(track(
                &format!("spotify:track:unplayed-{index}"),
                "artist-a",
                &format!("album-unplayed-{index}"),
            ));
        }
        let mut data = catalog(
            &seed,
            std::iter::once(seed.clone())
                .chain(played.iter().cloned())
                .chain(unplayed.iter().cloned())
                .collect(),
        );
        data.playlists = vec![
            std::iter::once(seed.uri.clone())
                .chain(played.iter().map(|track| track.uri.clone()))
                .chain(unplayed.iter().map(|track| track.uri.clone()))
                .collect(),
        ];
        let mut feedback = HashMap::new();
        for candidate in &played {
            feedback.insert(candidate.uri.clone(), positive_feedback());
        }

        let mut shares = Vec::new();
        for level in [0, 50, 100] {
            let mut context = discovery_context(seed.clone(), level, 6);
            context.feedback = feedback.clone();
            let report = rank(&data, &context);
            let discovery = report.discovery.unwrap();
            assert_eq!(discovery.shortfall, 0);
            shares.push(discovery.selected_unplayed);
        }
        assert!(shares[0] < shares[1]);
        assert!(shares[1] < shares[2]);
        assert_eq!(shares, vec![0, 3, 6]);
    }

    #[test]
    fn discovery_cold_start_reports_saved_familiarity_proxy() {
        let seed = track("spotify:track:seed", "artist-a", "album-seed");
        let saved = track("spotify:track:saved", "artist-b", "album-saved");
        let other = track("spotify:track:other", "artist-c", "album-other");
        let mut data = catalog(&seed, vec![seed.clone(), saved.clone(), other]);
        data.saved.insert(saved.uri.clone());
        data.playlists
            .push(vec![seed.uri.clone(), saved.uri.clone()]);
        let report = rank(&data, &discovery_context(seed, 0, 1));
        let discovery = report.discovery.unwrap();
        assert!(discovery.history_sparse);
        assert_eq!(report.selected[0].track.uri, saved.uri);
        assert!(
            discovery
                .reasons
                .iter()
                .any(|reason| reason.contains("saved tracks"))
        );
    }

    #[test]
    fn discovery_keeps_strong_pool_before_unrelated_fallback() {
        let seed = track("spotify:track:seed", "artist-a", "album-seed");
        let related = track("spotify:track:related", "artist-a", "album-related");
        let unrelated = track("spotify:track:unrelated", "artist-z", "album-unrelated");
        let mut data = catalog(&seed, vec![seed.clone(), related.clone(), unrelated]);
        data.playlists
            .push(vec![seed.uri.clone(), related.uri.clone()]);
        let mut context = discovery_context(seed, 100, 1);
        context
            .feedback
            .insert(related.uri.clone(), positive_feedback());
        let report = rank(&data, &context);
        assert_eq!(report.selected[0].track.uri, related.uri);
        assert_eq!(report.discovery.unwrap().selected_unplayed, 0);
    }

    #[test]
    fn discovery_reports_unplayed_shortfall_when_strong_pool_is_sparse() {
        let seed = track("spotify:track:seed", "artist-a", "album-seed");
        let related = track("spotify:track:related", "artist-a", "album-related");
        let first = track("spotify:track:first", "artist-z", "album-first");
        let second = track("spotify:track:second", "artist-y", "album-second");
        let mut data = catalog(&seed, vec![seed.clone(), related.clone(), first, second]);
        data.playlists
            .push(vec![seed.uri.clone(), related.uri.clone()]);
        let mut context = discovery_context(seed, 100, 3);
        context
            .feedback
            .insert(related.uri.clone(), positive_feedback());
        let report = rank(&data, &context);
        let discovery = report.discovery.unwrap();
        assert!(!report.fallback);
        assert_eq!(discovery.target_unplayed, 3);
        assert_eq!(discovery.selected_unplayed, 0);
        assert_eq!(discovery.shortfall, 3);
        assert_eq!(report.selected.len(), 1);
        assert_eq!(report.selected[0].track.uri, related.uri);
        assert!(
            discovery
                .reasons
                .iter()
                .any(|reason| reason.contains("seed-related cache exhausted"))
        );
    }

    #[test]
    fn discovery_seed_artist_choice_overrides_inherited_skip_veto() {
        let seed = track("spotify:track:chosen-seed", "artist-a", "seed-album");
        let history = track("spotify:track:skipped-history", "artist-a", "old-album");
        let fresh = track(
            "spotify:track:chosen-artist-unplayed",
            "artist-a",
            "new-album",
        );
        let mut data = catalog(&seed, vec![seed.clone(), history.clone(), fresh.clone()]);
        for index in 0..10 {
            let candidate = track(
                &format!("spotify:track:indirect-{index}"),
                &format!("other-{index}"),
                "other-album",
            );
            data.playlists
                .push(vec![history.uri.clone(), candidate.uri.clone()]);
            data.playlists.push(vec![
                seed.uri.clone(),
                history.uri.clone(),
                candidate.uri.clone(),
            ]);
            data.tracks.push(candidate);
        }
        let mut context = discovery_context(seed, 50, 1);
        context.feedback.insert(
            history.uri.clone(),
            Feedback {
                completed: 0,
                skips: 3,
                plays: 3,
                listened_ms: 1_000,
                last_played: 1,
            },
        );
        let report = rank(&data, &context);
        let diagnostic = report
            .candidates
            .iter()
            .find(|candidate| candidate.track_uri == fresh.uri)
            .unwrap();
        assert!(diagnostic.components.artist_feedback < 0.0);
        assert_eq!(diagnostic.components.track_feedback, 0.0);
        assert_eq!(report.selected[0].track.uri, fresh.uri);
    }

    #[test]
    fn discovery_does_not_rescue_a_skipped_track() {
        let seed = track("spotify:track:seed", "artist-a", "album-seed");
        let skipped = track("spotify:track:skipped", "artist-b", "album-skipped");
        let fresh = track("spotify:track:fresh", "artist-c", "album-fresh");
        let mut data = catalog(&seed, vec![seed.clone(), skipped.clone(), fresh.clone()]);
        data.playlists.extend([
            vec![seed.uri.clone(), skipped.uri.clone()],
            vec![seed.uri.clone(), fresh.uri.clone()],
        ]);
        let mut context = discovery_context(seed, 0, 1);
        context.feedback.insert(
            skipped.uri.clone(),
            Feedback {
                completed: 0,
                skips: 3,
                plays: 3,
                listened_ms: 2_000,
                last_played: 1,
            },
        );
        let report = rank(&data, &context);
        assert_eq!(report.selected[0].track.uri, fresh.uri);
        let skipped_diagnostic = report
            .candidates
            .iter()
            .find(|candidate| candidate.track_uri == skipped.uri)
            .unwrap();
        assert!(skipped_diagnostic.components.feedback < 0.0);
    }

    #[test]
    fn discovery_skips_negative_artist_candidates_when_neutral_tracks_exist() {
        let seed = track("spotify:track:seed", "artist-a", "album-seed");
        let mut known_history = track("spotify:track:known-history", "artist-b", "album-known");
        known_history.is_local = true;
        let candidate = track(
            "spotify:track:known-unplayed",
            "artist-b",
            "album-candidate",
        );
        let fresh = track("spotify:track:fresh", "artist-c", "album-fresh");
        let mut data = catalog(
            &seed,
            vec![
                seed.clone(),
                known_history.clone(),
                candidate.clone(),
                fresh.clone(),
            ],
        );
        data.playlists.extend([
            vec![seed.uri.clone(), candidate.uri.clone()],
            vec![seed.uri.clone(), fresh.uri.clone()],
        ]);
        let mut context = discovery_context(seed, 100, 1);
        context.feedback.insert(
            known_history.uri.clone(),
            Feedback {
                completed: 0,
                skips: 3,
                plays: 3,
                listened_ms: 1_000,
                last_played: 1,
            },
        );
        let report = rank(&data, &context);
        assert_eq!(report.selected[0].track.uri, fresh.uri);
    }

    #[test]
    fn discovery_none_preserves_legacy_selected_scores() {
        let seed = track("spotify:track:seed", "artist-a", "album-seed");
        let candidate = track("spotify:track:candidate", "artist-b", "album-candidate");
        let mut data = catalog(&seed, vec![seed.clone(), candidate.clone()]);
        data.playlists
            .push(vec![seed.uri.clone(), candidate.uri.clone()]);
        let context = context(seed);
        let direct = rank(&data, &context);
        let mut serialized = serde_json::to_value(&context).unwrap();
        serialized.as_object_mut().unwrap().remove("discovery");
        let loaded: Context = serde_json::from_value(serialized).unwrap();
        let replayed = rank(&data, &loaded);
        assert!(direct.discovery.is_none());
        assert!(replayed.discovery.is_none());
        assert_eq!(
            direct
                .selected
                .iter()
                .map(|selection| (selection.track.uri.clone(), selection.score))
                .collect::<Vec<_>>(),
            replayed
                .selected
                .iter()
                .map(|selection| (selection.track.uri.clone(), selection.score))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn session_played_tracks_are_hard_excluded_at_high_discovery() {
        let seed = track("spotify:track:seed", "artist-a", "album-seed");
        let played = track("spotify:track:played", "artist-a", "album-played");
        let fresh = track("spotify:track:fresh", "artist-b", "album-fresh");
        let mut data = catalog(&seed, vec![seed.clone(), played.clone(), fresh.clone()]);
        data.playlists
            .push(vec![seed.uri.clone(), played.uri.clone()]);
        data.playlists
            .push(vec![seed.uri.clone(), fresh.uri.clone()]);
        let mut context = discovery_context(seed, 100, 1);
        context.session_played.insert(played.uri.clone());
        let report = rank(&data, &context);
        assert_eq!(report.selected[0].track.uri, fresh.uri);
        let diagnostic = report
            .candidates
            .iter()
            .find(|candidate| candidate.track_uri == played.uri)
            .unwrap();
        assert!(diagnostic.excluded);
        assert!(
            diagnostic
                .reasons
                .iter()
                .any(|reason| reason == "already played in this terminal session")
        );
    }

    #[test]
    fn discovery_is_deterministic_across_input_order() {
        let seed = track("spotify:track:seed", "artist-a", "album-seed");
        let first = track("spotify:track:first", "artist-a", "album-first");
        let second = track("spotify:track:second", "artist-a", "album-second");
        let third = track("spotify:track:third", "artist-b", "album-third");
        let mut left = catalog(
            &seed,
            vec![seed.clone(), first.clone(), second.clone(), third.clone()],
        );
        left.playlists = vec![vec![
            seed.uri.clone(),
            first.uri.clone(),
            second.uri.clone(),
        ]];
        let mut right = catalog(
            &seed,
            vec![third.clone(), second, first.clone(), seed.clone()],
        );
        right.playlists = vec![vec![
            seed.uri.clone(),
            "spotify:track:second".into(),
            "spotify:track:first".into(),
        ]];
        let mut left_context = discovery_context(seed.clone(), 50, 2);
        left_context
            .feedback
            .insert(first.uri.clone(), positive_feedback());
        let right_context = left_context.clone();
        let left_report = rank(&left, &left_context);
        let right_report = rank(&right, &right_context);
        let left_selected = left_report
            .selected
            .iter()
            .map(|selection| selection.track.uri.clone())
            .collect::<Vec<_>>();
        let right_selected = right_report
            .selected
            .iter()
            .map(|selection| selection.track.uri.clone())
            .collect::<Vec<_>>();
        assert_eq!(left_selected, right_selected);
        assert_eq!(
            left_report
                .discovery
                .map(|discovery| discovery.selected_unplayed),
            right_report
                .discovery
                .map(|discovery| discovery.selected_unplayed)
        );
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
                familiar_artists: HashSet::new(),
            },
            &benchmark_context,
        );
        eprintln!("10k rank elapsed_us={}", report.elapsed_us);
        assert_eq!(report.selected.len(), MAX_RECOMMENDATIONS);
    }
}
