use std::collections::{BTreeMap, HashMap, VecDeque};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock, mpsc};
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

#[cfg(not(test))]
use crate::config;
use crate::model::track::Track;

const CACHE_FILE: &str = "radio-history.json";
const FILE_VERSION: u16 = 1;
const MAX_ENTRIES: usize = 5_000;
const MAX_RECENT: usize = 20;

static TEMP_FILE_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Aggregate feedback for one Spotify URI.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Feedback {
    pub completed: u32,
    pub skips: u32,
    pub plays: u32,
    pub listened_ms: u64,
    pub last_played: u64,
}

/// The result of a playback event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Completed,
    EarlySkip,
    Neutral,
}

#[derive(Clone, Debug)]
struct Entry {
    feedback: Feedback,
    artist_ids: Vec<String>,
    artist_names: Vec<String>,
    order: u64,
}

#[derive(Debug, Default)]
struct State {
    entries: HashMap<String, Entry>,
    recent: VecDeque<String>,
    order: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct StoredEntry {
    #[serde(flatten)]
    feedback: Feedback,
    #[serde(default)]
    artist_ids: Vec<String>,
    #[serde(default)]
    artist_names: Vec<String>,
    #[serde(default)]
    order: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Stored {
    version: u16,
    entries: BTreeMap<String, StoredEntry>,
    #[serde(default)]
    recent: Vec<String>,
}

#[derive(Debug)]
struct SaveJob {
    order: u64,
    completion: Option<mpsc::Sender<()>>,
}

#[derive(Clone, Debug)]
enum Health {
    Ready,
    Disabled(String),
}

struct Control {
    health: Health,
    sender: Option<mpsc::Sender<SaveJob>>,
}

struct Inner {
    state: RwLock<State>,
    control: Mutex<Control>,
    path: Option<PathBuf>,
}

/// Playback history used to seed local radio recommendations.
#[derive(Clone)]
pub struct History {
    inner: Arc<Inner>,
}

/// Return the process-wide playback history.
pub fn shared() -> Arc<History> {
    // Unit tests get an independent memory-only store. This both keeps tests isolated and makes
    // it impossible for a test process to alter the user's radio history.
    #[cfg(test)]
    {
        Arc::new(History::memory_only())
    }

    #[cfg(not(test))]
    {
        static SHARED: std::sync::OnceLock<Arc<History>> = std::sync::OnceLock::new();
        SHARED
            .get_or_init(|| Arc::new(History::load_from_disk()))
            .clone()
    }
}

impl History {
    /// Return feedback keyed by Spotify URI.
    pub fn snapshot(&self) -> HashMap<String, Feedback> {
        self.inner
            .state
            .read()
            .unwrap()
            .entries
            .iter()
            .map(|(uri, entry)| (uri.clone(), entry.feedback.clone()))
            .collect()
    }

    /// Return the most recently recorded unique URIs, newest first.
    pub fn recent(&self) -> Vec<String> {
        self.inner
            .state
            .read()
            .unwrap()
            .recent
            .iter()
            .cloned()
            .collect()
    }

    /// Record one playback event and queue an ordered background save.
    pub fn record(&self, track: &Track, outcome: Outcome, listened_ms: u64) {
        let uri = track.uri.trim();
        if uri.is_empty() {
            return;
        }

        // Keep the state lock held until the save job is enqueued. That makes the order in which
        // jobs enter the writer match the order in which state generations were created, even if
        // playback callbacks arrive from more than one thread.
        let mut state = self.inner.state.write().unwrap();
        state.order = state.order.saturating_add(1);
        let order = state.order;
        let entry = state
            .entries
            .entry(uri.to_owned())
            .or_insert_with(|| Entry {
                feedback: Feedback::default(),
                artist_ids: Vec::new(),
                artist_names: Vec::new(),
                order,
            });

        entry.feedback.plays = entry.feedback.plays.saturating_add(1);
        match outcome {
            Outcome::Completed => {
                entry.feedback.completed = entry.feedback.completed.saturating_add(1)
            }
            Outcome::EarlySkip => entry.feedback.skips = entry.feedback.skips.saturating_add(1),
            Outcome::Neutral => {}
        }
        entry.feedback.listened_ms = entry.feedback.listened_ms.saturating_add(listened_ms);
        entry.feedback.last_played = now_seconds();
        entry.order = order;
        if !track.artist_ids.is_empty() {
            entry.artist_ids.clone_from(&track.artist_ids);
        }
        if !track.artists.is_empty() {
            entry.artist_names.clone_from(&track.artists);
        }

        state.recent.retain(|recent| recent != uri);
        state.recent.push_front(uri.to_owned());
        while state.recent.len() > MAX_RECENT {
            state.recent.pop_back();
        }
        trim(&mut state);

        let mut control = self.inner.control.lock().unwrap();
        if !matches!(&control.health, Health::Ready) {
            return;
        }
        let Some(sender) = control.sender.as_ref() else {
            return;
        };
        if sender
            .send(SaveJob {
                order,
                completion: None,
            })
            .is_err()
        {
            control.health = Health::Disabled(
                "radio history save worker stopped; saves are disabled until restart".to_owned(),
            );
            control.sender = None;
        }
    }

    /// Describe the load/save state of the history file.
    pub fn status(&self) -> String {
        match &self.inner.control.lock().unwrap().health {
            Health::Ready => "ready".to_owned(),
            Health::Disabled(reason) => reason.clone(),
        }
    }

    /// Wait until all save jobs queued so far have either completed or failed.
    pub fn flush(&self) {
        let (sender, receiver) = mpsc::channel();
        let state = self.inner.state.read().unwrap();
        let order = state.order;
        let mut control = self.inner.control.lock().unwrap();
        if !matches!(&control.health, Health::Ready) {
            return;
        }
        let Some(save_sender) = control.sender.as_ref() else {
            return;
        };
        if save_sender
            .send(SaveJob {
                order,
                completion: Some(sender),
            })
            .is_err()
        {
            control.health = Health::Disabled(
                "radio history save worker stopped; saves are disabled until restart".to_owned(),
            );
            control.sender = None;
            return;
        }
        drop(control);
        drop(state);
        let _ = receiver.recv();
    }

    fn memory_only() -> Self {
        Self::from_parts(State::default(), Health::Ready, None, false)
    }

    #[cfg(not(test))]
    fn load_from_disk() -> Self {
        let path = config::cache_path(CACHE_FILE);
        match load_file(&path) {
            Ok(Some(state)) => Self::from_parts(state, Health::Ready, Some(path), true),
            Ok(None) => Self::from_parts(State::default(), Health::Ready, Some(path), true),
            Err(reason) => Self::from_parts(
                State::default(),
                Health::Disabled(reason),
                Some(path),
                false,
            ),
        }
    }

    fn from_parts(state: State, health: Health, path: Option<PathBuf>, persistence: bool) -> Self {
        let history = Self {
            inner: Arc::new(Inner {
                state: RwLock::new(state),
                control: Mutex::new(Control {
                    health,
                    sender: None,
                }),
                path,
            }),
        };

        if persistence {
            let (sender, receiver) = mpsc::channel();
            let worker_inner = Arc::clone(&history.inner);
            match thread::Builder::new()
                .name("resonance-radio-history".to_owned())
                .spawn(move || writer_loop(worker_inner, receiver))
            {
                Ok(_) => history.inner.control.lock().unwrap().sender = Some(sender),
                Err(error) => {
                    history.inner.control.lock().unwrap().health = Health::Disabled(format!(
                        "could not start radio history save worker: {error}; saves are disabled"
                    ));
                }
            }
        }

        history
    }

    #[cfg(test)]
    fn load_for_test(path: &Path) -> Self {
        match load_file(path) {
            Ok(Some(state)) => Self::from_parts(state, Health::Ready, Some(path.to_owned()), false),
            Ok(None) => Self::from_parts(
                State::default(),
                Health::Ready,
                Some(path.to_owned()),
                false,
            ),
            Err(reason) => Self::from_parts(
                State::default(),
                Health::Disabled(reason),
                Some(path.to_owned()),
                false,
            ),
        }
    }

    #[cfg(test)]
    fn save_for_test(&self, path: &Path) -> Result<(), String> {
        let state = self.inner.state.read().unwrap();
        save_file(path, &Stored::from_state(&state))
    }
}

impl Default for History {
    fn default() -> Self {
        Self::memory_only()
    }
}

impl Stored {
    fn from_state(state: &State) -> Self {
        let entries = state
            .entries
            .iter()
            .map(|(uri, entry)| {
                (
                    uri.clone(),
                    StoredEntry {
                        feedback: entry.feedback.clone(),
                        artist_ids: entry.artist_ids.clone(),
                        artist_names: entry.artist_names.clone(),
                        order: entry.order,
                    },
                )
            })
            .collect();
        Self {
            version: FILE_VERSION,
            entries,
            recent: state.recent.iter().cloned().collect(),
        }
    }

    fn into_state(self) -> State {
        let mut state = State::default();
        let mut next_order = self
            .entries
            .values()
            .map(|entry| entry.order)
            .max()
            .unwrap_or(0);

        for (uri, entry) in self.entries {
            let order = if entry.order == 0 {
                next_order = next_order.saturating_add(1);
                next_order
            } else {
                entry.order
            };
            state.order = state.order.max(order);
            state.entries.insert(
                uri,
                Entry {
                    feedback: entry.feedback,
                    artist_ids: entry.artist_ids,
                    artist_names: entry.artist_names,
                    order,
                },
            );
        }

        for uri in self.recent {
            if state.entries.contains_key(&uri)
                && !uri.is_empty()
                && !state.recent.iter().any(|recent| recent == &uri)
            {
                state.recent.push_back(uri);
                if state.recent.len() == MAX_RECENT {
                    break;
                }
            }
        }
        trim(&mut state);
        state
    }
}

fn trim(state: &mut State) {
    while state.entries.len() > MAX_ENTRIES {
        let Some(oldest) = state
            .entries
            .iter()
            .min_by(|(left_uri, left), (right_uri, right)| {
                left.order
                    .cmp(&right.order)
                    .then_with(|| left_uri.cmp(right_uri))
            })
            .map(|(uri, _)| uri.clone())
        else {
            return;
        };
        state.entries.remove(&oldest);
        state.recent.retain(|uri| uri != &oldest);
    }
}

fn now_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

fn writer_loop(inner: Arc<Inner>, receiver: mpsc::Receiver<SaveJob>) {
    let mut written_order = 0;
    while let Ok(mut job) = receiver.recv() {
        let mut completed = Vec::new();
        if let Some(completion) = job.completion.take() {
            completed.push(completion);
        }
        while let Ok(mut next) = receiver.try_recv() {
            if let Some(completion) = next.completion.take() {
                completed.push(completion);
            }
            if next.order >= job.order {
                job = next;
            }
        }
        if job.order <= written_order {
            for completion in completed {
                let _ = completion.send(());
            }
            continue;
        }

        let stored = {
            let state = inner.state.read().unwrap();
            Stored::from_state(&state)
        };
        let path = inner
            .path
            .as_deref()
            .expect("persistent history has a path");
        match save_file(path, &stored) {
            Ok(()) => {
                written_order = job.order;
                for completion in completed {
                    let _ = completion.send(());
                }
            }
            Err(error) => {
                for completion in completed {
                    let _ = completion.send(());
                }
                let mut control = inner.control.lock().unwrap();
                control.health =
                    Health::Disabled(format!("{error}; saves are disabled until restart"));
                control.sender = None;
                break;
            }
        }
    }
}

fn load_file(path: &Path) -> Result<Option<State>, String> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(format!(
                "could not read radio history at {}: {error}; saves are disabled and the file was preserved",
                path.display()
            ));
        }
    };

    let value: serde_json::Value = serde_json::from_slice(&bytes).map_err(|error| {
        format!(
            "could not parse radio history at {}: {error}; saves are disabled and the file was preserved",
            path.display()
        )
    })?;
    let version = value
        .get("version")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| {
            format!(
                "radio history at {} is corrupt (missing or invalid version); saves are disabled and the file was preserved",
                path.display()
            )
        })?;
    if version != u64::from(FILE_VERSION) {
        return Err(format!(
            "radio history at {} has unsupported version {version}; saves are disabled and the file was preserved",
            path.display()
        ));
    }

    let stored: Stored = serde_json::from_value(value).map_err(|error| {
        format!(
            "radio history at {} is corrupt: {error}; saves are disabled and the file was preserved",
            path.display()
        )
    })?;
    Ok(Some(stored.into_state()))
}

