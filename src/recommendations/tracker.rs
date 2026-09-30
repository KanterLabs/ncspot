//! Measure listening time, rather than seek position, for local feedback.
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::history::{History, Outcome};
use crate::model::track::Track;

struct Session {
    track: Track,
    listened: Duration,
    updated: Instant,
    playing: bool,
}

pub struct Tracker {
    history: Arc<History>,
    active: Option<Session>,
}

impl Tracker {
    pub fn new(history: Arc<History>) -> Self {
        Self {
            history,
            active: None,
        }
    }

    pub fn observe(&mut self, track: Option<Track>, playing: bool) {
        self.observe_at(track, playing, Instant::now());
    }

    fn observe_at(&mut self, track: Option<Track>, playing: bool, at: Instant) {
        if self.active.as_ref().map(|session| &session.track.uri)
            != track.as_ref().map(|track| &track.uri)
        {
            self.finish_at(Outcome::Neutral, at);
            self.active = track
                .filter(|track| !track.is_local && track.id.is_some())
                .map(|track| Session {
                    track,
                    listened: Duration::ZERO,
                    updated: at,
                    playing,
                });
        }
        if let Some(session) = &mut self.active {
            if session.playing {
                session.listened += at.saturating_duration_since(session.updated);
            }
            session.updated = at;
            session.playing = playing;
        }
    }

    pub fn finish(&mut self, manual_next: bool, finished: bool) {
        let outcome = match (manual_next, finished) {
            (true, _) => Outcome::EarlySkip,
            (_, true) => Outcome::Completed,
            _ => Outcome::Neutral,
        };
        self.finish_at(outcome, Instant::now());
    }

    fn finish_at(&mut self, requested: Outcome, at: Instant) {
        let Some(mut session) = self.active.take() else {
            return;
        };
        if session.playing {
            session.listened += at.saturating_duration_since(session.updated);
        }
        let listened_ms = session.listened.as_millis().min(u64::MAX as u128) as u64;
        // Failed loads and brief transport transitions are not listening events.
        if listened_ms < 2_000 {
            return;
        }
        let duration = session.track.duration as u64;
        let outcome = match requested {
            Outcome::EarlySkip
                if listened_ms < 30_000 && duration > 0 && listened_ms * 4 < duration =>
            {
                Outcome::EarlySkip
            }
            Outcome::Completed if duration > 0 && listened_ms >= duration * 4 / 5 => {
                Outcome::Completed
            }
            // A late manual next still indicates a song the listener stayed for.
            Outcome::EarlySkip if duration > 0 && listened_ms >= duration * 4 / 5 => {
                Outcome::Completed
            }
            _ => Outcome::Neutral,
        };
        self.history.record(&session.track, outcome, listened_ms);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track() -> Track {
        serde_json::from_value(serde_json::json!({
            "id":"seed", "uri":"spotify:track:seed", "title":"Seed", "track_number":1,
            "disc_number":1, "duration":180000, "artists":["Artist"], "artist_ids":["artist"],
            "album":"Album", "album_id":"album", "album_artists":[], "cover_url":null,
            "url":"", "added_at":null,"list_index":0,"is_local":false,"is_playable":true
        }))
        .unwrap()
    }

    #[test]
    fn pauses_and_seeks_do_not_create_skips_or_fake_listening() {
        let history = super::super::history::shared();
        let mut tracker = Tracker::new(history.clone());
        let at = Instant::now();
        tracker.observe_at(Some(track()), true, at);
        tracker.observe_at(Some(track()), false, at + Duration::from_secs(10));
        tracker.observe_at(Some(track()), false, at + Duration::from_secs(200));
        tracker.finish_at(Outcome::Neutral, at + Duration::from_secs(201));
        let snapshot = history.snapshot();
        let feedback = &snapshot["spotify:track:seed"];
        assert_eq!(feedback.listened_ms, 10_000);
        assert_eq!(feedback.skips, 0);
        assert_eq!(feedback.completed, 0);
    }

    #[test]
    fn deliberate_early_next_is_negative_but_failed_load_is_not() {
        let history = super::super::history::shared();
        let mut tracker = Tracker::new(history.clone());
        let at = Instant::now();
        tracker.observe_at(Some(track()), false, at);
        tracker.finish_at(Outcome::Completed, at + Duration::from_secs(180));
        assert!(history.snapshot().is_empty());
        tracker.observe_at(Some(track()), true, at);
        tracker.finish_at(Outcome::EarlySkip, at + Duration::from_secs(10));
        assert_eq!(history.snapshot()["spotify:track:seed"].skips, 1);
    }

    #[test]
    fn finished_tracks_and_repeats_are_counted_once_per_listen() {
        let history = super::super::history::shared();
        let mut tracker = Tracker::new(history.clone());
        let at = Instant::now();
        for n in 0..2 {
            let start = at + Duration::from_secs(n * 180);
            tracker.observe_at(Some(track()), true, start);
            tracker.finish_at(Outcome::Completed, start + Duration::from_secs(180));
        }
        tracker.finish_at(Outcome::Completed, at + Duration::from_secs(400));
        let snapshot = history.snapshot();
        assert_eq!(snapshot["spotify:track:seed"].completed, 2);
        assert_eq!(snapshot["spotify:track:seed"].plays, 2);
    }
}
