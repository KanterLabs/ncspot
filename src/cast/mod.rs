//! Casting: playing on another Spotify Connect device, such as the Spotify app on a
//! Roku, while Resonance stays the remote.
//!
//! Resonance's player takes its orders as [`WorkerCommand`]s. While a cast is running,
//! those orders go to a [`Session`] instead of the local player, and the session
//! carries them out on the device through the Web API. It watches what the device
//! is doing and reports it back as the same [`PlayerEvent`]s the local player
//! sends, so the queue, shuffle, repeat and every view keep working unchanged: the
//! queue still decides what plays next, and the device plays one track at a time.

pub mod roku;

use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use log::{debug, info, warn};
use rspotify::model::{Device, DeviceType, PlayableItem};
use rspotify::prelude::Id;

use crate::events::{Event, EventManager};
use crate::model::playable::Playable;
use crate::spotify::{PlayerEvent, Spotify};
use crate::spotify_api::WebApi;
use crate::spotify_worker::WorkerCommand;
use crate::ui::osd;

/// How often the device is asked what it is doing. Every ask is a Web API
/// request, and those are rate limited, so no more often than the UI needs.
const POLL: Duration = Duration::from_secs(2);
/// After a command, how long the device's answers are not taken at their word: it
/// takes a moment to act, and until it has it still reports the old state.
const SETTLE: Duration = Duration::from_secs(3);
/// How close to the end a stopped track counts as having played to the end.
const END_SLACK_MS: u64 = 2500;
/// How long the device can be missing, or playing for someone else, before the
/// cast is given up and playback comes home.
const LOST_AFTER: Duration = Duration::from_secs(12);
/// How long to wait for the Spotify app on a Roku to come up after opening it.
const ROKU_WAKE: Duration = Duration::from_secs(30);

/// Somewhere to cast to.
#[derive(Clone, Debug)]
pub enum Target {
    /// A Spotify Connect device the account can already see.
    Connect {
        id: String,
        name: String,
        kind: String,
    },
    /// A Roku on the network. Its Spotify app is opened first, and then played on.
    Roku(roku::Roku),
}

impl Target {
    pub fn name(&self) -> &str {
        match self {
            Self::Connect { name, .. } => name,
            Self::Roku(roku) => &roku.name,
        }
    }
}

/// Everything there is to cast to: the Connect devices the account can see, and
/// the Rokus on the network whose Spotify app is not open yet. A Roku whose app
/// is already open is listed once, as the Connect device.
pub fn targets(api: &WebApi, roku_hosts: &[String]) -> Vec<Target> {
    let rokus = thread::scope(|scope| {
        let search = scope.spawn(|| roku::discover(Duration::from_millis(1500), roku_hosts));
        let devices = api.devices().unwrap_or_default();
        (devices, search.join().unwrap_or_default())
    });
    let (devices, rokus) = rokus;

    let mut targets: Vec<Target> = devices
        .into_iter()
        // Resonance's own session is not somewhere to cast to. Keep the old device name
        // filtered as well so a session started by an earlier install is not offered.
        .filter(|device| !is_local_device(&device.name) && !device.is_restricted)
        .filter_map(|device| {
            Some(Target::Connect {
                id: device.id?,
                kind: kind(&device._type).to_string(),
                name: device.name,
            })
        })
        .collect();
    for roku in rokus {
        let open = targets
            .iter()
            .any(|target| same_name(target.name(), &roku.name));
        if !open {
            targets.push(Target::Roku(roku));
        }
    }
    targets
}

