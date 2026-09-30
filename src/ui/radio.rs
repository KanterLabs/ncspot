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
/// Build a local station immediately from prepared metadata. Catalog enrichment
/// happens separately and cannot hold up a cached station.
pub(crate) fn start(
    queue: Arc<Queue>,
    library: Arc<crate::library::Library>,
    events: EventManager,
    track: Track,
) {
    if track.id.is_none() || track.is_local {
        return;
    }
    let run = generation().fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
    osd::notify("Starting local radio…");
    thread::spawn(move || {
        let rng_seed = rand::random::<u64>();
        let count = fill(&queue, &library, &track, run, rng_seed);
        if count == Some(0) {
            osd::notify("Preparing radio metadata in the background…");
            let catalog = crate::recommendations::catalog(&queue, &library);
            let related = crate::recommendations::related_artists(&catalog, &track);
            let retry_queue = queue.clone();
            let retry_library = library.clone();
            let retry_events = events.clone();
            crate::recommendations::enrichment::shared().refresh_with_callback(
                queue.get_spotify(),
                track.clone(),
                related,
                events.clone(),
                move || {
                    if fill(&retry_queue, &retry_library, &track, run, rng_seed) == Some(0) {
                        osd::notify(
                            "No radio candidates available yet; see :radio-debug for details",
                        );
                    }
                    retry_events.try_trigger();
                },
            );
        } else {
            crate::recommendations::prewarm(queue, library, events.clone());
        }
        events.try_trigger();
    });
}

fn fill(
    queue: &Queue,
    library: &crate::library::Library,
    track: &Track,
    run: u64,
    rng_seed: u64,
) -> Option<usize> {
    if generation_value() != run {
        return None;
    }
    let (mut diagnostic, replay) =
        crate::recommendations::recommend(queue, library, track.clone(), rng_seed, 20);
    let tracks: Vec<_> = diagnostic
        .report
        .selected
        .iter()
        .map(|selection| selection.track.clone())
        .collect();
    if generation_value() != run {
        return None;
    }
    let applied = apply_station(queue, &track.uri, &tracks);
    diagnostic.applied = applied.is_some_and(|count| count > 0);
    if let Some(count) = applied {
        if count > 0 {
            osd::notify(format!("Local radio: {count} songs queued"));
            prefetch_covers(&tracks[..tracks.len().min(STATION_PREFETCH)]);
        }
    } else {
        debug!("radio: discarded because playback changed");
    }
    crate::recommendations::remember(diagnostic, replay);
    applied
}

fn generation() -> &'static std::sync::atomic::AtomicU64 {
    static GENERATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    &GENERATION
}

fn generation_value() -> u64 {
    generation().load(std::sync::atomic::Ordering::SeqCst)
}

#[cfg(test)]
fn seed_is_current(queue: &Queue, seed: &str) -> bool {
    queue
        .get_current()
        .is_some_and(|current| current.uri() == seed)
}

fn apply_station(queue: &Queue, seed: &str, tracks: &[Track]) -> Option<usize> {
    let station: Vec<_> = tracks
        .iter()
        .filter(|track| track.uri != seed)
        .cloned()
        .map(Playable::Track)
        .collect();
    queue.append_next_if_current(seed, &station)
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
    #[test]
    fn station_deduplicates_queue_and_result_atomically() {
        let seed = track("seed");
        let (queue, _) = queue(vec![seed.clone(), track("existing")], 0);
        assert_eq!(
            apply_station(
                &queue,
                &seed.uri,
                &[track("existing"), track("new"), track("new")]
            ),
            Some(1)
        );
        assert_eq!(queue.len(), 3);
        assert_eq!(queue.queue.read().unwrap()[1].id(), Some("new".into()));
        assert_eq!(
            apply_station(&queue, "spotify:track:stale", &[track("later")]),
            None
        );
        assert_eq!(queue.len(), 3);
    }
}