fn save_file(path: &Path, stored: &Stored) -> Result<(), String> {
    let data = serde_json::to_vec_pretty(stored).map_err(|error| {
        format!(
            "could not serialize radio history for {}: {error}",
            path.display()
        )
    })?;
    let parent = path.parent().ok_or_else(|| {
        format!(
            "could not save radio history at {}: path has no parent directory",
            path.display()
        )
    })?;
    fs::create_dir_all(parent).map_err(|error| {
        format!(
            "could not create radio history directory {}: {error}",
            parent.display()
        )
    })?;

    let filename = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            format!(
                "could not save radio history at {}: path has no valid filename",
                path.display()
            )
        })?;
    let suffix = TEMP_FILE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let temp = parent.join(format!(
        ".{filename}.tmp-{}-{timestamp}-{suffix}",
        std::process::id()
    ));

    let write_result = (|| -> Result<(), String> {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temp)
            .map_err(|error| {
                format!(
                    "could not create temporary radio history file {}: {error}",
                    temp.display()
                )
            })?;
        file.write_all(&data).map_err(|error| {
            format!(
                "could not write temporary radio history file {}: {error}",
                temp.display()
            )
        })?;
        file.flush().map_err(|error| {
            format!(
                "could not flush temporary radio history file {}: {error}",
                temp.display()
            )
        })?;
        file.sync_all().map_err(|error| {
            format!(
                "could not sync temporary radio history file {}: {error}",
                temp.display()
            )
        })?;
        drop(file);
        fs::rename(&temp, path).map_err(|error| {
            format!(
                "could not atomically replace radio history {} with {}: {error}",
                path.display(),
                temp.display()
            )
        })?;
        Ok(())
    })();

    if write_result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    write_result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::atomic::AtomicU64;

    static TEST_COUNTER: AtomicU64 = AtomicU64::new(0);

    fn temp_path(name: &str) -> (PathBuf, PathBuf) {
        let id = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "resonance-radio-history-test-{}-{id}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).unwrap();
        (dir.clone(), dir.join(name))
    }

    fn track(uri: &str, title: &str) -> Track {
        Track {
            id: Some(title.to_owned()),
            uri: uri.to_owned(),
            title: title.to_owned(),
            track_number: 1,
            disc_number: 1,
            duration: 180_000,
            artists: vec!["Artist".to_owned()],
            artist_ids: vec!["artist-id".to_owned()],
            album: Some("Album".to_owned()),
            album_id: Some("album-id".to_owned()),
            album_artists: vec!["Artist".to_owned()],
            cover_url: None,
            url: String::new(),
            added_at: None,
            list_index: 0,
            is_local: false,
            is_playable: Some(true),
        }
    }

    fn cleanup(dir: PathBuf) {
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn roundtrip_preserves_feedback_and_recent_order() {
        let (dir, path) = temp_path(CACHE_FILE);
        let history = History::memory_only();
        history.record(&track("spotify:track:a", "a"), Outcome::Completed, 100);
        history.record(&track("spotify:track:b", "b"), Outcome::EarlySkip, 200);
        history.record(&track("spotify:track:a", "a"), Outcome::Neutral, 300);
        history.save_for_test(&path).unwrap();

        let reloaded = History::load_for_test(&path);
        assert_eq!(reloaded.snapshot(), history.snapshot());
        assert_eq!(reloaded.recent(), history.recent());
        assert_eq!(
            reloaded.recent(),
            vec!["spotify:track:a", "spotify:track:b"]
        );
        assert_eq!(reloaded.status(), "ready");
        cleanup(dir);
    }

    #[test]
    fn completed_and_early_skip_have_separate_counts() {
        let history = History::memory_only();
        let item = track("spotify:track:a", "a");
        history.record(&item, Outcome::Completed, 10);
        history.record(&item, Outcome::EarlySkip, 20);
        history.record(&item, Outcome::Neutral, 30);
        let last_played = history.snapshot()["spotify:track:a"].last_played;
        assert_eq!(
            history.snapshot()["spotify:track:a"],
            Feedback {
                completed: 1,
                skips: 1,
                plays: 3,
                listened_ms: 60,
                last_played,
            }
        );
    }

    #[test]
    fn empty_uri_is_ignored() {
        let history = History::memory_only();
        history.record(&track("  ", "empty"), Outcome::Neutral, 50);
        assert!(history.snapshot().is_empty());
        assert!(history.recent().is_empty());
    }

    #[test]
    fn entries_and_recent_are_bounded() {
        let history = History::memory_only();
        for index in 0..=MAX_ENTRIES {
            let uri = format!("spotify:track:{index}");
            history.record(&track(&uri, &index.to_string()), Outcome::Neutral, 1);
        }
        let snapshot = history.snapshot();
        assert_eq!(snapshot.len(), MAX_ENTRIES);
        assert_eq!(history.recent().len(), MAX_RECENT);
        assert!(!snapshot.contains_key("spotify:track:0"));
        assert_eq!(
            history.recent().first().map(String::as_str),
            Some("spotify:track:5000")
        );
    }

    #[test]
    fn unsupported_version_is_preserved_and_disables_saves() {
        let (dir, path) = temp_path(CACHE_FILE);
        let original = br#"{"version":999,"entries":{},"recent":[]}"#;
        fs::write(&path, original).unwrap();
        let history = History::load_for_test(&path);
        history.record(&track("spotify:track:a", "a"), Outcome::Neutral, 1);
        assert!(history.status().contains("unsupported version"));
        assert_eq!(fs::read(&path).unwrap(), original);
        cleanup(dir);
    }

    #[test]
    fn corrupt_file_is_preserved_and_disables_saves() {
        let (dir, path) = temp_path(CACHE_FILE);
        let original = b"not json";
        fs::write(&path, original).unwrap();
        let history = History::load_for_test(&path);
        history.record(&track("spotify:track:a", "a"), Outcome::Neutral, 1);
        assert!(history.status().contains("could not parse"));
        assert_eq!(fs::read(&path).unwrap(), original);
        cleanup(dir);
    }

    #[test]
    fn memory_shared_store_does_not_touch_disk() {
        let history = shared();
        history.record(&track("spotify:track:a", "a"), Outcome::Neutral, 1);
        assert_eq!(history.snapshot().len(), 1);
        assert_eq!(history.status(), "ready");
    }

    #[test]
    fn save_failures_include_the_target_path() {
        let (dir, parent) = temp_path("parent");
        fs::write(&parent, b"file").unwrap();
        let path = parent.join(CACHE_FILE);
        let history = History::memory_only();
        let error = history.save_for_test(&path).unwrap_err();
        assert!(error.contains("radio history") || error.contains("directory"));
        assert!(error.contains(&parent.display().to_string()));
        cleanup(dir);
    }
}