/// Turn `target` into a Connect device to play on, opening Spotify on a Roku and
/// waiting for it to come up. `progress` is told what is happening while it waits.
pub fn connect(
    api: &WebApi,
    target: &Target,
    progress: &dyn Fn(String),
) -> Result<(String, String), String> {
    let roku = match target {
        Target::Connect { id, name, .. } => return Ok((id.clone(), name.clone())),
        Target::Roku(roku) => roku,
    };
    if roku.spotify == roku::SpotifyApp::Missing {
        return Err(format!(
            "{} does not have the Spotify app. Install it from the Roku channel store.",
            roku.name
        ));
    }

    let before: Vec<String> = api
        .devices()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|device| device.id)
        .collect();
    progress(format!("Opening Spotify on {}…", roku.name));
    roku::launch_spotify(roku)?;

    let started = Instant::now();
    let mut seen: Option<Vec<Device>> = None;
    while started.elapsed() < ROKU_WAKE {
        thread::sleep(Duration::from_secs(2));
        progress(format!(
            "Spotify is open on {}.\nWaiting for it to come online… {}s",
            roku.name,
            started.elapsed().as_secs()
        ));
        let Some(devices) = api.devices() else {
            continue;
        };
        debug!(
            "devices while waiting for {}: {:?}",
            roku.name,
            devices
                .iter()
                .map(|device| (&device.name, &device._type))
                .collect::<Vec<_>>()
        );
        if let Some(device) = pick(&devices, roku, &before)
            && let Some(id) = &device.id
        {
            return Ok((id.clone(), device.name.clone()));
        }
        seen = Some(devices);
    }

    let listing = match seen {
        None => "Spotify would not say which devices are online.".to_string(),
        Some(devices) if devices.is_empty() => {
            "Spotify reported no devices online at all.".to_string()
        }
        Some(devices) => {
            let lines: Vec<String> = devices
                .iter()
                .map(|device| format!("  • {} ({})", device.name, kind(&device._type)))
                .collect();
            format!("Spotify reported these devices:\n{}", lines.join("\n"))
        }
    };
    Err(format!(
        "Spotify opened on {} but never came online for this account.\n\n{listing}\n\n\
         Check the Spotify app on the TV is signed in to the same account. Picking a \
         device from the Spotify app on your phone once can also wake it up.",
        roku.name
    ))
}

/// Which of `devices` is the Spotify app on `roku`, most certain match first: one
/// named after any of the Roku's names; a TV that came online since the app was
/// opened; anything that came online since; or the only TV there is.
fn pick<'a>(devices: &'a [Device], roku: &roku::Roku, before: &[String]) -> Option<&'a Device> {
    let usable = |device: &&Device| device.id.is_some() && !is_local_device(&device.name);
    let is_new = |device: &&Device| device.id.as_ref().is_some_and(|id| !before.contains(id));
    let is_tv = |device: &&Device| matches!(device._type, DeviceType::Tv | DeviceType::Stb);

    devices
        .iter()
        .filter(usable)
        .find(|device| roku.names.iter().any(|name| same_name(&device.name, name)))
        .or_else(|| {
            devices
                .iter()
                .filter(usable)
                .find(|d| is_new(d) && is_tv(d))
        })
        .or_else(|| devices.iter().filter(usable).find(is_new))
        .or_else(|| {
            let mut tvs = devices.iter().filter(usable).filter(is_tv);
            let only = tvs.next()?;
            tvs.next().is_none().then_some(only)
        })
}

/// Names match ignoring case, spacing and punctuation, or when one contains the
/// other, since apps tend to decorate the name they are given.
fn same_name(a: &str, b: &str) -> bool {
    let normal = |name: &str| -> String {
        name.chars()
            .filter(|c| c.is_alphanumeric())
            .flat_map(char::to_lowercase)
            .collect()
    };
    let (a, b) = (normal(a), normal(b));
    !a.is_empty() && !b.is_empty() && (a == b || a.contains(&b) || b.contains(&a))
}

fn is_local_device(name: &str) -> bool {
    name == ncspot::BIN_NAME || name == "ncspot"
}

fn kind(kind: &DeviceType) -> &'static str {
    match kind {
        DeviceType::Tv => "TV",
        DeviceType::Speaker => "Speaker",
        DeviceType::Computer => "Computer",
        DeviceType::Smartphone => "Phone",
        DeviceType::Tablet => "Tablet",
        DeviceType::GameConsole => "Console",
        DeviceType::CastVideo | DeviceType::CastAudio => "Cast",
        DeviceType::Avr | DeviceType::Stb => "Receiver",
        DeviceType::Automobile => "Car",
        _ => "Device",
    }
}

enum Message {
    Command(Box<WorkerCommand>),
    /// Pause the device, if asked to, and stop driving it.
    End {
        pause: bool,
    },
}

/// A running cast. Dropping it does not end it; [`Session::end`] does.
pub struct Session {
    name: String,
    tx: Mutex<mpsc::Sender<Message>>,
}

impl Session {
    /// Start driving the Connect device `device_id`. `spotify` is where playback
    /// goes back to if the device drops out.
    pub fn start(spotify: Spotify, device_id: String, name: String) -> Arc<Self> {
        let (tx, rx) = mpsc::channel();
        let remote = Remote {
            api: spotify.api.clone(),
            events: spotify.events(),
            spotify,
            device: device_id,
            name: name.clone(),
            loaded: None,
            reported: None,
            quiet_until: Instant::now() + SETTLE,
            lost_since: None,
        };
        thread::Builder::new()
            .name("cast".into())
            .spawn(move || remote.run(rx))
            .expect("could not start the cast thread");
        Arc::new(Self {
            name,
            tx: Mutex::new(tx),
        })
    }

