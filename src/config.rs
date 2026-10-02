use std::collections::HashMap;
use std::error::Error;
use std::path::PathBuf;
use std::sync::{RwLock, RwLockReadGuard};
use std::{fs, process};

use cursive::theme::Theme;
use log::{debug, error};
use ncspot::{CONFIGURATION_FILE_NAME, USER_STATE_FILE_NAME};
use platform_dirs::AppDirs;
use std::io::Write;

use crate::command::{SortDirection, SortKey};
use crate::model::playable::Playable;
use crate::queue;
use crate::serialization::{CBOR, Serializer, TOML};

pub const CACHE_VERSION: u16 = 1;
pub const DEFAULT_COMMAND_KEY: char = ':';

const DEFAULT_RADIO_DISCOVERY: u8 = 50;
const PRE_DISCOVERY_USER_STATE_FILE_NAME: &str = "userstate.pre-discovery.cbor";

fn default_radio_discovery() -> u8 {
    DEFAULT_RADIO_DISCOVERY
}

fn clamp_radio_discovery(value: u8) -> u8 {
    value.min(100)
}

/// The playback state when ncspot is started.
#[derive(Clone, Serialize, Deserialize, Debug, PartialEq, Eq)]
pub enum PlaybackState {
    Playing,
    Paused,
    Stopped,
    Default,
}

/// The focussed library tab when ncspot is started.
#[derive(Clone, Serialize, Deserialize, Debug, Hash, strum_macros::EnumIter)]
#[serde(rename_all = "lowercase")]
pub enum LibraryTab {
    Tracks,
    Albums,
    Artists,
    Playlists,
    Podcasts,
    Browse,
}

/// The format used to represent tracks in a list.
#[derive(Serialize, Deserialize, Debug, Default, Clone)]
pub struct TrackFormat {
    pub left: Option<String>,
    pub center: Option<String>,
    pub right: Option<String>,
}

impl TrackFormat {
    pub fn default() -> Self {
        Self {
            left: Some(String::from("%artists - %title")),
            center: Some(String::from("%album")),
            right: Some(String::from("%saved %duration")),
        }
    }
}

/// The format used when sending desktop notifications about playback status.
#[derive(Serialize, Deserialize, Debug, Default, Clone)]
pub struct NotificationFormat {
    pub title: Option<String>,
    pub body: Option<String>,
}

impl NotificationFormat {
    pub fn default() -> Self {
        Self {
            title: Some(String::from("%title")),
            body: Some(String::from("%artists")),
        }
    }
}

/// The configuration of ncspot.
#[derive(Clone, Serialize, Deserialize, Debug, Default)]
pub struct ConfigValues {
    /// Independent Spotify application identity. No shared upstream fallback.
    pub spotify_client_id: Option<String>,
    pub spotify_redirect_uri: Option<String>,
    pub command_key: Option<char>,
    pub initial_screen: Option<String>,
    /// Addresses of Rokus to offer for casting, for networks where they cannot be
    /// found by searching.
    pub roku_hosts: Option<Vec<String>>,
    pub default_keybindings: Option<bool>,
    pub keybindings: Option<HashMap<String, String>>,
    pub theme: Option<ConfigTheme>,
    pub use_nerdfont: Option<bool>,
    pub flip_status_indicators: Option<bool>,
    pub audio_cache: Option<bool>,
    pub audio_cache_size: Option<u32>,
    pub backend: Option<String>,
    pub backend_device: Option<String>,
    pub volnorm: Option<bool>,
    pub volnorm_pregain: Option<f64>,
    pub notify: Option<bool>,
    pub bitrate: Option<u32>,
    pub gapless: Option<bool>,
    pub shuffle: Option<bool>,
    pub repeat: Option<queue::RepeatSetting>,
    pub cover_max_scale: Option<f32>,
    pub playback_state: Option<PlaybackState>,
    pub track_format: Option<TrackFormat>,
    pub notification_format: Option<NotificationFormat>,
    pub statusbar_format: Option<String>,
    pub library_tabs: Option<Vec<LibraryTab>>,
    pub hide_display_names: Option<bool>,
    /// Frames per second the now playing visualizer animates at. `0` turns the
    /// animation off, leaving the rest of the screen updating as usual.
    pub visualizer_fps: Option<u32>,
    /// Draw the progress bar as the track's own waveform, once enough of the
    /// track has been heard to know its shape.
    pub waveform: Option<bool>,
    /// Shape the now playing visualizer takes: `bars`, `mirror`, `wave` or `vu`.
    pub visualizer_style: Option<String>,
    pub cover_accent: Option<bool>,
    pub beat_pulse: Option<bool>,
    pub ap_port: Option<u16>,
    /// Local radio exploration level, from familiar (0) to exploratory (100).
    /// When omitted, the value persisted in the user state is used.
    pub radio_discovery: Option<u8>,
}

