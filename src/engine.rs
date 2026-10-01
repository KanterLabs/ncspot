//! Playback runtime shared by remote frontends. It never creates a terminal view.
use std::error::Error;
use std::sync::Arc;
use std::time::Duration;

use crate::application::{ASYNC_RUNTIME, initialize_session, mark_startup_phase};
use crate::command::{Command, SeekDirection};
use crate::config::{Config, PlaybackState};
use crate::events::{Event, EventManager};
use crate::library::Library;
use crate::queue::{Queue, RepeatSetting};
use crate::spotify::{PlayerEvent, Spotify};
use crate::traits::ListItem;

#[cfg(unix)]
use crate::ipc::IpcSocket;
#[cfg(unix)]
use signal_hook::{
    consts::{SIGHUP, SIGINT, SIGTERM},
    iterator::Signals,
};

pub struct Engine {
    pub queue: Arc<Queue>,
    pub library: Arc<Library>,
    pub config: Arc<Config>,
    pub events: EventManager,
    spotify: Spotify,
    #[cfg(unix)]
    ipc: Option<IpcSocket>,
    running: bool,
    shutdown_complete: bool,
    prewarmed: Option<String>,
}

impl Engine {
    pub fn new(configuration_file_path: Option<String>) -> Result<Self, Box<dyn Error>> {
        #[allow(unused_mut)]
        let (config, events, mut spotify) = initialize_session(configuration_file_path)?;
        crate::ui::osd::attach(&events);
        let library = Arc::new(Library::new(
            events.clone(),
            spotify.clone(),
            config.clone(),
        ));
        let queue = Arc::new(Queue::new(spotify.clone(), config.clone(), library.clone()));

        #[cfg(feature = "mpris")]
        {
            let manager = crate::mpris::MprisManager::new(
                events.clone(),
                queue.clone(),
                library.clone(),
                spotify.clone(),
            );
            spotify.set_mpris(manager);
        }
        let state = config.state();
        let playback_state = state.playback_state.clone();
        let progress = state
            .queuestate
            .track_progress
            .as_millis()
            .min(u128::from(u32::MAX)) as u32;
        drop(state);
        if let Some(playable) = queue.get_current() {
            spotify.load(
                &playable,
                playback_state == PlaybackState::Playing,
                progress,
            );
            spotify.update_track();
            match playback_state {
                PlaybackState::Stopped => spotify.stop(),
                PlaybackState::Paused | PlaybackState::Playing | PlaybackState::Default => {
                    spotify.pause()
                }
            }
        }
        mark_startup_phase("headless playback restored");

        #[cfg(unix)]
        let ipc = match crate::utils::create_runtime_directory() {
            Ok(directory) => Some(IpcSocket::new(
                ASYNC_RUNTIME.get().expect("runtime initialized").handle(),
                directory.join(format!("{}.sock", ncspot::BIN_NAME)),
                events.clone(),
                queue.clone(),
            )?),
            Err(error) => {
                log::error!("Could not create local IPC: {error}");
                None
            }
        };
        Ok(Self {
            queue,
            library,
            config,
            events,
            spotify,
            #[cfg(unix)]
            ipc,
            running: true,
            shutdown_complete: false,
            prewarmed: None,
        })
    }

    #[cfg(unix)]
    pub fn ipc_path(&self) -> Option<&std::path::Path> {
        self.ipc.as_ref().map(IpcSocket::path)
    }

    /// Run even when no frontend is connected. The timer handles listening,
    /// radio refill and progress publication when no player event is arriving.
    pub fn run(&mut self) -> Result<(), String> {
        let result = self.run_loop();
        self.shutdown();
        result
    }

    fn run_loop(&mut self) -> Result<(), String> {
        #[cfg(unix)]
        let mut signals = Signals::new([SIGTERM, SIGHUP, SIGINT])
            .map_err(|error| format!("Could not register engine signals: {error}"))?;
        while self.running {
            #[cfg(unix)]
            if signals.pending().next().is_some() {
                self.running = false;
                break;
            }
            // Collect first so event handling can mutably borrow the engine.
            let events: Vec<_> = self.events.msg_iter().collect();
            for event in events {
                self.handle_event(event)?;
                if !self.running {
                    break;
                }
            }
            if !self.running {
                break;
            }
            self.maintain();
            #[cfg(unix)]
            if let Some(ipc) = self.ipc.as_ref() {
                ipc.publish(&self.queue);
            }
            self.events.wait_timeout(Duration::from_millis(100));
        }
        Ok(())
    }

