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

use crate::events::{Event, EventManager, Notification};
use crate::model::playable::Playable;
use crate::model::track::Track;
use crate::queue::Queue;
use crate::rpc::RpcService;
use crate::spotify::PlayerEvent;
use crate::traits::ListItem;
use ncspot::BIN_NAME;
use std::os::unix::fs::{FileTypeExt, MetadataExt};

pub struct IpcSocket {
    tx: Sender<Status>,
    path: PathBuf,
    worker: tokio::task::JoinHandle<()>,
    socket_identity: (u64, u64),
}

#[derive(Clone, Debug, Serialize)]
struct Status {
    mode: PlayerEvent,
    playable: Option<Playable>,
    prototype: PrototypeStatus,
    notifications: Vec<Notification>,
}

#[derive(Clone, Debug, Serialize)]
struct PrototypeStatus {
    position_ms: u64,
    discovery: u8,
    volume_percent: u16,
    radio_active: bool,
    radio_waiting: bool,
    up_next: Vec<Track>,
    audio: Option<AudioStatus>,
}

#[derive(Clone, Debug, Serialize)]
struct AudioStatus {
    bands: Vec<f32>,
    level: f32,
    pulse: f32,
    tempo: Option<f32>,
}

impl Status {
    fn from_queue(queue: &Queue) -> Self {
        let spotify = queue.get_spotify();
        let current = queue.get_current_index();
        let start = current
            .and_then(|index| queue.play_position(index))
            .map_or(0, |position| position + 1);
        Self {
            notifications: spotify.events().notifications(),
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
                audio: spotify.audio_tap().bands(32).map(|bands| {
                    let tap = spotify.audio_tap();
                    AudioStatus {
                        bands,
                        level: tap.level(),
                        pulse: tap.pulse(),
                        tempo: tap.tempo(),
                    }
                }),
            },
        }
    }
}

impl Drop for IpcSocket {
    fn drop(&mut self) {
        self.worker.abort();
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
            if !std::fs::symlink_metadata(&path)?.file_type().is_socket() {
                return Err(io::Error::new(
                    io::ErrorKind::AddrInUse,
                    "IPC path is occupied by a non-socket file",
                ));
            }
            std::fs::remove_file(&path)?;
            path
        } else {
            path
        };

        info!("Creating IPC domain socket at {path:?}");

        let status = Status::from_queue(&queue);

        let (tx, rx) = tokio::sync::watch::channel(status);
        let listener = {
            let _runtime = handle.enter();
            UnixListener::bind(&path)?
        };
        let metadata = std::fs::symlink_metadata(&path)?;
        let socket_identity = (metadata.dev(), metadata.ino());
        let worker_tx = tx.clone();
        let worker = handle.spawn(async move {
            Self::worker(listener, ev, worker_tx, rx, queue).await;
        });