/// The ncspot theme.
#[derive(Serialize, Deserialize, Debug, Default, Clone)]
pub struct ConfigTheme {
    pub background: Option<String>,
    pub primary: Option<String>,
    pub secondary: Option<String>,
    pub title: Option<String>,
    pub playing: Option<String>,
    pub playing_selected: Option<String>,
    pub playing_bg: Option<String>,
    pub highlight: Option<String>,
    pub highlight_bg: Option<String>,
    pub highlight_inactive_bg: Option<String>,
    pub error: Option<String>,
    pub error_bg: Option<String>,
    pub statusbar_progress: Option<String>,
    pub statusbar_progress_bg: Option<String>,
    pub statusbar: Option<String>,
    pub statusbar_bg: Option<String>,
    pub cmdline: Option<String>,
    pub cmdline_bg: Option<String>,
    pub search_match: Option<String>,
}

/// The ordering that is used when representing a playlist.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct SortingOrder {
    pub key: SortKey,
    pub direction: SortDirection,
}

/// The runtime state of the music queue.
#[derive(Serialize, Default, Deserialize, Debug, Clone)]
pub struct QueueState {
    pub current_track: Option<usize>,
    pub random_order: Option<Vec<usize>>,
    pub track_progress: std::time::Duration,
    pub queue: Vec<Playable>,
    /// Per-occurrence provenance; old state files default to playback context.
    #[serde(default)]
    pub explicit_queued: Vec<usize>,
    #[serde(default)]
    pub radio_generated: Vec<usize>,
    /// Pending playback context retained while a station owns the active lane.
    #[serde(default)]
    pub resume_context: Vec<usize>,
    /// Distinguish a consumed continuation from an ordinary playback context.
    #[serde(default)]
    pub resume_context_valid: bool,
}

/// Runtime state that should be persisted accross sessions.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct UserState {
    pub volume: u16,
    pub shuffle: bool,
    pub repeat: queue::RepeatSetting,
    pub queuestate: QueueState,
    pub playlist_orders: HashMap<String, SortingOrder>,
    pub cache_version: u16,
    pub playback_state: PlaybackState,
    #[serde(default = "default_radio_discovery")]
    pub radio_discovery: u8,
}

impl Default for UserState {
    fn default() -> Self {
        Self {
            volume: u16::MAX,
            shuffle: false,
            repeat: queue::RepeatSetting::None,
            queuestate: QueueState::default(),
            playlist_orders: HashMap::new(),
            cache_version: 0,
            playback_state: PlaybackState::Default,
            radio_discovery: default_radio_discovery(),
        }
    }
}

/// Configuration files are read/written relative to this directory.
static BASE_PATH: RwLock<Option<PathBuf>> = RwLock::new(None);

/// The complete configuration (state + user configuration) of ncspot.
pub struct Config {
    /// The configuration file path.
    filename: String,
    /// Configuration set by the user, read only.
    values: RwLock<ConfigValues>,
    /// Runtime state which can't be edited by the user, read/write.
    state: RwLock<UserState>,
}

impl Config {
    /// Create a default configuration from in-memory defaults, without touching the filesystem.
    #[cfg(test)]
    pub fn new_for_test() -> std::sync::Arc<Self> {
        Self::new_for_test_with_values(ConfigValues::default())
    }

    /// Create an in-memory configuration for tests with explicit user values.
    /// Startup-only overrides are applied to the initial runtime state just as
    /// they are by [`Self::new`], without touching the filesystem.
    #[cfg(test)]
    pub fn new_for_test_with_values(values: ConfigValues) -> std::sync::Arc<Self> {
        let mut state = UserState::default();
        if let Some(discovery) = values.radio_discovery {
            state.radio_discovery = clamp_radio_discovery(discovery);
        }
        std::sync::Arc::new(Self {
            filename: String::new(),
            values: RwLock::new(values),
            state: RwLock::new(state),
        })
    }