    fn maintain(&mut self) {
        self.queue.observe_listening();
        let current = self.queue.get_current().map(|item| item.uri());
        if current != self.prewarmed {
            self.prewarmed = current;
            crate::recommendations::prewarm(
                self.queue.clone(),
                self.library.clone(),
                self.events.clone(),
            );
        }
        self.queue.resume_radio_if_ready();
        crate::ui::radio::maintain(
            self.queue.clone(),
            self.library.clone(),
            self.events.clone(),
        );
    }

    fn handle_event(&mut self, event: Event) -> Result<(), String> {
        match event {
            Event::Player(state) => {
                self.spotify.update_status(state.clone());
                self.queue.observe_listening();
                if state == PlayerEvent::FinishedTrack {
                    self.queue.next(false);
                } else if state == PlayerEvent::Stopped {
                    self.queue.cancel_radio();
                }
            }
            Event::Queue(event) => self.queue.handle_event(event),
            Event::SessionDied => {
                self.events.notify("Spotify session ended; reconnecting…");
                self.spotify.start_worker(None).map_err(|error| {
                    let message = format!("Could not reconnect Spotify session: {error}");
                    self.events.notify(message.clone());
                    message
                })?;
            }
            Event::IpcInput(input) => match crate::command::parse(&input) {
                Ok(commands) => {
                    for command in commands {
                        if let Err(error) = self.execute(command) {
                            self.events.notify(error);
                        }
                    }
                }
                Err(error) => self.events.notify(format!("Invalid command: {error}")),
            },
            Event::Shutdown => self.running = false,
            Event::OpenPrototype => self
                .events
                .notify("The OpenTUI frontend is already the active interface"),
        }
        Ok(())
    }

    /// Compatibility commands from the original line-based socket act directly
    /// on runtime state. View-specific commands require the frontend RPC API.
    pub fn execute(&mut self, command: Command) -> Result<(), String> {
        match command {
            Command::Quit => self.running = false,
            Command::Noop | Command::Redraw => {}
            Command::TogglePlay => self.queue.toggleplayback(),
            Command::Stop => self.queue.stop(),
            Command::Next => self.queue.next(true),
            Command::Previous => {
                if self.spotify.get_current_progress() < Duration::from_secs(5) {
                    self.queue.previous();
                } else {
                    self.spotify.seek(0);
                }
            }
            Command::Clear => self.queue.clear(),
            Command::UpdateLibrary => self.library.update_library(),
            Command::Shuffle(mode) => self
                .queue
                .set_shuffle(mode.unwrap_or_else(|| !self.queue.get_shuffle())),
            Command::Repeat(mode) => {
                self.queue
                    .set_repeat(mode.unwrap_or_else(|| match self.queue.get_repeat() {
                        RepeatSetting::None => RepeatSetting::RepeatPlaylist,
                        RepeatSetting::RepeatPlaylist => RepeatSetting::RepeatTrack,
                        RepeatSetting::RepeatTrack => RepeatSetting::None,
                    }))
            }
            Command::Seek(SeekDirection::Relative(value)) => self.spotify.seek_relative(value),
            Command::Seek(SeekDirection::Absolute(value)) => self.spotify.seek(value),
            Command::VolumeUp(amount) => self.spotify.set_volume(
                self.spotify
                    .volume()
                    .saturating_add(655u16.saturating_mul(amount)),
                true,
            ),
            Command::VolumeDown(amount) => self.spotify.set_volume(
                self.spotify
                    .volume()
                    .saturating_sub(655u16.saturating_mul(amount)),
                true,
            ),
            Command::Discovery(Some(level)) => self.config.set_discovery(level),
            Command::Discovery(None) => self
                .events
                .notify(format!("Radio discovery: {}%", self.config.discovery())),
            Command::ReloadConfig => self.config.reload().map_err(|error| error.to_string())?,
            Command::Reconnect => self
                .spotify
                .start_worker(None)
                .map_err(|error| error.to_string())?,
            Command::Radio => {
                let track = self
                    .queue
                    .get_current()
                    .and_then(|item| item.track())
                    .ok_or_else(|| "Play a track before starting radio".to_string())?;
                crate::ui::radio::start(
                    self.queue.clone(),
                    self.library.clone(),
                    self.events.clone(),
                    track,
                );
            }
            Command::SaveCurrent => {
                if let Some(mut item) = self.queue.get_current() {
                    item.save(&self.library);
                }
            }
            other => {
                return Err(format!(
                    "Command '{}' requires a frontend view",
                    other.basename()
                ));
            }
        }
        self.events.trigger();
        Ok(())
    }

