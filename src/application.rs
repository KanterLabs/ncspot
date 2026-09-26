use std::error::Error;
use std::path::Path;
use std::rc::Rc;
use std::sync::{Arc, OnceLock};
use std::time::Instant;

use cursive::traits::Nameable;
use cursive::{Cursive, CursiveRunner};
use log::{debug, error, info, trace};

#[cfg(unix)]
use signal_hook::{consts::SIGHUP, consts::SIGTERM, iterator::Signals};

use crate::command::Command;
use crate::commands::CommandManager;
use crate::config::{Config, PlaybackState};
use crate::events::{Event, EventManager};
use crate::library::Library;
use crate::queue::Queue;
use crate::spotify::{PlayerEvent, SessionError, Spotify};
use crate::ui::create_cursive;
use crate::{authentication, ui, utils};
use crate::{command, queue, spotify};

#[cfg(feature = "mpris")]
use crate::mpris::MprisManager;

#[cfg(unix)]
use crate::ipc::{self, IpcSocket};

/// Set up the global logger to log to `filename`.
pub fn setup_logging(filename: &Path) -> Result<(), fern::InitError> {
    fern::Dispatch::new()
        // Perform allocation-free log formatting
        .format(|out, message, record| {
            out.finish(format_args!(
                "{} [{}] [{}] {}",
                chrono::Local::now().format("[%Y-%m-%d][%H:%M:%S%.3f]"),
                record.target(),
                record.level(),
                message
            ))
        })
        // Add blanket level filter -
        .level(log::LevelFilter::Debug)
        // Set runtime log level for modules
        .level_for("ncspot", log::LevelFilter::Trace)
        // Output to stdout, files, and other Dispatch configurations
        .chain(fern::log_file(filename)?)
        // Apply globally
        .apply()?;
    Ok(())
}

pub type UserData = Rc<UserDataInner>;
pub struct UserDataInner {
    pub cmd: CommandManager,
}

/// The global Tokio runtime for running asynchronous tasks.
pub static ASYNC_RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();

/// When the process started, for reporting how long each part of startup took.
static STARTUP: OnceLock<Instant> = OnceLock::new();

/// Note that a stage of startup has finished, with the time since the process began. Only visible
/// with `--debug`, where it is the first thing to look at when startup feels slow.
pub fn mark_startup_phase(phase: &str) {
    let elapsed = STARTUP.get_or_init(Instant::now).elapsed();
    debug!("startup: {phase} at {}ms", elapsed.as_millis());
}

/// Start the startup clock, as early in `main` as possible.
pub fn begin_startup_clock() {
    STARTUP.get_or_init(Instant::now);
}

/// The representation of an ncspot application.
pub struct Application {
    /// The music queue which controls playback order.
    queue: Arc<Queue>,
    /// Internally shared
    spotify: Spotify,
    /// Internally shared
    event_manager: EventManager,
    /// An IPC implementation using the D-Bus MPRIS protocol, used to control and inspect ncspot.
    #[cfg(unix)]
    ipc: Option<IpcSocket>,
    /// The object to render to the terminal.
    cursive: CursiveRunner<Cursive>,
}