    /// Generate the configuration from the user configuration file and the runtime state file.
    /// `filename` can be used to look for a differently named configuration file.
    pub fn new(filename: Option<String>) -> Self {
        let filename = filename.unwrap_or(CONFIGURATION_FILE_NAME.to_owned());
        let values = load(&filename).unwrap_or_else(|e| {
            eprint!(
                "There is an error in your configuration file at {}:\n\n{e}",
                user_configuration_directory()
                    .map(|ref mut path| {
                        path.push(CONFIGURATION_FILE_NAME);
                        path.to_string_lossy().to_string()
                    })
                    .expect("configuration directory expected but not found")
            );
            process::exit(1);
        });

        let mut userstate = {
            let path = config_path(USER_STATE_FILE_NAME);
            if let Err(error) = backup_legacy_user_state(&path) {
                error!("could not preserve legacy user state before discovery migration: {error}");
                panic!("could not preserve legacy user state before discovery migration: {error}");
            }
            CBOR.load_or_generate_default(path, || Ok(UserState::default()), true)
                .expect("could not load user state")
        };

        if let Some(shuffle) = values.shuffle {
            userstate.shuffle = shuffle;
        }

        if let Some(repeat) = values.repeat {
            userstate.repeat = repeat;
        }

        if let Some(playback_state) = values.playback_state.clone() {
            userstate.playback_state = playback_state;
        }

        // A configured discovery value is the startup default. Runtime commands
        // update the persisted state and take effect until the next startup.
        if let Some(discovery) = values.radio_discovery {
            userstate.radio_discovery = clamp_radio_discovery(discovery);
        }

        Self {
            filename,
            values: RwLock::new(values),
            state: RwLock::new(userstate),
        }
    }

    /// Get the user configuration values.
    pub fn values(&self) -> RwLockReadGuard<'_, ConfigValues> {
        self.values.read().unwrap()
    }

    /// Get the runtime user state values.
    pub fn state(&self) -> RwLockReadGuard<'_, UserState> {
        self.state.read().unwrap()
    }

    /// Return the effective local radio exploration level. Both the configured
    /// startup value and the persisted runtime state are clamped so malformed or
    /// hand-edited state cannot leave the documented range.
    pub fn discovery(&self) -> u8 {
        clamp_radio_discovery(self.state().radio_discovery)
    }

    /// Set the runtime local radio exploration level.
    pub fn set_discovery(&self, value: u8) {
        self.with_state_mut(|state| state.radio_discovery = clamp_radio_discovery(value));
    }

    /// Modify the internal user state through a shared reference using a closure.
    pub fn with_state_mut<F>(&self, cb: F)
    where
        F: Fn(&mut UserState),
    {
        let mut state_guard = self.state.write().unwrap();
        cb(&mut state_guard);
    }

    /// Update the version number of the runtime user state. This should be done before saving it to
    /// disk.
    fn update_state_cache_version(&self) {
        self.with_state_mut(|state| state.cache_version = CACHE_VERSION);
    }

    /// Save runtime state to the user configuration directory.
    pub fn save_state(&self) {
        self.update_state_cache_version();

        let path = config_path(USER_STATE_FILE_NAME);
        debug!("saving user state to {}", path.display());
        if let Err(e) = CBOR.write(path, &*self.state()) {
            error!("Could not save user state: {e}");
        }
    }

    /// Create a [Theme] from the user supplied theme in the configuration file.
    pub fn build_theme(&self) -> Theme {
        crate::theme::load(&self.values().theme)
    }

    /// Attempt to reload the configuration from the configuration file.
    ///
    /// This only updates the values stored in memory but doesn't perform any additional actions
    /// like updating active keybindings.
    pub fn reload(&self) -> Result<(), Box<dyn Error>> {
        let cfg = load(&self.filename)?;
        *self.values.write().unwrap() = cfg;
        Ok(())
    }
}