    /// The device being played on.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Take `command`, meant for the local player, and carry it out on the device.
    /// False for a command the local player still has to see.
    pub fn handle(&self, command: &WorkerCommand) -> bool {
        if matches!(command, WorkerCommand::Shutdown) {
            return false;
        }
        self.send(Message::Command(Box::new(command.clone())));
        true
    }

    pub fn end(&self, pause: bool) {
        self.send(Message::End { pause });
    }

    fn send(&self, message: Message) {
        if self.tx.lock().unwrap().send(message).is_err() {
            debug!("cast to {} has already ended", self.name);
        }
    }
}

/// The item the device was told to play.
struct Loaded {
    playable: Playable,
    uri: String,
    duration_ms: u64,
    /// Seen playing on the last poll, so a stop can be told from a pause.
    was_playing: bool,
    /// The end has been reported, and the queue has moved on.
    finished: bool,
}

/// What was last reported to the app, so unchanged state is not sent again.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Reported {
    /// Playing, having started this many milliseconds after the epoch.
    Playing(u128),
    Paused(u64),
    Stopped,
}

struct Remote {
    api: WebApi,
    events: EventManager,
    spotify: Spotify,
    device: String,
    name: String,
    loaded: Option<Loaded>,
    reported: Option<Reported>,
    /// Until when the device's answers are not trusted, after a command.
    quiet_until: Instant,
    lost_since: Option<Instant>,
}