impl Application {
    /// Create a new ncspot application.
    ///
    /// # Arguments
    ///
    /// * `configuration_file_path` - Relative path to the configuration file inside the base path
    pub fn new(configuration_file_path: Option<String>) -> Result<Self, Box<dyn Error>> {
        // Things here may cause the process to abort; we must do them before creating curses
        // windows otherwise the error message will not be seen by a user

        ASYNC_RUNTIME
            .set(
                tokio::runtime::Builder::new_multi_thread()
                    .enable_all()
                    .build()
                    .unwrap(),
            )
            .unwrap();

        mark_startup_phase("runtime ready");

        let configuration = Arc::new(Config::new(configuration_file_path));
        let theme = configuration.build_theme();
        mark_startup_phase("configuration read");

        // Connect to Spotify before the terminal is taken over, because a rejected login has to
        // be answered on stdout. The Web API token is not fetched here: it is renewed on demand
        // by the first call that needs one, all of which happen off this thread.
        let event_manager = EventManager::new();
        let mut credentials = authentication::get_credentials()?;

        // Only prompts when there is no Web API token to renew at all, which is the first run.
        if let Err(e) = authentication::ensure_rspotify_token() {
            error!("Failed to get rspotify token: {e}");
        }
        mark_startup_phase("credentials ready");

        println!("Connecting to Spotify..");

        let mut spotify = loop {
            match spotify::Spotify::new(
                event_manager.clone(),
                credentials.clone(),
                configuration.clone(),
            ) {
                Ok(spotify) => break spotify,
                Err(error) => {
                    // A refused session, as opposed to one that couldn't be opened at all, is
                    // answered by offering a fresh login.
                    let session_error = error.downcast::<SessionError>()?;
                    credentials = authentication::credentials_prompt(Some(session_error.0))?;
                }
            }
        };

        mark_startup_phase("spotify session open");

        // DON'T USE STDOUT AFTER THIS CALL!
        let mut cursive = create_cursive().map_err(|error| error.to_string())?;
        event_manager.attach_cursive(cursive.cb_sink().clone());
        // From here on a failure has somewhere to be seen, so the background workers
        // starting below can report to the screen instead of only to the log.
        ui::osd::attach(&event_manager);
        mark_startup_phase("terminal ready");

        cursive.set_theme(theme.clone());

        #[cfg(all(unix, feature = "pancurses_backend"))]
        cursive.add_global_callback(cursive::event::Event::CtrlChar('z'), |_s| unsafe {
            libc::raise(libc::SIGTSTP);
        });

        let library = Arc::new(Library::new(
            event_manager.clone(),
            spotify.clone(),
            configuration.clone(),
        ));

        mark_startup_phase("library created");

        let queue = Arc::new(queue::Queue::new(
            spotify.clone(),
            configuration.clone(),
            library.clone(),
        ));

        #[cfg(feature = "mpris")]
        let mpris_manager = MprisManager::new(
            event_manager.clone(),
            queue.clone(),
            library.clone(),
            spotify.clone(),
        );

        #[cfg(feature = "mpris")]
        spotify.set_mpris(mpris_manager.clone());

        // Load the last played track into the player
        let playback_state = configuration.state().playback_state.clone();
        let queue_state = configuration.state().queuestate.clone();

        if let Some(playable) = queue.get_current() {
            spotify.load(
                &playable,
                playback_state == PlaybackState::Playing,
                queue_state.track_progress.as_millis() as u32,
            );
            spotify.update_track();
            match playback_state {
                PlaybackState::Stopped => {
                    spotify.stop();
                }
                PlaybackState::Paused | PlaybackState::Playing | PlaybackState::Default => {
                    spotify.pause();
                }
            }
        }

        mark_startup_phase("playback restored");

        #[cfg(unix)]
        let ipc = if let Ok(runtime_directory) = utils::create_runtime_directory() {
            Some(
                ipc::IpcSocket::new(
                    ASYNC_RUNTIME.get().unwrap().handle(),
                    runtime_directory.join("ncspot.sock"),
                    event_manager.clone(),
                )
                .map_err(|e| e.to_string())?,
            )
        } else {
            error!("failed to create IPC socket: no suitable user runtime directory found");
            None
        };

        let mut cmd_manager = CommandManager::new(
            spotify.clone(),
            queue.clone(),
            library.clone(),
            configuration.clone(),
            event_manager.clone(),
        );

        cmd_manager.register_all();
        cmd_manager.register_keybindings(&mut cursive);

        cursive.set_user_data(Rc::new(UserDataInner { cmd: cmd_manager }));
        mark_startup_phase("commands registered");

        let search =
            ui::search::SearchView::new(event_manager.clone(), queue.clone(), library.clone());

        let libraryview = ui::library::LibraryView::new(queue.clone(), library.clone());

        let queueview = ui::queue::QueueView::new(queue.clone(), library.clone());

        let nowplayingview = ui::now_playing::NowPlayingView::new(
            queue.clone(),
            library.clone(),
            event_manager.clone(),
        );

        #[cfg(feature = "cover")]
        let coverview = ui::cover::CoverView::new(queue.clone(), library.clone(), &configuration);

        mark_startup_phase("screens constructed");

        let status = ui::statusbar::StatusBar::new(
            queue.clone(),
            Arc::clone(&library),
            event_manager.clone(),
        );

        let mut layout =
            ui::layout::Layout::new(status, &event_manager, theme, Arc::clone(&configuration))
                .screen("search", search.with_name("search"))
                .screen("library", libraryview.with_name("library"))
                .screen("queue", queueview)
                .screen("playing", nowplayingview);

        #[cfg(feature = "cover")]
        layout.add_screen("cover", coverview.with_name("cover"));

        // initial screen is library
        let initial_screen = configuration
            .values()
            .initial_screen
            .clone()
            .unwrap_or_else(|| "library".to_string());
        if layout.has_screen(&initial_screen) {
            layout.set_screen(initial_screen);
        } else {
            error!("Invalid screen name: {initial_screen}");
            layout.set_screen("library");
        }

        cursive.add_fullscreen_layer(layout.with_name("main"));
        mark_startup_phase("views built");

        Ok(Self {
            queue,
            spotify,
            event_manager,
            #[cfg(unix)]
            ipc,
            cursive,
        })
    }