/// Preserve a valid pre-discovery state file before the new field is persisted.
///
/// The backup is deliberately immutable: a pre-existing sibling is never
/// replaced. This leaves the original state untouched if copying or verification
/// fails, and makes the backup useful for decoding with an older binary.
fn backup_legacy_user_state(path: &std::path::Path) -> Result<(), String> {
    if !path.is_file() {
        return Ok(());
    }

    let bytes = fs::read(path).map_err(|error| {
        format!(
            "unable to read existing user state {}: {error}",
            path.display()
        )
    })?;
    let value: serde_cbor::Value = match serde_cbor::from_slice(&bytes) {
        Ok(value) => value,
        Err(_) => return Ok(()),
    };
    let has_discovery = match &value {
        serde_cbor::Value::Map(map) => map
            .keys()
            .any(|key| matches!(key, serde_cbor::Value::Text(key) if key == "radio_discovery")),
        _ => return Ok(()),
    };
    if has_discovery {
        return Ok(());
    }

    // Only preserve a state document that the current schema can actually read.
    // Invalid CBOR or unrelated documents retain the existing serializer's parse
    // failure behavior and do not create a misleading migration backup.
    if serde_cbor::from_slice::<UserState>(&bytes).is_err() {
        return Ok(());
    }

    let backup_path = path.with_file_name(PRE_DISCOVERY_USER_STATE_FILE_NAME);
    if !backup_path.exists() {
        return create_verified_backup(&backup_path, &bytes);
    }
    if verify_existing_backup(&backup_path, &bytes)? {
        return Ok(());
    }

    // A rollback binary may have written a different valid legacy state. Keep
    // the original base backup and give the new content its own immutable name.
    let hashed_path = path.with_file_name(format!(
        "userstate.pre-discovery-{:016x}.cbor",
        discovery_content_hash(&bytes)
    ));
    create_verified_backup(&hashed_path, &bytes)
}

fn verify_existing_backup(path: &std::path::Path, bytes: &[u8]) -> Result<bool, String> {
    if !path.exists() {
        return Ok(false);
    }
    let existing = fs::read(path).map_err(|error| {
        format!(
            "unable to verify existing pre-discovery backup {}: {error}",
            path.display()
        )
    })?;
    if existing == bytes {
        Ok(true)
    } else {
        Ok(false)
    }
}

fn create_verified_backup(path: &std::path::Path, bytes: &[u8]) -> Result<(), String> {
    if verify_existing_backup(path, bytes)? {
        return Ok(());
    }

    let mut backup = match fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
    {
        Ok(backup) => backup,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            if verify_existing_backup(path, bytes)? {
                return Ok(());
            }
            return Err(format!(
                "existing pre-discovery backup {} differs from the legacy state",
                path.display()
            ));
        }
        Err(error) => {
            return Err(format!(
                "unable to create pre-discovery backup {}: {error}",
                path.display()
            ));
        }
    };
    backup.write_all(bytes).map_err(|error| {
        format!(
            "unable to copy legacy state to pre-discovery backup {}: {error}",
            path.display()
        )
    })?;
    backup.sync_all().map_err(|error| {
        format!(
            "unable to sync pre-discovery backup {}: {error}",
            path.display()
        )
    })?;
    drop(backup);

    if verify_existing_backup(path, bytes)? {
        Ok(())
    } else {
        Err(format!(
            "pre-discovery backup {} does not match the legacy state",
            path.display()
        ))
    }
}

fn discovery_content_hash(bytes: &[u8]) -> u64 {
    // FNV-1a keeps the sibling name deterministic across process and compiler
    // versions while remaining small enough for a filename.
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3_u64);
    }
    hash
}

/// Parse the configuration file with name `filename` at the configuration base path.
fn load(filename: &str) -> Result<ConfigValues, String> {
    let path = config_path(filename);
    TOML.load_or_generate_default(path, || Ok(ConfigValues::default()), false)
}

/// Returns the plaform app directories for ncspot if they could be determined,
/// or an error otherwise.
pub fn try_proj_dirs() -> Result<AppDirs, String> {
    match *BASE_PATH
        .read()
        .map_err(|_| String::from("Poisoned RWLock"))?
    {
        Some(ref basepath) => Ok(AppDirs {
            cache_dir: basepath.join(".cache"),
            config_dir: basepath.join(".config"),
            data_dir: basepath.join(".local/share"),
            state_dir: basepath.join(".local/state"),
        }),
        None => {
            let primary = AppDirs::new(Some(ncspot::BIN_NAME), true)
                .ok_or_else(|| String::from("Couldn't determine platform standard directories"))?;
            // Keep an installed fork's populated library and playback state in
            // place. New installs use Resonance directories; existing ncspot
            // users keep their paths until they explicitly choose a new base.
            Ok(compatible_dirs(primary, AppDirs::new(Some("ncspot"), true)))
        }
    }
}

