use std::collections::HashSet;
use std::time::{Duration, Instant};

use super::*;
use crate::config::Config;
use crate::events::EventManager;
use crate::library::Library;
use crate::model::track::Track;
use crate::spotify::{PlayerEvent, Spotify};

fn track(id: usize) -> Track {
    let id = id.to_string();
    Track {
        id: Some(id.clone()),
        uri: format!("spotify:track:test-{id}"),
        title: format!("Test {id}"),
        track_number: 1,
        disc_number: 1,
        duration: 180_000,
        artists: vec!["Test artist".to_string()],
        artist_ids: vec![],
        album: Some("Test album".to_string()),
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

fn playable(id: usize) -> Playable {
    Playable::Track(track(id))
}

fn fixture(tracks: Vec<Playable>, current: Option<usize>) -> (Queue, Spotify) {
    let config = Config::new_for_test();
    let events = EventManager::new_for_test();
    let spotify = Spotify::new_for_test(config.clone(), events.clone());
    let library = Library::new_for_test(events, spotify.clone(), config.clone());
    (
        Queue::new_for_test(tracks, current, spotify.clone(), config, library),
        spotify,
    )
}

fn uri(id: usize) -> String {
    track(id).uri
}

fn radio_batch(start: usize, count: usize) -> Vec<Playable> {
    (start..start + count).map(playable).collect()
}

fn alternate_release(id: usize, uri_id: &str) -> Playable {
    let mut track = track(id);
    track.id = Some(uri_id.to_string());
    track.uri = format!("spotify:track:{uri_id}");
    Playable::Track(track)
}

#[test]
fn stale_radio_generation_cannot_apply_a_batch() {
    let (queue, _) = fixture(vec![playable(0)], Some(0));
    let old = queue.start_radio(&uri(0));
    let current = queue.start_radio(&uri(0));

    assert_ne!(old, current);
    assert_eq!(
        queue.append_radio_next_if_current(old, &uri(0), &[playable(1)]),
        None
    );
    assert_eq!(queue.len(), 1);
    assert_eq!(
        queue.append_radio_next_if_current(current, &uri(0), &[playable(1)]),
        Some(1)
    );
}

#[test]
fn played_uri_survives_queue_edits_and_manual_duplicate_remains_allowed() {
    let (queue, _) = fixture(vec![playable(0), playable(1)], Some(0));
    queue.play(0, false, false);
    assert!(queue.session_played().contains(&uri(0)));

    queue.remove(0);
    queue.clear();
    assert!(queue.session_played().contains(&uri(0)));

    queue.append(playable(0));
    queue.play(0, false, false);
    let generation = queue.start_radio(&uri(0));
    assert_eq!(
        queue.append_radio_next_if_current(generation, &uri(0), &[playable(0), playable(2)],),
        Some(1),
        "radio excludes played URI while a manually queued duplicate remains possible"
    );
    queue.append_next(&[playable(0)]);
    assert_eq!(
        queue
            .queue
            .read()
            .unwrap()
            .iter()
            .filter(|item| item.uri() == uri(0))
            .count(),
        2,
        "the explicit queue action may add a duplicate URI"
    );
}

#[test]
fn player_context_is_parked_without_manual_provenance() {
    let (queue, _) = fixture(vec![playable(0)], Some(0));

    queue.rpc_append_play_many(&[playable(1), playable(2), playable(3)]);
    assert_eq!(queue.get_current_index(), Some(1));
    assert_eq!(queue.queue_origin(1), "context");
    assert_eq!(queue.queue_origin(2), "context");
    assert_eq!(queue.queue_origin(3), "context");
    assert!(queue.queue_provenance().0.is_empty());

    queue.start_radio(&uri(1));
    assert_eq!(queue.parked_context_count(), 2);
    assert_eq!(queue.next_index(), None);
}

#[test]
fn selecting_external_radio_seed_preserves_parked_context() {
    let (queue, _) = fixture(vec![playable(0), playable(1), playable(2)], Some(0));
    queue.play(0, false, false);
    let generation = queue.start_radio(&uri(0));
    assert_eq!(
        queue.append_radio_next_if_current(generation, &uri(0), &[playable(3)]),
        Some(1)
    );

    let selected = track(9);
    let reseeded = queue.rpc_append_play_radio_seed(&selected);
    assert_ne!(reseeded, generation);
    assert_eq!(
        queue.get_current().map(|item| item.uri()),
        Some(selected.uri)
    );
    assert_eq!(queue.queue_origin(4), "explicit");
    assert_eq!(queue.queue_origin(2), "context");

    queue.cancel_radio();
    assert_eq!(queue.next_index(), Some(2));
}

#[test]
fn future_radio_items_are_not_session_played_until_they_play() {
    let (queue, _) = fixture(vec![playable(0)], Some(0));
    let generation = queue.start_radio(&uri(0));
    assert_eq!(
        queue.append_radio_next_if_current(generation, &uri(0), &[playable(1)]),
        Some(1)
    );
    assert!(!queue.session_played().contains(&uri(1)));

    queue.play(1, false, false);
    assert!(queue.session_played().contains(&uri(1)));
}

#[test]
fn low_water_refill_appends_after_manual_tail_and_is_single_flight() {
    let (queue, _) = fixture(vec![playable(0)], Some(0));
    let generation = queue.start_radio(&uri(0));
    assert_eq!(
        queue.append_radio_next_if_current(generation, &uri(0), &radio_batch(1, 20)),
        Some(20)
    );
    queue.append(playable(100));

    for index in 1..=16 {
        queue.play(index, false, false);
    }
    let claim = queue
        .radio_begin_fill(false, 5)
        .expect("five entries remain, so one refill should be claimable");
    assert_eq!(claim.generation, generation);
    assert_eq!(claim.seed.uri, uri(0));
    assert_eq!(claim.current_uri, uri(16));
    assert!(queue.radio_begin_fill(false, 5).is_none());

    assert_eq!(
        queue.append_radio_at_tail_if_current(generation, &uri(16), &radio_batch(200, 20)),
        Some(20)
    );
    assert_eq!(queue.len(), 42);
    assert_eq!(queue.queue.read().unwrap()[21].uri(), uri(100));
    assert_eq!(queue.queue.read().unwrap()[22].uri(), uri(200));
    assert!(queue.radio_finish_fill(generation, 20));
    assert!(queue.radio_retry_delay(generation).is_none());
}

#[test]
fn low_water_counts_automatic_lane_past_explicit_priority() {
    let (queue, _) = fixture(vec![playable(0)], Some(0));
    let generation = queue.start_radio(&uri(0));
    assert_eq!(
        queue.append_radio_at_tail_if_current(generation, &uri(0), &radio_batch(1, 20)),
        Some(20)
    );
    for id in 100..120 {
        queue.append(playable(id));
    }

    assert!(queue.radio_begin_fill(false, 5).is_none());
}

#[test]
fn empty_radio_backoff_is_bounded_and_cancellation_invalidates_retry() {
    let (queue, _) = fixture(vec![playable(0)], Some(0));
    let generation = queue.start_radio(&uri(0));
    let claim = queue
        .radio_begin_fill(true, 5)
        .expect("the initial station fill is forced");
    assert_eq!(claim.generation, generation);
    assert!(queue.radio_finish_fill(generation, 0));
    assert!(queue.radio_waiting());
    let first = queue
        .radio_retry_delay(generation)
        .expect("retry is scheduled");
    assert!(first <= Duration::from_secs(1));
    assert!(queue.radio_begin_fill(false, 5).is_none());

    queue.radio.lock().unwrap().retry_at = Some(Instant::now());
    let _ = queue
        .radio_begin_fill(false, 5)
        .expect("the controlled deadline makes the retry due");
    assert!(queue.radio_finish_fill(generation, 0));
    let second = queue
        .radio_retry_delay(generation)
        .expect("second retry is scheduled");
    assert!(second <= Duration::from_secs(2));
    assert!(queue.radio.lock().unwrap().retry_attempt >= 2);

    queue.cancel_radio();
    assert!(!queue.radio_active());
    assert!(queue.radio_retry_delay(generation).is_none());
    assert!(!queue.radio_finish_fill(generation, 0));
}

#[test]
fn repeat_modes_do_not_replay_seed_or_autogenerated_entries() {
    let (queue, _spotify) = fixture(vec![playable(0), playable(1)], Some(0));
    queue.play(0, false, false);
    queue.start_radio(&uri(0));
    queue.set_repeat(RepeatSetting::RepeatTrack);
    queue.next(false);
    assert_eq!(queue.get_current_index(), Some(0));

    let (queue, spotify) = fixture(vec![playable(0)], Some(0));
    let generation = queue.start_radio(&uri(0));
    queue.append_radio_next_if_current(generation, &uri(0), &[playable(1)]);
    queue.play(0, false, false);
    queue.play(1, false, false);
    queue.set_repeat(RepeatSetting::RepeatTrack);
    queue.next(false);
    assert_eq!(queue.get_current_index(), Some(1));
    assert_eq!(spotify.get_current_status(), PlayerEvent::Stopped);

    let (queue, spotify) = fixture(vec![playable(0)], Some(0));
    let generation = queue.start_radio(&uri(0));
    queue.append_radio_next_if_current(generation, &uri(0), &[playable(1)]);
    queue.play(0, false, false);
    queue.play(1, false, false);
    queue.set_repeat(RepeatSetting::RepeatPlaylist);
    queue.next(false);
    assert_eq!(queue.get_current_index(), Some(1));
    assert_eq!(spotify.get_current_status(), PlayerEvent::Stopped);
}

#[test]
fn shuffle_skips_consumed_entries_but_keeps_new_manual_duplicates() {
    let (queue, _) = fixture(vec![playable(0), playable(1), playable(2)], Some(0));
    queue.play(0, false, false);
    queue.start_radio(&uri(0));
    queue.append(playable(1));
    queue.set_shuffle(true);
    *queue.random_order.write().unwrap() = Some(vec![0, 1, 2, 3]);
    assert_eq!(queue.next_index(), Some(3));

    queue.play(3, false, false);
    queue.cancel_radio();
    assert_eq!(queue.next_index(), Some(1));
}

#[test]
fn adjacent_radio_and_played_markers_survive_insert_remove_and_move() {
    let (queue, _) = fixture((0..7).map(playable).collect(), Some(0));
    *queue.radio_generated.write().unwrap() = HashSet::from([1, 2, 3]);
    *queue.played_indices.write().unwrap() = HashSet::from([1, 2, 3]);

    queue.insert_after_current(playable(20));
    assert_eq!(
        *queue.radio_generated.read().unwrap(),
        HashSet::from([2, 3, 4])
    );
    assert_eq!(
        *queue.played_indices.read().unwrap(),
        HashSet::from([2, 3, 4])
    );

    queue.remove(2);
    assert_eq!(
        *queue.radio_generated.read().unwrap(),
        HashSet::from([2, 3])
    );
    assert_eq!(*queue.played_indices.read().unwrap(), HashSet::from([2, 3]));

    queue.shift(3, 1);
    assert_eq!(
        *queue.radio_generated.read().unwrap(),
        HashSet::from([1, 3])
    );
    assert_eq!(*queue.played_indices.read().unwrap(), HashSet::from([1, 3]));
}

#[test]
fn natural_exhaustion_keeps_station_alive_until_late_refill() {
    let (queue, spotify) = fixture(vec![playable(0)], Some(0));
    let generation = queue.start_radio(&uri(0));
    queue.append_radio_next_if_current(generation, &uri(0), &[playable(1)]);
    queue.play(1, false, false);
    queue.next(false);
    assert!(queue.radio_natural_end());
    assert!(queue.radio_active());
    assert_eq!(spotify.get_current_status(), PlayerEvent::Stopped);

    let claim = queue
        .radio_begin_fill(false, 5)
        .expect("a stopped station still claims its late refill");
    assert_eq!(claim.generation, generation);
    assert_eq!(claim.seed.uri, uri(0));
    assert_eq!(claim.current_uri, uri(1));
    assert_eq!(
        queue.append_radio_at_tail_if_current(generation, &uri(1), &[playable(2)]),
        Some(1)
    );
    assert!(queue.radio_finish_fill(generation, 1));
    assert!(queue.radio_natural_end());
    assert!(queue.resume_radio_if_ready());
    assert!(!queue.radio_natural_end());
    assert_eq!(queue.get_current_index(), Some(2));
}

#[test]
fn queue_end_during_in_flight_fill_resumes_without_replaying_seed() {
    let (queue, spotify) = fixture(vec![playable(0)], Some(0));
    let generation = queue.start_radio(&uri(0));
    assert!(queue.radio_begin_fill(false, 5).is_some());
    spotify.update_status(PlayerEvent::FinishedTrack);
    queue.next(false);
    assert!(queue.radio_natural_end());
    assert_eq!(spotify.get_current_status(), PlayerEvent::FinishedTrack);
    assert_eq!(
        queue.append_radio_at_tail_if_current(generation, &uri(0), &[playable(1)]),
        Some(1)
    );
    assert!(queue.radio_finish_fill(generation, 1));
    assert!(queue.resume_radio_if_ready());
    assert_eq!(queue.get_current_index(), Some(1));
    assert!(!queue.resume_radio_if_ready());
}

#[test]
fn repeat_track_does_not_loop_any_manual_entry_during_radio() {
    let (queue, _) = fixture(vec![playable(0), playable(1), playable(2)], Some(0));
    queue.start_radio(&uri(0));
    queue.append(playable(3));
    queue.play(3, false, false);
    queue.set_repeat(RepeatSetting::RepeatTrack);
    queue.next(false);
    assert_eq!(queue.get_current_index(), Some(3));
}

#[test]
fn moving_queue_entries_preserves_shuffle_identity_and_consumed_markers() {
    let (queue, _) = fixture((0..4).map(playable).collect(), Some(0));
    queue.start_radio(&uri(0));
    queue.play(2, false, false);
    *queue.random_order.write().unwrap() = Some(vec![0, 2, 3, 1]);
    let before = queue
        .in_play_order(0, 4)
        .into_iter()
        .map(|(_, item)| item.uri())
        .collect::<Vec<_>>();
    queue.shift(3, 1);
    assert_eq!(queue.get_current_index(), Some(3));
    assert!(queue.played_indices.read().unwrap().contains(&3));
    assert_eq!(
        before,
        queue
            .in_play_order(0, 4)
            .into_iter()
            .map(|(_, item)| item.uri())
            .collect::<Vec<_>>()
    );
    assert_eq!(queue.next_index(), None);
}

#[test]
fn parked_context_resumes_after_radio_advances_into_raw_tail() {
    let (queue, _) = fixture(vec![playable(0), playable(10), playable(11)], Some(0));
    let generation = queue.start_radio(&uri(0));
    assert_eq!(
        queue.append_radio_at_tail_if_current(generation, &uri(0), &[playable(20), playable(21)]),
        Some(2)
    );
    assert_eq!(
        queue
            .upcoming(8)
            .into_iter()
            .map(|(index, _)| index)
            .collect::<Vec<_>>(),
        vec![3, 4]
    );

    queue.play(3, false, false);
    assert_eq!(queue.next_index(), Some(4));
    queue.play(4, false, false);
    queue.cancel_radio();
    assert_eq!(queue.next_index(), Some(1));
    assert_eq!(queue.queue_origin(1), "context");
}

#[test]
fn toggleplayback_resumes_parked_context_after_stop() {
    let (queue, _) = fixture(vec![playable(0), playable(10)], Some(0));
    let generation = queue.start_radio(&uri(0));
    queue.append_radio_at_tail_if_current(generation, &uri(0), &[playable(20)]);
    queue.play(2, false, false);
    queue.stop();
    assert_eq!(queue.get_current_index(), None);

    queue.toggleplayback();
    assert_eq!(queue.get_current_index(), Some(1));
}

#[test]
fn restored_consumed_context_does_not_resume_canceled_radio_tail() {
    let (queue, spotify) = fixture(vec![playable(0), playable(10), playable(20)], Some(1));
    queue.cfg.with_state_mut(|state| {
        state.queuestate.queue = vec![playable(0), playable(10), playable(20)];
        state.queuestate.current_track = Some(1);
        state.queuestate.radio_generated = vec![2];
        state.queuestate.resume_context_valid = true;
    });
    let bytes = serde_cbor::to_vec(&*queue.cfg.state()).unwrap();
    let restored_state: crate::config::UserState = serde_cbor::from_slice(&bytes).unwrap();
    queue
        .cfg
        .with_state_mut(|state| *state = restored_state.clone());
    let restored = Queue::new(spotify, queue.cfg.clone(), queue.library.clone());
    assert_eq!(restored.len(), 3);
    assert!(restored.queue_resume_context_valid());
    assert_eq!(restored.next_index(), None);
}

#[test]
fn canceled_station_tail_stays_parked_after_context_is_consumed() {
    let (queue, _) = fixture(vec![playable(0), playable(10)], Some(0));
    let generation = queue.start_radio(&uri(0));
    assert_eq!(
        queue.append_radio_at_tail_if_current(generation, &uri(0), &[playable(20), playable(21)]),
        Some(2)
    );

    // A manual choice can consume the saved context while the station is live.
    queue.play(1, false, false);
    queue.cancel_radio();
    assert!(queue.resume_context().is_empty());
    assert_eq!(queue.next_index(), None);
}

#[test]
fn explicit_tail_precedes_radio_and_unplayed_radio_remains_after_manual_choice() {
    let (queue, _) = fixture(vec![playable(0)], Some(0));
    let generation = queue.start_radio(&uri(0));
    queue.append_radio_at_tail_if_current(generation, &uri(0), &[playable(1), playable(2)]);
    queue.append(playable(99));
    assert_eq!(queue.next_index(), Some(3));
    queue.play(3, false, false);
    assert_eq!(queue.next_index(), Some(1));
}

#[test]
fn manual_alternate_release_does_not_replay_matching_radio_occurrence() {
    let (queue, _) = fixture(vec![playable(0)], Some(0));
    let generation = queue.start_radio(&uri(0));
    queue.append_radio_at_tail_if_current(generation, &uri(0), &[playable(1), playable(2)]);
    queue.append(alternate_release(1, "alternate-a"));
    assert_eq!(queue.next_index(), Some(3));
    queue.play(3, false, false);
    assert_eq!(queue.next_index(), Some(2));
}

#[test]
fn reseeding_active_station_preserves_original_parked_context() {
    let (queue, _) = fixture(vec![playable(0), playable(10)], Some(0));
    let generation = queue.start_radio(&uri(0));
    queue.append_radio_at_tail_if_current(generation, &uri(0), &[playable(20)]);
    queue.play(2, false, false);
    queue.start_radio_track(&track(20));
    queue.cancel_radio();
    assert_eq!(queue.next_index(), Some(1));
}

#[test]
fn delayed_fill_rejects_same_index_after_current_occurrence_replacement() {
    let (queue, _) = fixture(vec![playable(0), playable(1)], Some(0));
    let generation = queue.start_radio(&uri(0));
    let claim = queue
        .radio_begin_fill(true, 5)
        .expect("the initial station fill is forced");

    {
        let mut items = queue.queue.write().unwrap();
        let mut replacement = track(0);
        replacement.title = "Replacement metadata".to_string();
        items[0] = Playable::Track(replacement);
    }
    let mut token = queue.current_token.write().unwrap();
    *token = token.wrapping_add(1).max(1);
    drop(token);

    assert_eq!(
        queue.append_radio_at_tail_if_claim_current(&claim, &[playable(2)]),
        None
    );
    assert_eq!(queue.len(), 2);
    assert!(queue.radio_finish_fill(generation, 0));
}