    /// Start the application and run the event loop.
    pub fn run(&mut self) -> Result<(), String> {
        #[cfg(unix)]
        let mut signals =
            Signals::new([SIGTERM, SIGHUP]).expect("could not register signal handler");

        let mut first_frame = true;

        // cursive event loop
        while self.cursive.is_running() {
            self.cursive.step();

            if first_frame {
                mark_startup_phase("first frame drawn");
                first_frame = false;
            }
            #[cfg(feature = "cover")]
            self.cursive
                .call_on_name("cover", |view: &mut ui::cover::CoverView| {
                    view.render_to_terminal();
                });
            #[cfg(unix)]
            for signal in signals.pending() {
                if signal == SIGTERM || signal == SIGHUP {
                    info!("Caught {signal}, cleaning up and closing");
                    if let Some(data) = self.cursive.user_data::<UserData>().cloned() {
                        data.cmd.handle(&mut self.cursive, Command::Quit);
                    }
                }
            }
            for event in self.event_manager.msg_iter() {
                match event {
                    Event::Player(state) => {
                        trace!("event received: {state:?}");
                        self.spotify.update_status(state.clone());

                        #[cfg(unix)]
                        if let Some(ref ipc) = self.ipc {
                            ipc.publish(&state, self.queue.get_current());
                        }

                        if state == PlayerEvent::FinishedTrack {
                            self.queue.next(false);
                        }
                    }
                    Event::Queue(event) => {
                        self.queue.handle_event(event);
                    }
                    Event::SessionDied => {
                        if self.spotify.start_worker(None).is_err() {
                            let data: UserData = self
                                .cursive
                                .user_data()
                                .cloned()
                                .expect("user data should be set");
                            data.cmd.handle(&mut self.cursive, Command::Quit);
                        };
                    }
                    Event::IpcInput(input) => match command::parse(&input) {
                        Ok(commands) => {
                            if let Some(data) = self.cursive.user_data::<UserData>().cloned() {
                                for cmd in commands {
                                    info!("Executing command from IPC: {cmd}");
                                    data.cmd.handle(&mut self.cursive, cmd);
                                }
                            }
                        }
                        Err(e) => error!("Parsing error: {e}"),
                    },
                }
            }
        }
        Ok(())
    }
}
