#[macro_use]
extern crate cursive;
#[macro_use]
extern crate serde;

use std::{path::PathBuf, process::exit};

use application::{Application, begin_startup_clock, setup_logging};
use config::set_configuration_base_path;
use log::error;
use ncspot::program_arguments;

mod application;
mod audio_tap;
mod authentication;
mod cast;
mod cli;
mod command;
mod commands;
mod config;
mod engine;
mod events;
mod ext_traits;
mod library;
mod model;
mod panic;
mod prototype;
mod queue;
mod recommendations;
mod search_cache;
mod serialization;
mod sharing;
mod spotify;
mod spotify_api;
mod spotify_url;
mod spotify_worker;
mod theme;
mod traits;
mod ui;
mod utils;
mod waveform;

#[cfg(unix)]
mod ipc;
#[cfg(unix)]
mod rpc;

#[cfg(feature = "mpris")]
mod mpris;

fn main() -> Result<(), String> {
    begin_startup_clock();

    // Set a custom backtrace hook that writes the backtrace to a file instead of stdout, since
    // stdout is most likely in use by Cursive.
    panic::register_backtrace_panic_handler();

    // Parse the command line arguments.
    let matches = program_arguments().get_matches();

    // Enable debug logging to a file if specified on the command line.
    if let Some(filename) = matches.get_one::<PathBuf>("debug") {
        setup_logging(filename).expect("logger could not be initialized");
    }

    // Set the configuration base path. All configuration files are read/written relative to this
    // path.
    set_configuration_base_path(matches.get_one::<PathBuf>("basepath").cloned());

    match matches.subcommand() {
        Some(("info", _subcommand_matches)) => cli::info(),
        Some(("radio-debug", args)) => cli::radio_debug(
            args.get_one::<String>("seed").cloned(),
            *args.get_one::<u64>("rng-seed").unwrap(),
            *args.get_one::<usize>("limit").unwrap(),
            *args.get_one::<u8>("discovery").unwrap(),
            args.get_one::<PathBuf>("replay").cloned(),
        ),
        Some((_, _)) => unreachable!(),
        None => {
            #[cfg(unix)]
            if !matches.get_flag("legacy-ui") {
                return run_opentui(
                    matches.get_one::<String>("config").cloned(),
                    matches.get_flag("headless"),
                    matches.get_one::<PathBuf>("debug").cloned(),
                );
            }
            #[cfg(not(unix))]
            if matches.get_flag("headless") {
                return Err("Headless mode currently requires Unix local IPC".into());
            }
            // Create the application.
            let mut application =
                match Application::new(matches.get_one::<String>("config").cloned()) {
                    Ok(application) => application,
                    Err(error) => {
                        eprintln!("{error}");
                        error!("{error}");
                        exit(-1);
                    }
                };

            // Start the application event loop.
            application.run()
        }
    }?;

    Ok(())
}

/// Authentication completes on the normal terminal before OpenTUI takes it
/// over. Rust retains the only playback session and owns frontend lifetime.
#[cfg(unix)]
fn run_opentui(
    config: Option<String>,
    headless: bool,
    debug_file: Option<PathBuf>,
) -> Result<(), String> {
    use std::process::Command;
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};
    // Locate the frontend before logging in or creating a playback session.
    let binary = if headless {
        None
    } else {
        Some(prototype::frontend_binary().map_err(|error| {
            format!("{error}. Use --legacy-ui while installing the OpenTUI executable.")
        })?)
    };
    let mut engine = engine::Engine::new(config).map_err(|error| error.to_string())?;
    if headless {
        return engine.run();
    }
    let socket = engine
        .ipc_path()
        .ok_or("Local IPC unavailable; use --legacy-ui")?
        .to_owned();
    let mut command = Command::new(binary.unwrap());
    command.arg("--socket").arg(socket);
    if let Some(file) = debug_file {
        command.arg("--debug").arg(file.with_extension("ui.log"));
    }
    let child = Arc::new(Mutex::new(
        command
            .spawn()
            .map_err(|error| format!("Could not start OpenTUI: {error}"))?,
    ));
    let monitor_child = child.clone();
    let events = engine.events.clone();
    let monitor = std::thread::spawn(move || {
        loop {
            match monitor_child.lock().unwrap().try_wait() {
                Ok(Some(status)) => {
                    events.send(events::Event::Shutdown);
                    return Ok(status);
                }
                Ok(None) => {}
                Err(error) => {
                    events.send(events::Event::Shutdown);
                    return Err(error);
                }
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    });
    let result = engine.run();
    let stopped_frontend = {
        let mut child = child.lock().unwrap();
        if child
            .try_wait()
            .map_err(|error| error.to_string())?
            .is_none()
        {
            // OpenTUI handles SIGTERM by restoring the terminal and releasing its timers.
            // The process is our owned child; SIGKILL is only a bounded fallback below.
            unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGTERM) };
            true
        } else {
            false
        }
    };
    if stopped_frontend {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let mut child = child.lock().unwrap();
            if child
                .try_wait()
                .map_err(|error| error.to_string())?
                .is_some()
            {
                break;
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                break;
            }
            drop(child);
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    let status = monitor
        .join()
        .map_err(|_| "Frontend monitor stopped unexpectedly")?
        .map_err(|error| error.to_string())?;
    result?;
    if !status.success() && !stopped_frontend {
        return Err(format!(
            "OpenTUI exited with {status}; try --legacy-ui and inspect the debug log"
        ));
    }
    Ok(())
}