impl Remote {
    fn run(mut self, rx: mpsc::Receiver<Message>) {
        info!("casting to {}", self.name);
        loop {
            match rx.recv_timeout(POLL) {
                Ok(Message::Command(command)) => self.execute(*command),
                Ok(Message::End { pause }) => {
                    if pause {
                        self.api.pause_on(&self.device);
                    }
                    info!("stopped casting to {}", self.name);
                    return;
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return,
            }
            if !self.poll() {
                return;
            }
        }
    }

    fn execute(&mut self, command: WorkerCommand) {
        debug!("cast command: {command:?}");
        let device = self.device.clone();
        let ok = match command {
            WorkerCommand::Load(playable, start_playing, position_ms) => {
                let ok = self.api.play_on(&device, &playable, position_ms);
                if ok && !start_playing {
                    self.api.pause_on(&device);
                }
                self.loaded = Some(Loaded {
                    uri: playable.uri(),
                    duration_ms: u64::from(playable.duration()),
                    playable,
                    was_playing: false,
                    finished: false,
                });
                self.reported = None;
                ok
            }
            WorkerCommand::Play => self.api.resume_on(&device),
            WorkerCommand::Pause => self.api.pause_on(&device),
            WorkerCommand::Stop => {
                self.loaded = None;
                self.report(Reported::Stopped);
                self.api.pause_on(&device)
            }
            WorkerCommand::Seek(position_ms) => self.api.seek_on(&device, position_ms),
            WorkerCommand::SetVolume(volume) => {
                let percent = (f64::from(volume) / f64::from(u16::MAX) * 100.0).round() as u8;
                // TVs usually keep the volume to themselves; that is not a failure.
                self.api.volume_on(&device, percent);
                true
            }
            // The device plays one track at a time, so there is nothing to preload.
            WorkerCommand::Preload(_) | WorkerCommand::Shutdown => true,
        };
        if !ok {
            osd::notify(format!("{} did not respond", self.name));
        }
        self.quiet_until = Instant::now() + SETTLE;
    }

    /// Ask the device what it is doing and pass that on. False once the cast has
    /// been given up.
    fn poll(&mut self) -> bool {
        let playback = self.api.playback();
        let here = playback
            .as_ref()
            .is_some_and(|playback| playback.device.id.as_deref() == Some(self.device.as_str()));
        if !here {
            return self.missing();
        }
        self.lost_since = None;
        let Some(playback) = playback else {
            return true;
        };
        let settling = Instant::now() < self.quiet_until;
        let progress = playback
            .progress
            .map_or(0, |progress| progress.num_milliseconds().max(0) as u64);
        let on_device = item_uri(playback.item.as_ref());
        if let Some(event) = self.observe(
            on_device.as_deref(),
            playback.is_playing,
            progress,
            settling,
        ) {
            match event {
                Observed::Finished => {
                    self.reported = None;
                    self.events.send(Event::Player(PlayerEvent::FinishedTrack));
                }
                Observed::State(state) => self.report(state),
            }
        }
        true
    }

    /// Work out what the device's state means: `on_device` is the item it has,
    /// `progress` how far into it, and `settling` whether a command was sent too
    /// recently for the answer to be trusted.
    fn observe(
        &mut self,
        on_device: Option<&str>,
        is_playing: bool,
        progress: u64,
        settling: bool,
    ) -> Option<Observed> {
        let loaded = self.loaded.as_mut()?;
        if loaded.finished {
            return None;
        }

        // Something else is playing: the track ran out and the device carried on
        // with something of its own.
        if on_device != Some(loaded.uri.as_str()) {
            if settling {
                return None;
            }
            loaded.finished = true;
            return Some(Observed::Finished);
        }

        let was_playing = std::mem::replace(&mut loaded.was_playing, is_playing);
        let at_end = progress == 0 || progress + END_SLACK_MS >= loaded.duration_ms;
        if !is_playing && was_playing && at_end && !settling {
            loaded.finished = true;
            return Some(Observed::Finished);
        }
        if settling {
            return None;
        }
        Some(Observed::State(if is_playing {
            let now = SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis();
            Reported::Playing(now.saturating_sub(u128::from(progress)))
        } else {
            Reported::Paused(progress)
        }))
    }

    /// Tell the app about `state`, unless it already knows.
    fn report(&mut self, state: Reported) {
        let unchanged = match (self.reported, state) {
            // The start time wobbles with network latency; only a real jump counts.
            (Some(Reported::Playing(before)), Reported::Playing(now)) => {
                before.abs_diff(now) < 1500
            }
            (Some(Reported::Paused(before)), Reported::Paused(now)) => before.abs_diff(now) < 1000,
            (before, now) => before == Some(now),
        };
        if unchanged {
            return;
        }
        self.reported = Some(state);
        let event = match state {
            Reported::Playing(started) => {
                PlayerEvent::Playing(SystemTime::UNIX_EPOCH + Duration::from_millis(started as u64))
            }
            Reported::Paused(progress) => PlayerEvent::Paused(Duration::from_millis(progress)),
            Reported::Stopped => PlayerEvent::Stopped,
        };
        self.events.send(Event::Player(event));
    }

    /// The device is gone, or playing for something else. Wait a while in case it
    /// comes back, then bring playback home, paused where it was.
    fn missing(&mut self) -> bool {
        if Instant::now() < self.quiet_until {
            return true;
        }
        let since = *self.lost_since.get_or_insert_with(Instant::now);
        if since.elapsed() < LOST_AFTER {
            return true;
        }
        warn!("lost {} while casting, coming home", self.name);
        osd::notify(format!("Lost {}; playback is back here", self.name));
        let resume = self.loaded.take().map(|loaded| {
            let position = self.spotify.get_current_progress().as_millis() as u32;
            (loaded.playable, position)
        });
        self.spotify.cast_lost(resume);
        false
    }
}

#[derive(PartialEq)]
enum Observed {
    Finished,
    State(Reported),
}

fn item_uri(item: Option<&PlayableItem>) -> Option<String> {
    match item? {
        PlayableItem::Track(track) => track.id.as_ref().map(|id| id.uri()),
        PlayableItem::Episode(episode) => Some(episode.id.uri()),
        PlayableItem::Unknown(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use crate::config::Config;
    use crate::events::EventManager;
    use crate::model::playable::Playable;
    use crate::model::track::Track;
    use crate::spotify::Spotify;

    use rspotify::model::{Device, DeviceType};

    use super::roku::{Roku, SpotifyApp};
    use super::{Loaded, Observed, Remote, Reported, pick, same_name};

    const SONG: &str = "spotify:track:say";

    fn remote() -> Remote {
        let cfg = Config::new_for_test();
        let events = EventManager::new_for_test();
        let spotify = Spotify::new_for_test(cfg, events.clone());
        let playable = Playable::Track(Track {
            id: Some("say".into()),
            uri: SONG.into(),
            title: "Say Why".into(),
            track_number: 5,
            disc_number: 1,
            duration: 143_000,
            artists: vec![],
            artist_ids: vec![],
            album: None,
            album_id: None,
            album_artists: vec![],
            cover_url: None,
            url: String::new(),
            added_at: None,
            list_index: 0,
            is_local: false,
            is_playable: Some(true),
        });
        Remote {
            api: spotify.api.clone(),
            events,
            spotify,
            device: "tv".into(),
            name: "Living Room TV".into(),
            loaded: Some(Loaded {
                playable,
                uri: SONG.into(),
                duration_ms: 143_000,
                was_playing: false,
                finished: false,
            }),
            reported: None,
            quiet_until: Instant::now(),
            lost_since: None,
        }
    }

    #[test]
    fn a_playing_track_is_reported_with_its_start_time() {
        let mut remote = remote();
        match remote.observe(Some(SONG), true, 76_000, false) {
            Some(Observed::State(Reported::Playing(started))) => {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::SystemTime::UNIX_EPOCH)
                    .unwrap()
                    .as_millis();
                assert!((now - started).abs_diff(76_000) < 1000);
            }
            _ => panic!("expected a playing state"),
        }
        assert!(matches!(
            remote.observe(Some(SONG), false, 80_000, false),
            Some(Observed::State(Reported::Paused(80_000)))
        ));
    }

    #[test]
    fn a_pause_mid_track_is_not_the_end_of_it() {
        let mut remote = remote();
        remote.observe(Some(SONG), true, 60_000, false);
        assert!(!matches!(
            remote.observe(Some(SONG), false, 61_000, false),
            Some(Observed::Finished)
        ));
    }

    #[test]
    fn a_track_that_plays_out_is_finished_once() {
        let mut remote = remote();
        remote.observe(Some(SONG), true, 141_500, false);
        // Spotify stops at the end with the playhead back at the start.
        assert!(remote.observe(Some(SONG), false, 0, false) == Some(Observed::Finished));
        assert!(remote.observe(Some(SONG), false, 0, false).is_none());
    }

    #[test]
    fn the_device_moving_on_by_itself_finishes_the_track() {
        let mut remote = remote();
        remote.observe(Some(SONG), true, 142_000, false);
        let other = Some("spotify:track:autoplayed");
        assert!(remote.observe(other, true, 1_000, false) == Some(Observed::Finished));
    }

    #[test]
    fn nothing_is_believed_while_a_command_settles() {
        let mut remote = remote();
        // Straight after a load the device still reports what it had before.
        assert!(
            remote
                .observe(Some("spotify:track:old"), true, 90_000, true)
                .is_none()
        );
        assert!(remote.observe(Some(SONG), false, 0, true).is_none());
        remote.quiet_until = Instant::now() - Duration::from_secs(1);
        assert!(remote.observe(Some(SONG), true, 500, false).is_some());
    }

    fn device(id: &str, name: &str, kind: DeviceType) -> Device {
        Device {
            id: Some(id.into()),
            is_active: false,
            is_private_session: false,
            is_restricted: false,
            name: name.into(),
            _type: kind,
            volume_percent: None,
        }
    }

    fn roku() -> Roku {
        Roku {
            name: "Living Room TV".into(),
            names: vec!["Living Room TV".into(), "TCL Roku TV".into()],
            base: "http://10.0.0.41:8060".into(),
            spotify: SpotifyApp::Unknown,
        }
    }

    #[test]
    fn the_app_is_found_under_any_of_the_rokus_names() {
        let devices = [
            device("phone", "Pixel", DeviceType::Smartphone),
            device("tv", "TCL Roku TV", DeviceType::Tv),
        ];
        // Known before it was opened, but its name gives it away.
        let before = ["phone".to_string(), "tv".to_string()];
        assert_eq!(
            pick(&devices, &roku(), &before).unwrap().name,
            "TCL Roku TV"
        );
    }

    #[test]
    fn a_newly_online_device_is_the_app_whatever_it_calls_itself() {
        let devices = [
            device("phone", "Pixel", DeviceType::Smartphone),
            device("new", "Spotify", DeviceType::Unknown),
        ];
        let before = ["phone".to_string()];
        assert_eq!(
            pick(&devices, &roku(), &before).unwrap().id.as_deref(),
            Some("new")
        );
    }

    #[test]
    fn the_only_tv_is_taken_when_nothing_else_fits() {
        let devices = [
            device("phone", "Pixel", DeviceType::Smartphone),
            device("tv", "Bedroom", DeviceType::Tv),
        ];
        let before = ["phone".to_string(), "tv".to_string()];
        assert_eq!(
            pick(&devices, &roku(), &before).unwrap().id.as_deref(),
            Some("tv")
        );

        let two = [
            device("a", "Bedroom", DeviceType::Tv),
            device("b", "Den", DeviceType::Tv),
        ];
        let before = ["a".to_string(), "b".to_string()];
        assert!(pick(&two, &roku(), &before).is_none());
    }

    #[test]
    fn device_names_match_loosely() {
        assert!(same_name("Living Room TV", "living room tv"));
        assert!(same_name("Living Room Roku", "Living-Room Roku (Spotify)"));
        assert!(!same_name("Kitchen", "Living Room TV"));
        assert!(!same_name("", "Living Room TV"));
    }
}