fn compatible_dirs(primary: AppDirs, legacy: Option<AppDirs>) -> AppDirs {
    if !primary.config_dir.join(CONFIGURATION_FILE_NAME).exists()
        && let Some(legacy) = legacy
        && legacy.config_dir.join(CONFIGURATION_FILE_NAME).is_file()
    {
        return legacy;
    }
    primary
}

/// Return the path to the current user's configuration directory, or None if it couldn't be found.
/// This function does not guarantee correct permissions or ownership of the directory!
pub fn user_configuration_directory() -> Option<PathBuf> {
    let project_directories = try_proj_dirs().ok()?;
    Some(project_directories.config_dir)
}

/// Return the path to the current user's cache directory, or None if one couldn't be found. This
/// function does not guarantee correct permissions or ownership of the directory!
pub fn user_cache_directory() -> Option<PathBuf> {
    let project_directories = try_proj_dirs().ok()?;
    Some(project_directories.cache_dir)
}

/// Force create the configuration directory at the default project location, removing anything that
/// isn't a directory but has the same name. Return the path to the configuration file inside the
/// directory.
///
/// This doesn't create the file, only the containing directory.
pub fn config_path(file: &str) -> PathBuf {
    let cfg_dir = user_configuration_directory().unwrap();
    if cfg_dir.exists() && !cfg_dir.is_dir() {
        fs::remove_file(&cfg_dir).expect("unable to remove old config file");
    }
    if !cfg_dir.exists() {
        fs::create_dir_all(&cfg_dir).expect("can't create config folder");
    }
    let mut cfg = cfg_dir.to_path_buf();
    cfg.push(file);
    cfg
}

/// Create the cache directory at the default project location, preserving it if it already exists,
/// and return the path to the cache file inside the directory.
///
/// This doesn't create the file, only the containing directory.
pub fn cache_path(file: &str) -> PathBuf {
    let cache_dir = user_cache_directory().unwrap();
    if !cache_dir.exists() {
        fs::create_dir_all(&cache_dir).expect("can't create cache folder");
    }
    let mut pb = cache_dir.to_path_buf();
    pb.push(file);
    pb
}

/// Set the configuration base path. All configuration files are read/written relative to this path.
pub fn set_configuration_base_path(base_path: Option<PathBuf>) {
    if let Some(basepath) = base_path {
        if !basepath.exists() {
            fs::create_dir_all(&basepath).expect("could not create basepath directory");
        }
        *BASE_PATH.write().unwrap() = Some(basepath);
    }
}

#[cfg(test)]
mod compatibility_tests {
    use super::*;

    #[derive(Clone, Deserialize, Serialize)]
    struct LegacyQueueState {
        current_track: Option<usize>,
        random_order: Option<Vec<usize>>,
        track_progress: std::time::Duration,
        queue: Vec<Playable>,
    }

    #[derive(Clone, Deserialize, Serialize)]
    struct LegacyUserState {
        volume: u16,
        shuffle: bool,
        repeat: queue::RepeatSetting,
        queuestate: LegacyQueueState,
        playlist_orders: HashMap<String, SortingOrder>,
        cache_version: u16,
        playback_state: PlaybackState,
    }

    fn legacy_track() -> Playable {
        Playable::Track(crate::model::track::Track {
            id: Some("legacy-track".into()),
            uri: "spotify:track:legacy-track".into(),
            title: "Legacy track".into(),
            track_number: 1,
            disc_number: 1,
            duration: 180_000,
            artists: vec!["Legacy artist".into()],
            artist_ids: vec!["legacy-artist".into()],
            album: Some("Legacy album".into()),
            album_id: Some("legacy-album".into()),
            album_artists: vec!["Legacy artist".into()],
            cover_url: None,
            url: "https://open.spotify.com/track/legacy-track".into(),
            added_at: None,
            list_index: 0,
            is_local: false,
            is_playable: Some(true),
        })
    }

    fn dirs(root: &std::path::Path, name: &str) -> AppDirs {
        let root = root.join(name);
        AppDirs {
            config_dir: root.join("config"),
            cache_dir: root.join("cache"),
            data_dir: root.join("data"),
            state_dir: root.join("state"),
        }
    }