    /// Persist the same queue/user state and listening history as the legacy UI.
    /// Idempotent so signal, RPC and Drop cleanup can converge here safely.
    pub fn shutdown(&mut self) {
        if self.shutdown_complete {
            return;
        }
        self.running = false;
        self.queue.finish_listening();
        crate::recommendations::history::shared().flush();
        self.queue.cancel_radio();
        let queue = self.queue.queue.read().unwrap().clone();
        let random_order = self.queue.get_random_order();
        let current_track = self.queue.get_current_index();
        let track_progress = self.spotify.get_current_progress();
        self.config.with_state_mut(|state| {
            state.queuestate.queue.clone_from(&queue);
            state.queuestate.random_order.clone_from(&random_order);
            state.queuestate.current_track = current_track;
            state.queuestate.track_progress = track_progress;
        });
        self.config.save_state();
        self.spotify.shutdown();
        self.events.close();
        #[cfg(unix)]
        self.ipc.take();
        self.shutdown_complete = true;
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{playable::Playable, track::Track};

    fn track(id: &str) -> Track {
        Track {
            id: Some(id.into()),
            uri: format!("spotify:track:{id}"),
            title: id.into(),
            track_number: 1,
            disc_number: 1,
            duration: 1_000,
            artists: vec!["Artist".into()],
            artist_ids: vec![],
            album: Some("Album".into()),
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

    fn engine(ids: &[&str]) -> Engine {
        let config = Config::new_for_test();
        let events = EventManager::new();
        let spotify = Spotify::new_for_test(config.clone(), events.clone());
        let library = Library::new_for_test(events.clone(), spotify.clone(), config.clone());
        let queue = Arc::new(Queue::new_for_test(
            ids.iter().map(|id| Playable::Track(track(id))).collect(),
            Some(0),
            spotify.clone(),
            config.clone(),
            library.clone(),
        ));
        Engine {
            queue,
            library,
            config,
            events,
            spotify,
            #[cfg(unix)]
            ipc: None,
            running: true,
            // Test fixtures do not own a live session or persisted user state.
            shutdown_complete: true,
            prewarmed: None,
        }
    }

    #[test]
    fn finished_track_advances_without_a_frontend() {
        let mut engine = engine(&["first", "second"]);
        engine
            .handle_event(Event::Player(PlayerEvent::FinishedTrack))
            .unwrap();
        assert_eq!(engine.queue.get_current_index(), Some(1));
        assert!(!engine.events.has_cursive());
    }

    #[test]
    fn exhausted_radio_resumes_after_background_fill_without_a_frontend() {
        let mut engine = engine(&["seed"]);
        let seed = "spotify:track:seed";
        let generation = engine.queue.start_radio(seed);
        engine
            .handle_event(Event::Player(PlayerEvent::FinishedTrack))
            .unwrap();
        assert!(engine.queue.radio_active());
        assert!(engine.queue.radio_natural_end());
        assert_eq!(
            engine.queue.append_radio_at_tail_if_current(
                generation,
                seed,
                &[Playable::Track(track("refill"))],
            ),
            Some(1)
        );
        assert!(engine.queue.resume_radio_if_ready());
        assert_eq!(engine.queue.get_current_index(), Some(1));
        assert!(engine.queue.radio_active());
    }

    #[test]
    fn explicit_stop_cancels_radio() {
        let mut engine = engine(&["seed"]);
        engine.queue.start_radio("spotify:track:seed");
        engine
            .handle_event(Event::Player(PlayerEvent::Stopped))
            .unwrap();
        assert!(!engine.queue.radio_active());
    }

    #[test]
    fn line_commands_and_shutdown_are_handled_without_cursive() {
        let mut engine = engine(&["first", "second"]);
        engine.handle_event(Event::IpcInput("next".into())).unwrap();
        assert_eq!(engine.queue.get_current_index(), Some(1));
        engine.handle_event(Event::Shutdown).unwrap();
        assert!(!engine.running);
    }
}