        Ok(Self {
            tx,
            path,
            worker,
            socket_identity,
        })
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
        let rpc = Arc::new(RpcService::from_queue(queue.clone(), ev.clone()));
        let mut ticker = tokio::time::interval(Duration::from_millis(500));
        loop {
            tokio::select! {
                accepted = listener.accept() => match accepted {
                    Ok((stream, sockaddr)) => {
                        debug!("Connection from {sockaddr:?}");
                        // Refresh before subscription so a newly opened frontend gets live state immediately.
                        tx.send_replace(Status::from_queue(&queue));
                        tokio::spawn(Self::stream_handler(stream, ev.clone(), WatchStream::new(rx.clone()), rpc.clone()));
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
        rpc: Arc<RpcService>,
    ) -> Result<(), String> {
        let (reader, writer) = stream.split();
        let mut framed_reader =
            FramedRead::new(reader, LinesCodec::new_with_max_length(1024 * 1024));
        let mut framed_writer = FramedWrite::new(writer, LinesCodec::new());
        let (responses_tx, mut responses_rx) = tokio::sync::mpsc::channel::<serde_json::Value>(32);

        let inflight = Arc::new(tokio::sync::Semaphore::new(16));
        loop {
            tokio::select! {
                line = framed_reader.next() => {
                    match line {
                        Some(Ok(line)) => {
                            debug!("Received IPC request ({} bytes)", line.len());
                            if line.trim_start().starts_with('{') || line.trim_start().starts_with('[') {
                                let service = rpc.clone();
                                let responses = responses_tx.clone();
                                match inflight.clone().try_acquire_owned() {
                                    Ok(permit) => {
                                        tokio::task::spawn_blocking(move || {
                                            let _permit = permit;
                                            let response = service.handle_line(&line);
                                            let _ = responses.blocking_send(response);
                                        });
                                    }
                                    Err(_) => {
                                        let response = service.busy_response(&line);
                                        framed_writer.send(serde_json::to_string(&response).map_err(|e| e.to_string())?)
                                            .await.map_err(|e| e.to_string())?;
                                    }
                                }
                            } else {
                                ev.send(Event::IpcInput(line));
                            }
                        }
                        Some(Err(e)) => error!("Error reading line: {e}"),
                        None => {
                            debug!("Closing IPC connection");
                            return Ok(())
                        }
                    }
                }
                Some(response) = responses_rx.recv() => {
                    framed_writer.send(serde_json::to_string(&response).map_err(|e| e.to_string())?)
                        .await.map_err(|e| e.to_string())?;
                }
                status = rx.next() => {
                    let Some(status) = status else { return Ok(()); };
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
        let owned = std::fs::symlink_metadata(&self.path).is_ok_and(|meta| {
            meta.file_type().is_socket() && (meta.dev(), meta.ino()) == self.socket_identity
        });
        if owned && std::fs::remove_file(&self.path).is_ok() {
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

    /// Hosts the actual IPC/RPC stack for an externally driven OpenTUI integration test.
    /// This test never starts authentication, a Spotify worker, or library refresh.
    #[test]
    #[ignore = "requires an external frontend driver and RESONANCE_TEST_SOCKET"]
    fn opentui_frontend_fixture() {
        use crate::model::{artist::Artist, episode::Episode, playlist::Playlist, show::Show};
        let path = PathBuf::from(
            std::env::var("RESONANCE_TEST_SOCKET").expect("fixture socket path required"),
        );
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let config = Config::new_for_test();
        let events = EventManager::new();
        let spotify = Spotify::new_for_test(config.clone(), events.clone());
        let library = Library::new_for_test(events.clone(), spotify.clone(), config.clone());
        let track = |id: &str, title: &str, index| Track {
            id: Some(id.to_owned()),
            uri: format!("spotify:track:{id}"),
            title: title.to_owned(),
            track_number: index as u32 + 1,
            disc_number: 1,
            duration: 180_000,
            artists: vec!["Fixture Artist".to_owned()],
            artist_ids: vec!["FixtureArtist000000001".to_owned()],
            album: Some("Fixture Album".to_owned()),
            album_id: Some("FixtureAlbum0000000001".to_owned()),
            album_artists: vec!["Fixture Artist".to_owned()],
            cover_url: None,
            url: String::new(),
            added_at: None,
            list_index: index,
            is_local: false,
            is_playable: Some(true),
        };
        let alpha = track("FixtureTrackAlpha00001", "Fixture Alpha", 0);
        let beta = track("FixtureTrackBeta000002", "Fixture Beta", 1);
        *library.tracks.write().unwrap() = vec![alpha.clone(), beta.clone()];
        let mut artist = Artist::new(
            "FixtureArtist000000001".to_owned(),
            "Fixture Artist".to_owned(),
        );
        artist.tracks = Some(vec![alpha.clone(), beta.clone()]);
        artist.is_followed = true;
        library.artists.write().unwrap().push(artist);
        let album = serde_json::from_value(serde_json::json!({
            "id":"FixtureAlbum0000000001","title":"Fixture Album","artists":["Fixture Artist"],
            "artist_ids":["FixtureArtist000000001"],"year":"2026","cover_url":null,"url":null,
            "tracks":[alpha.clone(),beta.clone()],"added_at":null,"total_tracks":2
        }))
        .unwrap();
        library.albums.write().unwrap().push(album);
        let mut duplicate = alpha.clone();
        duplicate.list_index = 1;
        let mut final_track = beta.clone();
        final_track.list_index = 2;
        let items = vec![
            Playable::Track(alpha.clone()),
            Playable::Track(duplicate),
            Playable::Track(final_track),
        ];
        library.playlists.write().unwrap().push(Playlist {
            id: "FixturePlaylist000001".to_owned(),
            name: "Fixture Duplicates".to_owned(),
            owner_id: "FixtureOwner000000001".to_owned(),
            owner_name: Some("Fixture Owner".to_owned()),
            snapshot_id: "fixture-snapshot-1".to_owned(),
            num_tracks: 3,
            tracks: Some(items.clone()),
            collaborative: true,
        });
        library.shows.write().unwrap().push(Show {
            id: "FixtureShow00000000001".to_owned(),
            uri: "spotify:show:FixtureShow00000000001".to_owned(),
            name: "Fixture Podcast".to_owned(),
            description: "Cached fixture podcast".to_owned(),
            cover_url: None,
            episodes: Some(vec![Episode {
                id: "FixtureEpisode0000001".to_owned(),
                uri: "spotify:episode:FixtureEpisode0000001".to_owned(),
                duration: 240_000,
                name: "Fixture Episode".to_owned(),
                description: "Cached episode".to_owned(),
                release_date: "2026-10-01".to_owned(),
                cover_url: None,
                added_at: None,
                list_index: 0,
            }]),
        });
        library.set_load_state_for_test(crate::library::LoadState::Ready);
        let queue = Arc::new(Queue::new_for_test(
            items,
            None,
            spotify,
            config.clone(),
            library,
        ));
        let ipc = IpcSocket::new(
            runtime.handle(),
            path.clone(),
            events.clone(),
            queue.clone(),
        )
        .unwrap();
        events.notify("Fixture engine ready");
        let stopped = runtime.block_on(async {
            tokio::time::timeout(Duration::from_secs(60), async {
                loop {
                    if events
                        .msg_iter()
                        .any(|event| matches!(event, Event::Shutdown))
                    {
                        break;
                    }
                    ipc.publish(&queue);
                    tokio::time::sleep(Duration::from_millis(25)).await;
                }
            })
            .await
        });
        drop(ipc);
        assert!(
            stopped.is_ok(),
            "frontend driver did not issue quit within 60 seconds"
        );
        assert!(!path.exists(), "fixture must remove its socket on shutdown");
    }

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
            write.send(r#"{"protocol":"resonance","version":1,"id":"integration","method":"session.info","params":{}}"#.to_owned()).await.unwrap();
            let response = tokio::time::timeout(Duration::from_secs(2), async {
                loop {
                    let line = read.next().await.unwrap().unwrap();
                    let response: serde_json::Value = serde_json::from_str(&line).unwrap();
                    if response["id"] == "integration" { break response; }
                }
            }).await.unwrap();
            assert_eq!(response["ok"], true);
            assert_eq!(response["protocol"], "resonance");
            assert_eq!(response["result"]["version"], 1);
            assert!(response["result"]["capabilities"].as_array().unwrap().iter().any(|method| method == "queue.action"));
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
