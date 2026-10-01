use std::{
    io,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use futures::SinkExt;
use log::{debug, error, info};
use tokio::net::{UnixListener, UnixStream};
use tokio::runtime::Handle;
use tokio::sync::watch::{Receiver, Sender};
use tokio_stream::StreamExt;
use tokio_stream::wrappers::WatchStream;
use tokio_util::codec::{FramedRead, FramedWrite, LinesCodec};

use crate::events::{Event, EventManager};
use crate::model::playable::Playable;
use crate::model::track::Track;
use crate::queue::Queue;
use crate::spotify::PlayerEvent;
use crate::traits::ListItem;
use ncspot::BIN_NAME;

pub struct IpcSocket {
    tx: Sender<Status>,
    path: PathBuf,
}

#[derive(Clone, Debug, Serialize)]
struct Status {
    mode: PlayerEvent,
    playable: Option<Playable>,
    prototype: PrototypeStatus,
}

#[derive(Clone, Debug, Serialize)]
struct PrototypeStatus {
    position_ms: u64,
    discovery: u8,
    volume_percent: u16,
    radio_active: bool,
    radio_waiting: bool,
    up_next: Vec<Track>,
}

impl Status {
    fn from_queue(queue: &Queue) -> Self {
        let spotify = queue.get_spotify();
        let current = queue.get_current_index();
        let start = current
            .and_then(|index| queue.play_position(index))
            .map_or(0, |position| position + 1);
        Self {
            mode: spotify.get_current_status(),
            playable: queue.get_current(),
            prototype: PrototypeStatus {
                position_ms: spotify
                    .get_current_progress()
                    .as_millis()
                    .min(u128::from(u64::MAX)) as u64,
                discovery: queue.get_library().cfg.discovery(),
                volume_percent: (f64::from(spotify.volume()) / 65535.0 * 100.0).round() as u16,
                radio_active: queue.radio_active(),
                radio_waiting: queue.radio_waiting() || queue.radio_natural_end(),
                up_next: queue
                    .in_play_order(start, 8)
                    .into_iter()
                    .filter_map(|(_, item)| item.track())
                    .collect(),
            },
        }
    }
}

impl Drop for IpcSocket {
    fn drop(&mut self) {
        self.try_remove_socket();
    }
}

impl IpcSocket {
    pub fn new(
        handle: &Handle,
        path: PathBuf,
        ev: EventManager,
        queue: Arc<Queue>,
    ) -> io::Result<Self> {
        let path = if path.exists() && Self::is_open_socket(&path) {
            let mut new_path = path;
            new_path.set_file_name(format!("{BIN_NAME}.{}.sock", std::process::id()));
            new_path
        } else if path.exists() && !Self::is_open_socket(&path) {
            std::fs::remove_file(&path)?;
            path
        } else {
            path
        };

        info!("Creating IPC domain socket at {path:?}");

        let status = Status::from_queue(&queue);

        let (tx, rx) = tokio::sync::watch::channel(status);
        let listener_path = path.clone();
        let worker_tx = tx.clone();
        handle.spawn(async move {
            let listener =
                UnixListener::bind(listener_path).expect("Could not create IPC domain socket");
            Self::worker(listener, ev, worker_tx, rx, queue).await;
        });

        Ok(Self { tx, path })
    }

    fn is_open_socket(path: &PathBuf) -> bool {
        std::os::unix::net::UnixStream::connect(path).is_ok()
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn publish(&self, queue: &Queue) {
        self.tx.send_replace(Status::from_queue(queue));
    }

    async fn worker(
        listener: UnixListener,
        ev: EventManager,
        tx: Sender<Status>,
        rx: Receiver<Status>,
        queue: Arc<Queue>,
    ) {
        let mut ticker = tokio::time::interval(Duration::from_millis(500));
        loop {
            tokio::select! {
                accepted = listener.accept() => match accepted {
                    Ok((stream, sockaddr)) => {
                        debug!("Connection from {sockaddr:?}");
                        // Refresh before subscription so a newly opened frontend gets live state immediately.
                        tx.send_replace(Status::from_queue(&queue));
                        tokio::spawn(Self::stream_handler(stream, ev.clone(), WatchStream::new(rx.clone())));
                    }
                    Err(e) => error!("Error accepting connection: {e}"),
                },
                _ = ticker.tick() => {
                    // The worker owns one receiver. Avoid snapshots when no frontend is connected.
                    if tx.receiver_count() > 1 {
                        tx.send_replace(Status::from_queue(&queue));
                    }
                }
            }
        }
    }

    async fn stream_handler(
        mut stream: UnixStream,
        ev: EventManager,
        mut rx: WatchStream<Status>,
    ) -> Result<(), String> {
        let (reader, writer) = stream.split();
        let mut framed_reader = FramedRead::new(reader, LinesCodec::new());
        let mut framed_writer = FramedWrite::new(writer, LinesCodec::new());

        loop {
            tokio::select! {
                line = framed_reader.next() => {
                    match line {
                        Some(Ok(line)) => {
                            debug!("Received line: \"{line}\"");
                            ev.send(Event::IpcInput(line));
                        }
                        Some(Err(e)) => error!("Error reading line: {e}"),
                        None => {
                            debug!("Closing IPC connection");
                            return Ok(())
                        }
                    }
                }
                Some(status) = rx.next() => {
                    debug!("IPC Status update: {status:?}");
                    let status_str = serde_json::to_string(&status).map_err(|e| e.to_string())?;
                    framed_writer.send(status_str).await.map_err(|e| e.to_string())?;
                }
                else => {
                    error!("All streams are closed");
                    return Ok(())
                }
            }
        }
    }

    /// Try to remove the IPC socket if there is one for this instance of Resonance. Don't do
    /// anything if the socket has already been removed for some reason.
    fn try_remove_socket(&mut self) {
        if std::fs::remove_file(&self.path).is_ok() {
            info!("removed socket at {:?}", self.path);
        } else {
            info!("socket already removed");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{config::Config, library::Library, spotify::Spotify};

    #[test]
    fn prototype_receives_live_state_and_sends_commands_to_existing_engine() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let config = Config::new_for_test();
        let events = EventManager::new();
        let spotify = Spotify::new_for_test(config.clone(), events.clone());
        let library = Library::new_for_test(events.clone(), spotify.clone(), config.clone());
        let queue = Arc::new(Queue::new_for_test(
            vec![],
            None,
            spotify,
            config.clone(),
            library,
        ));
        let path = std::env::temp_dir().join(format!(
            "resonance-prototype-{}.sock",
            rand::random::<u64>()
        ));
        let ipc = IpcSocket::new(runtime.handle(), path.clone(), events.clone(), queue).unwrap();
        runtime.block_on(async {
            let client = tokio::time::timeout(Duration::from_secs(2), async {
                loop {
                    if let Ok(client) = UnixStream::connect(&path).await {
                        break client;
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .unwrap();
            let (read, write) = client.into_split();
            let mut read = FramedRead::new(read, LinesCodec::new());
            let mut write = FramedWrite::new(write, LinesCodec::new());
            let initial = tokio::time::timeout(Duration::from_secs(2), read.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            let initial: serde_json::Value = serde_json::from_str(&initial).unwrap();
            assert_eq!(initial["mode"], "Stopped");
            assert!(initial["playable"].is_null());
            assert_eq!(initial["prototype"]["discovery"], 50);
            config.set_discovery(75);
            let updated = tokio::time::timeout(Duration::from_secs(2), async {
                loop {
                    let line = read.next().await.unwrap().unwrap();
                    let state: serde_json::Value = serde_json::from_str(&line).unwrap();
                    if state["prototype"]["discovery"] == 75 {
                        break state;
                    }
                }
            })
            .await
            .unwrap();
            assert_eq!(updated["prototype"]["position_ms"], 0);
            write.send("discovery 25".to_string()).await.unwrap();
            tokio::time::timeout(Duration::from_secs(2), async {
                loop {
                    if events.msg_iter().any(
                        |event| matches!(event, Event::IpcInput(input) if input == "discovery 25"),
                    ) {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .unwrap();
        });
        drop(ipc);
        assert!(!path.exists());
    }
}