    #[test]
    fn populated_legacy_install_keeps_paths_and_data() {
        let root = std::env::temp_dir().join(format!("resonance-compat-{}", rand::random::<u64>()));
        let primary = dirs(&root, "resonance");
        let legacy = dirs(&root, "ncspot");
        fs::create_dir_all(&legacy.config_dir).unwrap();
        fs::create_dir_all(&legacy.cache_dir).unwrap();
        fs::write(
            legacy.config_dir.join(CONFIGURATION_FILE_NAME),
            "shuffle = true",
        )
        .unwrap();
        fs::write(legacy.cache_dir.join("tracks.db"), "populated library").unwrap();
        let selected = compatible_dirs(primary, Some(legacy));
        assert_eq!(selected.cache_dir, root.join("ncspot/cache"));
        assert_eq!(
            fs::read_to_string(selected.cache_dir.join("tracks.db")).unwrap(),
            "populated library"
        );
        let primary = dirs(&root, "resonance");
        fs::create_dir_all(&primary.config_dir).unwrap();
        fs::write(
            primary.config_dir.join(CONFIGURATION_FILE_NAME),
            "shuffle = false",
        )
        .unwrap();
        let selected = compatible_dirs(primary, Some(dirs(&root, "ncspot")));
        assert_eq!(selected.config_dir, root.join("resonance/config"));
        assert_eq!(
            fs::read_to_string(root.join("ncspot/cache/tracks.db")).unwrap(),
            "populated library"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn populated_legacy_cbor_gets_default_discovery_and_round_trips() {
        let mut playlist_orders = HashMap::new();
        playlist_orders.insert(
            "playlist-1".into(),
            SortingOrder {
                key: SortKey::Title,
                direction: SortDirection::Ascending,
            },
        );
        let legacy = LegacyUserState {
            volume: 23_456,
            shuffle: true,
            repeat: queue::RepeatSetting::RepeatPlaylist,
            queuestate: LegacyQueueState {
                current_track: Some(0),
                random_order: Some(vec![0]),
                track_progress: std::time::Duration::from_millis(4_321),
                queue: vec![legacy_track()],
            },
            playlist_orders,
            cache_version: CACHE_VERSION,
            playback_state: PlaybackState::Paused,
        };
        let bytes = serde_cbor::to_vec(&legacy).unwrap();
        let mut state: UserState = serde_cbor::from_slice(&bytes).unwrap();

        assert_eq!(state.radio_discovery, 50);
        assert_eq!(state.volume, legacy.volume);
        assert_eq!(state.queuestate.queue.len(), 1);
        assert!(state.queuestate.explicit_queued.is_empty());
        assert!(state.queuestate.radio_generated.is_empty());
        assert!(state.queuestate.resume_context.is_empty());
        assert!(!state.queuestate.resume_context_valid);
        assert_eq!(state.playlist_orders.len(), 1);

        state.radio_discovery = 75;
        state.queuestate.explicit_queued = vec![0];
        state.queuestate.resume_context = vec![0];
        state.queuestate.resume_context_valid = true;
        let bytes = serde_cbor::to_vec(&state).unwrap();
        let restored: UserState = serde_cbor::from_slice(&bytes).unwrap();
        assert_eq!(restored.radio_discovery, 75);
        assert_eq!(restored.queuestate.explicit_queued, vec![0]);
        assert_eq!(restored.queuestate.resume_context, vec![0]);
        assert!(restored.queuestate.resume_context_valid);
        let rollback: LegacyUserState = serde_cbor::from_slice(&bytes).unwrap();
        assert_eq!(rollback.queuestate.queue.len(), 1);
        assert_eq!(rollback.queuestate.current_track, Some(0));
        assert_eq!(
            rollback.queuestate.track_progress,
            legacy.queuestate.track_progress
        );
        assert_eq!(restored.volume, legacy.volume);
        assert_eq!(restored.queuestate.queue.len(), 1);
        assert_eq!(restored.playlist_orders.len(), 1);
    }

    #[test]
    fn configured_discovery_is_startup_default_then_runtime_can_change() {
        let root =
            std::env::temp_dir().join(format!("resonance-discovery-{}", rand::random::<u64>()));
        let config_dir = root.join(".config");
        fs::create_dir_all(&config_dir).unwrap();
        fs::write(
            config_dir.join(CONFIGURATION_FILE_NAME),
            "radio_discovery = 125",
        )
        .unwrap();

        let mut playlist_orders = HashMap::new();
        playlist_orders.insert(
            "playlist-1".into(),
            SortingOrder {
                key: SortKey::Title,
                direction: SortDirection::Ascending,
            },
        );
        let legacy = LegacyUserState {
            volume: 23_456,
            shuffle: true,
            repeat: queue::RepeatSetting::RepeatPlaylist,
            queuestate: LegacyQueueState {
                current_track: Some(0),
                random_order: Some(vec![0]),
                track_progress: std::time::Duration::from_millis(4_321),
                queue: vec![legacy_track()],
            },
            playlist_orders,
            cache_version: CACHE_VERSION,
            playback_state: PlaybackState::Paused,
        };
        fs::write(
            config_dir.join(USER_STATE_FILE_NAME),
            serde_cbor::to_vec(&legacy).unwrap(),
        )
        .unwrap();

        let legacy_bytes = fs::read(config_dir.join(USER_STATE_FILE_NAME)).unwrap();
        let backup_path = config_dir.join(PRE_DISCOVERY_USER_STATE_FILE_NAME);

        set_configuration_base_path(Some(root.clone()));
        let config = Config::new(None);
        assert_eq!(config.discovery(), 100);
        assert_eq!(config.state().volume, legacy.volume);
        assert_eq!(config.state().queuestate.queue.len(), 1);
        assert_eq!(config.state().playlist_orders.len(), 1);
        assert_eq!(fs::read(&backup_path).unwrap(), legacy_bytes);

        config.set_discovery(25);
        assert_eq!(config.discovery(), 25);
        config.save_state();
        let saved_state = fs::read(config_dir.join(USER_STATE_FILE_NAME)).unwrap();
        let restored: UserState = CBOR.load(config_dir.join(USER_STATE_FILE_NAME)).unwrap();
        assert_eq!(restored.radio_discovery, 25);
        assert_eq!(restored.volume, legacy.volume);
        assert_eq!(restored.queuestate.queue.len(), 1);
        assert_eq!(restored.playlist_orders.len(), 1);

        // An older binary can still decode the new state because the added
        // field is an unknown map entry to its schema.
        let rollback: LegacyUserState = serde_cbor::from_slice(&saved_state).unwrap();
        assert_eq!(rollback.volume, legacy.volume);
        assert_eq!(rollback.queuestate.queue.len(), 1);
        assert_eq!(rollback.playlist_orders.len(), 1);

        // Simulate a rollback binary that writes a valid but changed legacy
        // state. A content-addressed sibling keeps both rollback snapshots.
        let mut changed_legacy = legacy.clone();
        changed_legacy.volume = 54_321;
        let changed_legacy_bytes = serde_cbor::to_vec(&changed_legacy).unwrap();
        drop(config);
        fs::write(config_dir.join(USER_STATE_FILE_NAME), &changed_legacy_bytes).unwrap();
        let second_start = Config::new(None);
        assert_eq!(second_start.state().volume, changed_legacy.volume);
        assert_eq!(fs::read(&backup_path).unwrap(), legacy_bytes);
        let changed_backup_path = config_dir.join(format!(
            "userstate.pre-discovery-{:016x}.cbor",
            discovery_content_hash(&changed_legacy_bytes)
        ));
        assert_eq!(
            fs::read(&changed_backup_path).unwrap(),
            changed_legacy_bytes
        );

        // Reusing the same changed legacy bytes must not create or overwrite a
        // second copy.
        drop(second_start);
        let _third_start = Config::new(None);
        assert_eq!(fs::read(&backup_path).unwrap(), legacy_bytes);
        assert_eq!(
            fs::read(&changed_backup_path).unwrap(),
            changed_legacy_bytes
        );

        *BASE_PATH.write().unwrap() = None;
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn discovery_clamps_runtime_values_even_when_startup_override_is_present() {
        let config = Config::new_for_test_with_values(ConfigValues {
            radio_discovery: Some(250),
            ..ConfigValues::default()
        });
        assert_eq!(config.discovery(), 100);
        config.set_discovery(150);
        assert_eq!(config.discovery(), 100);
        config.set_discovery(0);
        assert_eq!(config.discovery(), 0);
        config.set_discovery(75);
        assert_eq!(config.discovery(), 75);
    }
}
