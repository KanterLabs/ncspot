use std::sync::Arc;
use std::thread;

use log::debug;

use crate::events::EventManager;
use crate::model::playable::Playable;
use crate::model::track::Track;
use crate::queue::Queue;
use crate::ui::osd;

/// Station tracks whose art is fetched up front when a radio starts. The rest
/// of the station is long enough that its art can wait until it is asked for.
const STATION_PREFETCH: usize = 8;
/// How many rate limits a radio waits out before giving up.
const RADIO_RETRIES: usize = 3;

/// Fetch and queue recommendations for `track` without touching playback.
///
/// The caller owns the immediate playback decision. This worker only appends
/// recommendations once its seed is still the item playing, so a slow response
/// cannot add a station behind a later track.
pub(super) fn start(queue: Arc<Queue>, events: EventManager, track: Track) {
    if track.id.is_none() || track.is_local {
        return;
    }

    osd::notify("Starting radio…");

    let seed = track.uri;
    let id = track.id.expect("radio start checked that the id exists");
    let spotify = queue.get_spotify();
    thread::spawn(move || {
        // A rate limit is waited out rather than ending the station before it
        // starts; the seed is already playing, so there is time.
        let mut found = spotify.api.recommendations(None, None, Some(vec![&id]));
        for _ in 0..RADIO_RETRIES {
            let Some(wait) = spotify.api.rate_limit_wait().filter(|_| found.is_err()) else {
                break;
            };
            osd::notify(format!(
                "Radio: Spotify is busy, retrying in {}s",
                wait.as_secs()
            ));
            thread::sleep(wait);
            found = spotify.api.recommendations(None, None, Some(vec![&id]));
        }
        let Ok(found) = found else {
            debug!("no recommendations for {id}");
            osd::notify("Couldn't start the radio: Spotify didn't send any tracks");
            return;
        };

        // Recommendations arrive without an album, and so without cover art;
        // looking them up in full is what gives the whole station artwork
        // rather than just the seed.
        let tracks = spotify.api.hydrate_tracks(&found.tracks);
        let Some(queued) = apply_station(&queue, &seed, &tracks) else {
            return;
        };

        // Only the front of the station is worth warming; the rest will have
        // been fetched long before it is reached.
        prefetch_covers(&tracks[..tracks.len().min(STATION_PREFETCH)]);
        if queued == 0 {
            osd::notify("Radio: Spotify didn't send any similar tracks");
        } else {
            osd::notify(format!("Radio: {queued} similar songs queued"));
        }
        events.trigger();
    });
}

/// Return whether the seed is still the item playing.
fn seed_is_current(queue: &Queue, seed: &str) -> bool {
    queue
        .get_current()
        .is_some_and(|current| current.uri() == seed)
}

/// Filter and append a fetched station when playback has not moved on.
///
/// `None` means the worker lost its race with playback. `Some(count)` means the
/// seed still matches; `count` is the number of recommendations inserted.
fn apply_station(queue: &Queue, seed: &str, tracks: &[Track]) -> Option<usize> {
    if !seed_is_current(queue, seed) {
        return None;
    }

    let station: Vec<Playable> = tracks
        .iter()
        .filter(|track| track.uri != seed)
        .cloned()
        .map(Playable::Track)
        .collect();
    let count = station.len();
    if !station.is_empty() {
        queue.append_next(&station);
    }
    Some(count)
}

/// Warm the cover cache for the first part of a radio station.
pub(super) fn prefetch_covers(tracks: &[Track]) {
    #[cfg(feature = "album_art")]
    crate::ui::album_art::prefetch_covers(
        tracks
            .iter()
            .filter_map(|track| track.cover_url.clone())
            .collect(),
    );
    #[cfg(not(feature = "album_art"))]
    let _ = tracks;
}

#[cfg(test)]
mod tests {
    use super::{apply_station, seed_is_current};
    use crate::config::Config;
    use crate::events::EventManager;
    use crate::library::Library;
    use crate::model::playable::Playable;
    use crate::model::track::Track;
    use crate::queue::Queue;
    use crate::spotify::{PlayerEvent, Spotify};
    use std::time::Duration;

    fn track(id: &str) -> Track {
        Track {
            id: Some(id.to_string()),
            uri: format!("spotify:track:{id}"),
            title: id.to_string(),
            track_number: 1,
            disc_number: 1,
            duration: 1_000,
            artists: vec!["Artist".to_string()],
            artist_ids: vec![],
            album: Some("Album".to_string()),
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

    fn queue(tracks: Vec<Track>, current: usize) -> (Queue, Spotify) {
        let config = Config::new_for_test();
        let events = EventManager::new_for_test();
        let spotify = Spotify::new_for_test(config.clone(), events.clone());
        let library = Library::new_for_test(events, spotify.clone(), config.clone());
        let queue = Queue::new_for_test(
            tracks.into_iter().map(Playable::Track).collect(),
            Some(current),
            spotify.clone(),
            config,
            library,
        );
        (queue, spotify)
    }

    #[test]
    fn delayed_results_are_rejected_after_playback_moves() {
        let seed = track("seed");
        let (queue, _) = queue(vec![seed.clone(), track("other")], 1);

        assert!(!seed_is_current(&queue, &seed.uri));
        assert_eq!(
            apply_station(&queue, &seed.uri, &[track("recommendation")]),
            None
        );
        assert_eq!(queue.len(), 2);
        assert_eq!(queue.get_current_index(), Some(1));
    }

    #[test]
    fn station_is_inserted_after_the_seed_without_restarting_playback() {
        let seed = track("seed");
        let (queue, spotify) = queue(vec![seed.clone(), track("existing")], 0);
        let paused = Duration::from_secs(62);
        spotify.update_status(PlayerEvent::Paused(paused));

        assert!(seed_is_current(&queue, &seed.uri));
        assert_eq!(
            apply_station(&queue, &seed.uri, &[seed.clone(), track("recommendation")],),
            Some(1)
        );

        {
            let items = queue.queue.read().unwrap();
            assert_eq!(items.len(), 3);
            assert_eq!(items[1].id(), Some("recommendation".to_string()));
        }
        assert_eq!(queue.get_current_index(), Some(0));
        assert_eq!(queue.get_current().map(|item| item.uri()), Some(seed.uri));
        assert_eq!(spotify.get_current_status(), PlayerEvent::Paused(paused));
        assert_eq!(spotify.get_current_progress(), paused);
    }
}
