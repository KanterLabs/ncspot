//! Versioned, presentation-independent local API. Network calls use WebApi's retry policy.
use crate::{
    artwork::{self, ArtworkCache},
    config::Config,
    events::{Event, EventManager},
    library::Library,
    model::{
        album::Album, artist::Artist, episode::Episode, playable::Playable, playlist::Playlist,
        show::Show, track::Track,
    },
    queue::{Queue, RepeatSetting},
    traits::ListItem,
};
use rspotify::model::{SearchResult, SearchType};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const VERSION: u64 = 1;
const METHODS: &[&str] = &[
    "session.info",
    "player.status",
    "player.action",
    "player.artwork",
    "queue.list",
    "queue.action",
    "library.list",
    "library.detail",
    "library.action",
    "search",
    "playlist.action",
    "radio.status",
    "radio.action",
    "radio.debug",
    "settings.get",
    "settings.action",
    "cast.list",
    "cast.action",
    "share",
];
#[derive(Debug)]
struct Error {
    code: &'static str,
    message: String,
}
type Result<T> = std::result::Result<T, Error>;
fn fail(code: &'static str, message: impl Into<String>) -> Error {
    Error {
        code,
        message: message.into(),
    }
}
fn invalid(message: impl Into<String>) -> Error {
    fail("invalid_params", message)
}
fn remote<T>(result: std::result::Result<T, ()>) -> Result<T> {
    result.map_err(|_| {
        fail(
            "upstream_error",
            "Spotify request failed; check authentication or rate limiting",
        )
    })
}
fn string<'a>(p: &'a Value, key: &str) -> Result<&'a str> {
    p.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| invalid(format!("{key} must be a nonempty string")))
}
fn number(p: &Value, key: &str, default: usize, max: usize) -> Result<usize> {
    match p.get(key) {
        None => Ok(default),
        Some(v) => v
            .as_u64()
            .filter(|n| *n <= max as u64)
            .map(|n| n as usize)
            .ok_or_else(|| invalid(format!("{key} must be an integer from 0 to {max}"))),
    }
}
fn bounds(p: &Value) -> Result<(usize, usize)> {
    let o = number(p, "offset", 0, u32::MAX as usize)?;
    let l = number(p, "limit", 50, 200)?;
    if l == 0 {
        return Err(invalid("limit must be positive"));
    }
    Ok((o, l))
}
fn search_bounds(p: &Value) -> Result<(usize, usize)> {
    let (offset, limit) = bounds(p)?;
    if offset > 1000 {
        return Err(invalid("search offset must be from 0 to 1000"));
    }
    Ok((
        offset,
        limit.min(crate::spotify_api::SEARCH_MAX_LIMIT as usize),
    ))
}
fn dimension(p: &Value, key: &str, default: usize, max: usize) -> Result<usize> {
    let value = number(p, key, default, max)?;
    if value == 0 {
        return Err(invalid(format!("{key} must be an integer from 1 to {max}")));
    }
    Ok(value)
}
fn id(p: &Value) -> Result<String> {
    let raw = p
        .get("id")
        .or_else(|| p.get("uri"))
        .and_then(Value::as_str)
        .ok_or_else(|| invalid("id or uri required"))?;
    let id = raw.rsplit(':').next().unwrap_or("");
    if id.is_empty() || !id.chars().all(|c| c.is_ascii_alphanumeric()) {
        return Err(invalid("invalid Spotify identity"));
    }
    Ok(id.to_owned())
}
fn row(
    id: impl Into<String>,
    kind: &str,
    title: impl Into<String>,
    subtitle: impl Into<String>,
    uri: Option<String>,
) -> Value {
    json!({"id":id.into(),"kind":kind,"title":title.into(),"subtitle":subtitle.into(),"uri":uri})
}
fn track(t: &Track) -> Value {
    let mut r = row(
        t.id.clone().unwrap_or_else(|| t.uri.clone()),
        "track",
        &t.title,
        t.artists.join(", "),
        Some(t.uri.clone()),
    );
    r["duration_ms"] = json!(t.duration);
    r["detail"] = json!(t.album);
    r["meta"] = json!({"added_at":t.added_at});
    r
}
fn playable(p: &Playable) -> Value {
    match p {
        Playable::Track(t) => track(t),
        Playable::Episode(e) => episode(e),
    }
}
fn cached_radio_seed_track(
    queue: &Queue,
    library: &Library,
    seed_uri: Option<&str>,
) -> Option<Track> {
    let seed_uri = seed_uri?;
    queue
        .queue
        .read()
        .unwrap()
        .iter()
        .find_map(|item| match item {
            Playable::Track(track) if track.uri == seed_uri => Some(track.clone()),
            _ => None,
        })
        .or_else(|| {
            library
                .tracks
                .read()
                .unwrap()
                .iter()
                .find(|track| track.uri == seed_uri)
                .cloned()
        })
}
fn playlist_item(p: &Playable) -> Value {
    let mut row = playable(p);
    if !row["meta"].is_object() {
        row["meta"] = json!({});
    }
    row["meta"]["index"] = json!(p.list_index());
    row
}
fn album(a: &Album) -> Value {
    let mut r = row(
        a.id.clone().unwrap_or_default(),
        "album",
        &a.title,
        a.artists.join(", "),
        a.id.as_ref().map(|id| format!("spotify:album:{id}")),
    );
    r["meta"] = json!({"added_at":a.added_at});
    r
}
fn artist(a: &Artist) -> Value {
    row(
        a.id.clone().unwrap_or_default(),
        "artist",
        &a.name,
        "",
        a.id.as_ref().map(|id| format!("spotify:artist:{id}")),
    )
}
fn playlist(p: &Playlist) -> Value {
    let mut r = row(
        &p.id,
        "playlist",
        &p.name,
        p.owner_name.as_deref().unwrap_or(&p.owner_id),
        Some(format!("spotify:playlist:{}", p.id)),
    );
    r["detail"] = json!(format!("{} tracks", p.num_tracks));
    r
}
fn show(s: &Show) -> Value {
    let mut r = row(&s.id, "show", &s.name, "", Some(s.uri.clone()));
    r["detail"] = json!(s.description);
    r
}
fn episode(e: &Episode) -> Value {
    let mut r = row(
        &e.id,
        "episode",
        &e.name,
        &e.release_date,
        Some(e.uri.clone()),
    );
    r["duration_ms"] = json!(e.duration);
    r["detail"] = json!(e.description);
    r
}
fn unavailable_artwork(uri: Option<&str>, width: usize, height: usize, reason: &str) -> Value {
    json!({
        "available": false,
        "uri": uri,
        "width": width,
        "height": height,
        "reason": reason,
    })
}
fn page(mut rows: Vec<Value>, p: &Value, source: &str) -> Result<Value> {
    let (offset, limit) = bounds(p)?;
    let total = rows.len();
    for (index, row) in rows.iter_mut().enumerate() {
        if row["kind"] == "track" || row["kind"] == "episode" {
            if !row["meta"].is_object() {
                row["meta"] = json!({});
            }
            if row["meta"].get("index").is_none() {
                row["meta"]["index"] = json!(index);
            }
        }
    }
    Ok(
        json!({"items":rows.into_iter().skip(offset).take(limit).collect::<Vec<_>>(),"offset":offset,"limit":limit,"total":total,"has_more":offset.saturating_add(limit)<total,"source":source}),
    )
}

const PAGE_CACHE_LIMIT: usize = 128;
const PAGE_CACHE_TTL: Duration = Duration::from_secs(300);
struct CachedPage {
    fetched_at: Instant,
    value: Value,
}

pub struct RpcService {
    queue: Arc<Queue>,
    library: Arc<Library>,
    config: Arc<Config>,
    events: EventManager,
    instance_id: String,
    pages: Mutex<HashMap<String, CachedPage>>,
    artwork: ArtworkCache,
    casts: Mutex<Vec<crate::cast::Target>>,
    playlist_mutations: Mutex<HashMap<String, Arc<Mutex<()>>>>,
}
impl RpcService {
    pub fn new(
        queue: Arc<Queue>,
        library: Arc<Library>,
        config: Arc<Config>,
        events: EventManager,
    ) -> Self {
        Self {
            queue,
            library,
            config,
            events,
            instance_id: format!("{}-{:016x}", std::process::id(), rand::random::<u64>()),
            pages: Mutex::new(HashMap::new()),
            artwork: ArtworkCache::new(),
            casts: Mutex::new(Vec::new()),
            playlist_mutations: Mutex::new(HashMap::new()),
        }
    }
    pub fn from_queue(queue: Arc<Queue>, events: EventManager) -> Self {
        let library = queue.get_library();
        let config = library.cfg.clone();
        Self::new(queue, library, config, events)
    }
    pub fn busy_response(&self, line: &str) -> Value {
        let id = serde_json::from_str::<Value>(line)
            .ok()
            .and_then(|request| request.get("id").cloned())
            .unwrap_or(Value::Null);
        self.response(
            id,
            Err(fail(
                "busy",
                "Too many in-flight requests; retry after pending requests complete",
            )),
        )
    }
    pub fn handle_line(&self, line: &str) -> Value {
        let request: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => {
                return self.response(Value::Null, Err(fail("invalid_request", "Malformed JSON")));
            }
        };
        let request_id = request.get("id").cloned().unwrap_or(Value::Null);
        let result = (|| {
            if !request.is_object() || !(request_id.is_string() || request_id.is_number()) {
                return Err(fail(
                    "invalid_request",
                    "Request id must be a string or number",
                ));
            }
            if request.get("protocol").and_then(Value::as_str) != Some("resonance") {
                return Err(fail("invalid_protocol", "Expected resonance protocol"));
            }
            if request.get("version").and_then(Value::as_u64) != Some(VERSION) {
                return Err(fail("unsupported_version", "Expected protocol version 1"));
            }
            let method = string(&request, "method")?;
            let p = request.get("params").cloned().unwrap_or_else(|| json!({}));
            if !p.is_object() {
                return Err(invalid("params must be an object"));
            }
            self.dispatch(method, &p).map(|mut result| {
                self.decorate(&mut result);
                result
            })
        })();
        self.response(request_id, result)
    }
    fn response(&self, id: Value, result: Result<Value>) -> Value {
        match result {
            Ok(result) => {
                json!({"protocol":"resonance","version":VERSION,"id":id,"instance_id":self.instance_id,"ok":true,"result":result})
            }
            Err(e) => {
                json!({"protocol":"resonance","version":VERSION,"id":id,"instance_id":self.instance_id,"ok":false,"error":{"code":e.code,"message":e.message}})
            }
        }
    }
    fn cached_page(&self, key: &str) -> Option<Value> {
        let mut pages = self.pages.lock().unwrap();
        pages.retain(|_, entry| entry.fetched_at.elapsed() < PAGE_CACHE_TTL);
        pages.get(key).map(|entry| {
            let mut value = entry.value.clone();
            value["source"] = json!("memory_cache");
            value["refresh_available"] = json!(true);
            value
        })
    }
    fn remember_page(&self, key: String, value: &Value) {
        let mut pages = self.pages.lock().unwrap();
        pages.retain(|_, entry| entry.fetched_at.elapsed() < PAGE_CACHE_TTL);
        if !pages.contains_key(&key)
            && pages.len() >= PAGE_CACHE_LIMIT
            && let Some(oldest) = pages
                .iter()
                .min_by_key(|(_, entry)| entry.fetched_at)
                .map(|(key, _)| key.clone())
        {
            pages.remove(&oldest);
        }
        pages.insert(
            key,
            CachedPage {
                fetched_at: Instant::now(),
                value: value.clone(),
            },
        );
    }
    fn cached_remote_page(
        &self,
        domain: &str,
        p: &Value,
        fetch: impl FnOnce() -> Result<Value>,
    ) -> Result<Value> {
        bounds(p)?;
        let key = format!("{domain}:{p}");
        if let Some(value) = self.cached_page(&key) {
            return Ok(value);
        }
        let result = fetch()?;
        if result["source"] == "spotify" {
            self.remember_page(key, &result);
        }
        Ok(result)
    }
    fn decorate(&self, result: &mut Value) {
        let Some(items) = result.get_mut("items").and_then(Value::as_array_mut) else {
            return;
        };
        let saved = self
            .library
            .tracks
            .read()
            .unwrap()
            .iter()
            .map(|t| t.uri.clone())
            .collect::<std::collections::HashSet<_>>();
        let albums = self
            .library
            .albums
            .read()
            .unwrap()
            .iter()
            .filter_map(|a| a.id.clone())
            .collect::<std::collections::HashSet<_>>();
        let artists = self
            .library
            .artists
            .read()
            .unwrap()
            .iter()
            .filter_map(|a| a.id.clone())
            .collect::<std::collections::HashSet<_>>();
        let shows = self
            .library
            .shows
            .read()
            .unwrap()
            .iter()
            .map(|show| show.id.clone())
            .collect::<std::collections::HashSet<_>>();
        let user_id = self.library.user_id();
        let playlists = self.library.playlists.read().unwrap();
        for item in items {
            if item["kind"] == "track" {
                item["saved"] = json!(item["uri"].as_str().is_some_and(|uri| saved.contains(uri)));
            }
            if let Some(id) = item["id"].as_str().map(str::to_owned) {
                match item["kind"].as_str() {
                    Some("album") => item["saved"] = json!(albums.contains(&id)),
                    Some("artist") => item["saved"] = json!(artists.contains(&id)),
                    Some("show") => item["saved"] = json!(shows.contains(&id)),
                    Some("playlist") => {
                        item["saved"] = json!(playlists.iter().any(|list| list.id == id))
                    }
                    _ => {}
                }
            }
            if item["kind"] == "playlist"
                && let Some(list) = playlists
                    .iter()
                    .find(|list| item["id"].as_str() == Some(list.id.as_str()))
            {
                let owned = user_id.as_deref() == Some(list.owner_id.as_str());
                item["meta"] = json!({"owned":owned,"can_edit":owned || list.collaborative,"collaborative":list.collaborative,"owner_id":list.owner_id});
            }
        }
    }
    fn cached(&self, kind: &str) -> Result<Vec<Value>> {
        Ok(match kind {
            "tracks" => self
                .library
                .tracks
                .read()
                .unwrap()
                .iter()
                .map(track)
                .collect(),
            "albums" => self
                .library
                .albums
                .read()
                .unwrap()
                .iter()
                .map(album)
                .collect(),
            "artists" => self
                .library
                .artists
                .read()
                .unwrap()
                .iter()
                .map(artist)
                .collect(),
            "playlists" => self
                .library
                .playlists
                .read()
                .unwrap()
                .iter()
                .map(playlist)
                .collect(),
            "shows" => self
                .library
                .shows
                .read()
                .unwrap()
                .iter()
                .map(show)
                .collect(),
            _ => return Err(invalid("unknown library kind")),
        })
    }
    fn resolve(&self, uri: &str) -> Result<Playable> {
        if let Some(t) = self
            .library
            .tracks
            .read()
            .unwrap()
            .iter()
            .find(|t| t.uri == uri)
            .cloned()
        {
            return Ok(Playable::Track(t));
        }
        if let Some(p) = self
            .queue
            .queue
            .read()
            .unwrap()
            .iter()
            .find(|t| t.uri() == uri)
            .cloned()
        {
            return Ok(p);
        }
        if let Some(t) = crate::search_cache::shared()
            .snapshot()
            .into_iter()
            .find(|t| t.uri == uri)
        {
            return Ok(Playable::Track(t));
        }
        for list in self.library.playlists.read().unwrap().iter() {
            if let Some(p) = list
                .tracks
                .as_ref()
                .and_then(|ts| ts.iter().find(|t| t.uri() == uri))
                .cloned()
            {
                return Ok(p);
            }
        }
        for album in self.library.albums.read().unwrap().iter() {
            if let Some(t) = album
                .tracks
                .as_ref()
                .and_then(|ts| ts.iter().find(|t| t.uri == uri))
                .cloned()
            {
                return Ok(Playable::Track(t));
            }
        }
        for show in self.library.shows.read().unwrap().iter() {
            if let Some(e) = show
                .episodes
                .as_ref()
                .and_then(|es| es.iter().find(|e| e.uri == uri))
                .cloned()
            {
                return Ok(Playable::Episode(e));
            }
        }
        let parts: Vec<_> = uri.split(':').collect();
        if parts.len() != 3 || parts[0] != "spotify" {
            return Err(invalid("expected Spotify track or episode URI"));
        }
        let i = id(&json!({"id":parts[2]}))?;
        let api = self.queue.get_spotify().api;
        match parts[1] {
            "track" => Ok(Playable::Track(Track::from(&remote(api.track(&i))?))),
            "episode" => Ok(Playable::Episode(Episode::from(&remote(api.episode(&i))?))),
            _ => Err(invalid("only track and episode URIs are playable")),
        }
    }
    /// Resolve a complete context before touching the queue. Every failed page aborts the action.
    fn context(&self, uri: &str) -> Result<Vec<Playable>> {
        const MAX_CONTEXT_ITEMS: usize = 100_000;
        let parts = uri.split(':').collect::<Vec<_>>();
        if parts.len() != 3 || parts[0] != "spotify" {
            return Err(invalid("expected a Spotify URI"));
        }
        let identity = id(&json!({"id":parts[2]}))?;
        let api = self.queue.get_spotify().api;
        let tracks = match parts[1] {
            "track" | "episode" => vec![self.resolve(uri)?],
            "album" => {
                let cached = self
                    .library
                    .albums
                    .read()
                    .unwrap()
                    .iter()
                    .find(|album| album.id.as_deref() == Some(&identity))
                    .and_then(|album| album.complete_tracks().map(<[Track]>::to_vec));
                if let Some(tracks) = cached {
                    tracks.into_iter().map(Playable::Track).collect()
                } else {
                    let full = remote(api.album(&identity))?;
                    let total = full.tracks.total as usize;
                    if total > MAX_CONTEXT_ITEMS {
                        return Err(invalid("context exceeds 100000 entries"));
                    }
                    if full.tracks.offset != 0 {
                        return Err(fail(
                            "upstream_error",
                            "Album first page has an unexpected offset",
                        ));
                    }
                    let mut items = full
                        .tracks
                        .items
                        .iter()
                        .map(|track| Playable::Track(Track::from_simplified_track(track, &full)))
                        .collect::<Vec<_>>();
                    let mut offset = items.len();
                    while offset < total {
                        let page = remote(api.album_tracks(&identity, 50, offset as u32))?;
                        if page.offset as usize != offset
                            || page.items.is_empty()
                            || page.total as usize != total
                        {
                            return Err(fail(
                                "upstream_error",
                                "Album changed or pagination failed; queue was not modified",
                            ));
                        }
                        offset = offset.saturating_add(page.items.len());
                        items.extend(page.items.iter().map(|track| {
                            Playable::Track(Track::from_simplified_track(track, &full))
                        }));
                    }
                    if items.len() != total {
                        return Err(fail(
                            "upstream_error",
                            "Incomplete album; queue was not modified",
                        ));
                    }
                    items
                }
            }
            "artist" => {
                let cached = self
                    .library
                    .artists
                    .read()
                    .unwrap()
                    .iter()
                    .find(|artist| artist.id.as_deref() == Some(&identity))
                    .and_then(|artist| artist.tracks.clone());
                match cached {
                    Some(tracks) => tracks,
                    None => remote(api.artist_top_tracks(&identity))?,
                }
                .into_iter()
                .map(Playable::Track)
                .collect()
            }
            "playlist" => {
                let cached = self
                    .library
                    .playlists
                    .read()
                    .unwrap()
                    .iter()
                    .find(|playlist| playlist.id == identity)
                    .and_then(|playlist| playlist.tracks.clone());
                if let Some(items) = cached {
                    items
                } else {
                    let playlist = Playlist::from(&remote(api.playlist(&identity))?);
                    if playlist.num_tracks > MAX_CONTEXT_ITEMS {
                        return Err(invalid("context exceeds 100000 entries"));
                    }
                    let mut items = Vec::new();
                    let mut offset = 0;
                    while offset < playlist.num_tracks {
                        let page = remote(api.playlist_tracks_page(&identity, offset as u32, 100))?;
                        if page.offset as usize != offset
                            || page.total as usize != playlist.num_tracks
                        {
                            return Err(fail(
                                "stale_playlist",
                                "Playlist changed while loading; queue was not modified",
                            ));
                        }
                        items.extend(page.items);
                        offset = offset.saturating_add(100);
                    }
                    let latest = remote(api.playlist(&identity))?;
                    if latest.snapshot_id != playlist.snapshot_id {
                        return Err(fail(
                            "stale_playlist",
                            "Playlist changed while loading; queue was not modified",
                        ));
                    }
                    items
                }
            }
            _ => {
                return Err(invalid(
                    "playable context must be a track, episode, album, artist or playlist",
                ));
            }
        };
        if tracks.len() > MAX_CONTEXT_ITEMS {
            return Err(invalid("context exceeds 100000 entries"));
        }
        let tracks = tracks
            .into_iter()
            .filter(|item| match item {
                Playable::Track(track) => {
                    track.id.is_some() && !track.is_local && track.is_playable != Some(false)
                }
                Playable::Episode(episode) => !episode.id.is_empty(),
            })
            .collect::<Vec<_>>();
        if tracks.is_empty() {
            return Err(fail("empty_context", "No available items in this context"));
        }
        Ok(tracks)
    }
    fn dispatch(&self, method: &str, p: &Value) -> Result<Value> {
        match method {
            "session.info" => Ok(
                json!({"instance_id":self.instance_id,"version":VERSION,"capabilities":METHODS,"user_id":self.library.user_id(),"display_name":self.library.display_name(),"notifications":self.events.notifications()}),
            ),
            "library.list" => {
                let kind = string(p, "kind")?;
                if kind == "browse" {
                    return self.cached_remote_page("browse", p, || self.browse(p, None));
                }
                let mut rows = self.cached(kind)?;
                if let Some(filter) = p.get("filter").and_then(Value::as_str) {
                    let f = filter.to_lowercase();
                    rows.retain(|r| {
                        format!("{} {}", r["title"], r["subtitle"])
                            .to_lowercase()
                            .contains(&f)
                    });
                }
                if let Some(sort) = p.get("sort").and_then(Value::as_str) {
                    match sort {
                        "title" | "name" => {
                            rows.sort_by_key(|r| r["title"].as_str().unwrap_or("").to_lowercase())
                        }
                        "artist" => rows
                            .sort_by_key(|r| r["subtitle"].as_str().unwrap_or("").to_lowercase()),
                        "added" => rows.sort_by(|a, b| {
                            b["meta"]["added_at"]
                                .as_str()
                                .unwrap_or("")
                                .cmp(a["meta"]["added_at"].as_str().unwrap_or(""))
                        }),
                        "default" => {}
                        _ => return Err(invalid("unsupported sort")),
                    }
                }
                for r in &mut rows {
                    r["saved"] = json!(true);
                }
                page(rows, p, "cache")
            }
            "library.detail" => self.cached_remote_page("detail", p, || self.detail(p)),
            "search" => self.search(p),
            "queue.list" => {
                let (revision, items, current) = self.queue.rpc_snapshot();
                let rows = items
                    .iter()
                    .enumerate()
                    .map(|(i, item)| {
                        let mut r = playable(item);
                        r["id"] = json!(format!("{revision}:{i}"));
                        r["meta"] = json!({"index":i,"current":current==Some(i),"origin":self.queue.queue_origin(i)});
                        r
                    })
                    .collect();
                let mut result = page(rows, p, "queue")?;
                result["revision"] = json!(revision);
                Ok(result)
            }
            "queue.action" => self.queue_action(p),
            "player.status" => {
                let current = self.queue.get_current();
                Ok(
                    json!({"repeat":self.queue.get_repeat(),"shuffle":self.queue.get_shuffle(),"saved":current.as_ref().map(|t|self.library.is_saved_track(t)),"current":current.as_ref().map(playable),"volume_percent":u64::from(self.queue.get_spotify().volume())*100/65535}),
                )
            }
            "player.action" => self.player_action(p),
            "player.artwork" => self.player_artwork(p),
            "library.action" => self.library_action(p),
            "playlist.action" => self.playlist_action(p),
            "radio.status" => {
                let seed = self.queue.radio_seed();
                let seed_track = self.queue.radio_seed_track().or_else(|| {
                    cached_radio_seed_track(&self.queue, &self.library, seed.as_deref())
                });
                let catalog = crate::recommendations::catalog(&self.queue, &self.library);
                let catalog_tracks = catalog
                    .tracks
                    .iter()
                    .map(|track| &track.uri)
                    .collect::<HashSet<_>>()
                    .len();
                let upcoming = self.queue.upcoming(usize::MAX);
                let radio_pending_count = upcoming
                    .iter()
                    .filter(|(index, _)| self.queue.queue_origin(*index) == "radio")
                    .count();
                let explicit_pending_count = upcoming
                    .iter()
                    .filter(|(index, _)| self.queue.queue_origin(*index) == "explicit")
                    .count();
                Ok(json!({
                    "active":self.queue.radio_active(),
                    "waiting":self.queue.radio_waiting() || self.queue.radio_natural_end(),
                    "seed":seed,"seed_track":seed_track,"discovery":self.config.discovery(),
                    "played_count":self.queue.session_played().len(),
                    "cache_tracks":self.library.tracks.read().unwrap().len(),
                    "catalog_tracks":catalog_tracks,
                    "queue_mode":if self.queue.radio_active() { "station" } else { "context" },
                    "parked_count":self.queue.parked_context_count(),
                    "radio_pending_count":radio_pending_count,"explicit_pending_count":explicit_pending_count
                }))
            }
            "radio.action" => {
                match string(p, "action")? {
                    "start" => {
                        let uri = p
                            .get("uri")
                            .and_then(Value::as_str)
                            .map(str::to_owned)
                            .or_else(|| self.queue.get_current().map(|t| t.uri()))
                            .ok_or_else(|| invalid("uri or current track required"))?;
                        let seed = self.resolve(&uri)?;
                        if !matches!(seed, Playable::Track(_)) {
                            return Err(invalid("radio seed must be a track"));
                        }
                        let track = seed
                            .track()
                            .ok_or_else(|| invalid("radio requires a track"))?;
                        if track.is_local || track.id.is_none() {
                            return Err(invalid("radio requires a Spotify track"));
                        }
                        if self
                            .queue
                            .get_current()
                            .as_ref()
                            .map(Playable::uri)
                            .as_deref()
                            != Some(&uri)
                        {
                            self.queue.rpc_append_play_radio_seed(&track);
                        }
                        crate::ui::radio::start(
                            self.queue.clone(),
                            self.library.clone(),
                            self.events.clone(),
                            track,
                        );
                    }
                    "stop" => self.queue.cancel_radio(),
                    "discovery" => self
                        .config
                        .set_discovery(number(p, "value", 50, 100)? as u8),
                    _ => return Err(invalid("unknown radio action")),
                }
                Ok(json!({"applied":true}))
            }
            "radio.debug" => {
                let limit = number(p, "limit", 20, 200)?;
                let rng = number(p, "rng_seed", 0, usize::MAX)?;
                let uri = p
                    .get("uri")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .or_else(|| self.queue.radio_seed())
                    .or_else(|| self.queue.get_current().map(|t| t.uri()));
                let seed = if let Some(uri) = uri {
                    match self
                        .queue
                        .radio_seed_track()
                        .filter(|track| track.uri == uri)
                    {
                        Some(track) => Some(track),
                        None => self.resolve(&uri)?.track(),
                    }
                } else {
                    self.library.tracks.read().unwrap().first().cloned()
                }
                .ok_or_else(|| fail("not_ready", "No cached radio seed available"))?;
                let (diagnostic, _) = crate::recommendations::recommend(
                    &self.queue,
                    &self.library,
                    seed,
                    rng as u64,
                    limit,
                );
                serde_json::to_value(diagnostic).map_err(|e| fail("internal_error", e.to_string()))
            }
            "settings.get" => {
                let mut values = serde_json::to_value(self.config.values().clone())
                    .map_err(|e| fail("internal_error", e.to_string()))?;
                if let Some(v) = values.as_object_mut() {
                    v.remove("spotify_client_id");
                    v.remove("spotify_redirect_uri");
                }
                let bindings: HashMap<String, String> =
                    crate::commands::CommandManager::get_bindings(&self.config)
                        .into_iter()
                        .map(|(key, commands)| {
                            (
                                key,
                                commands
                                    .iter()
                                    .map(ToString::to_string)
                                    .collect::<Vec<_>>()
                                    .join("; "),
                            )
                        })
                        .collect();
                Ok(json!({"values":values,"bindings":bindings}))
            }
            "settings.action" => {
                match string(p, "action")? {
                    "reload" => self
                        .config
                        .reload()
                        .map_err(|e| fail("config_error", e.to_string()))?,
                    "reconnect" => self
                        .queue
                        .get_spotify()
                        .start_worker(None)
                        .map_err(|e| fail("session_error", e.to_string()))?,
                    "logout" => {
                        for path in [
                            crate::authentication::playback_cache_path().join("credentials.json"),
                            crate::authentication::api_token_path(),
                        ] {
                            if let Err(e) = std::fs::remove_file(path)
                                && e.kind() != std::io::ErrorKind::NotFound
                            {
                                return Err(fail("logout_error", e.to_string()));
                            }
                        }
                        self.queue.get_spotify().shutdown();
                        self.events.send(Event::Shutdown);
                    }
                    "command" => {
                        let commands = crate::command::parse(string(p, "command")?)
                            .map_err(|error| invalid(error.to_string()))?;
                        let mut results = Vec::with_capacity(commands.len());
                        for command in commands {
                            results.push(self.execute_command(command)?);
                        }
                        let completed = results.iter().all(|result| {
                            result
                                .get("completed")
                                .and_then(Value::as_bool)
                                .unwrap_or(true)
                        });
                        return Ok(json!({"completed":completed,"results":results}));
                    }
                    _ => return Err(invalid("unknown settings action")),
                }
                Ok(json!({"applied":true}))
            }
            "cast.list" => self.cast_list(p),
            "cast.action" => self.cast_action(p),
            "share" => {
                let uri = string(p, "uri")?;
                let parts: Vec<_> = uri.split(':').collect();
                if parts.len() != 3
                    || parts[0] != "spotify"
                    || !["track", "album", "artist", "playlist", "show", "episode"]
                        .contains(&parts[1])
                {
                    return Err(invalid("unsupported Spotify URI"));
                }
                let i = id(&json!({"id":parts[2]}))?;
                Ok(json!({"url":format!("https://open.spotify.com/{}/{}",parts[1],i)}))
            }
            _ => Err(fail("unknown_method", format!("Unknown method {method}"))),
        }
    }
    /// Execute runtime commands directly so failures reach the requesting frontend.
    fn execute_command(&self, command: crate::command::Command) -> Result<Value> {
        use crate::command::{Command, SeekDirection};
        use std::time::Duration;
        let spotify = self.queue.get_spotify();
        match command {
            Command::Quit => {
                self.events.send(Event::Shutdown);
                return Ok(json!({"accepted":true,"completed":false}));
            }
            Command::Noop | Command::Redraw => {}
            Command::TogglePlay => self.queue.toggleplayback(),
            Command::Stop => self.queue.stop(),
            Command::Next => self.queue.next(true),
            Command::Previous => {
                if spotify.get_current_progress() < Duration::from_secs(5) {
                    self.queue.previous();
                } else {
                    spotify.seek(0);
                }
            }
            Command::Clear => self.queue.clear(),
            Command::UpdateLibrary => {
                self.library.update_library();
                return Ok(json!({"accepted":true,"completed":false}));
            }
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
            Command::Seek(SeekDirection::Relative(value)) => spotify.seek_relative(value),
            Command::Seek(SeekDirection::Absolute(value)) => spotify.seek(value),
            Command::VolumeUp(amount) => spotify.set_volume(
                spotify
                    .volume()
                    .saturating_add(655u16.saturating_mul(amount)),
                true,
            ),
            Command::VolumeDown(amount) => spotify.set_volume(
                spotify
                    .volume()
                    .saturating_sub(655u16.saturating_mul(amount)),
                true,
            ),
            Command::Discovery(Some(level)) => self.config.set_discovery(level),
            Command::Discovery(None) => return Ok(json!({"discovery":self.config.discovery()})),
            Command::ReloadConfig => self
                .config
                .reload()
                .map_err(|error| fail("config_error", error.to_string()))?,
            Command::Reconnect => spotify
                .start_worker(None)
                .map_err(|error| fail("session_error", error.to_string()))?,
            Command::Radio => return self.dispatch("radio.action", &json!({"action":"start"})),
            Command::RadioDebug => return self.dispatch("radio.debug", &json!({})),
            Command::Cast(true) => return self.cast_action(&json!({"action":"disconnect"})),
            Command::NewPlaylist(name) => {
                return self.playlist_action(&json!({"action":"create","name":name}));
            }
            Command::SaveCurrent => {
                let current = self
                    .queue
                    .get_current()
                    .ok_or_else(|| fail("not_ready", "No current track to save"))?;
                let track = current
                    .track()
                    .ok_or_else(|| fail("unsupported", "Saving an episode is unsupported"))?;
                let id = track.id.ok_or_else(|| {
                    fail("unsupported", "Local tracks cannot be saved to Spotify")
                })?;
                return self.library_action(&json!({"action":"save","kind":"track","id":id}));
            }
            other => {
                return Err(fail(
                    "unsupported_command",
                    format!("Command '{}' requires a frontend view", other.basename()),
                ));
            }
        }
        self.events.trigger();
        Ok(json!({"applied":true,"completed":true}))
    }

    fn detail(&self, p: &Value) -> Result<Value> {
        let kind = string(p, "kind")?;
        let i = id(p)?;
        let api = self.queue.get_spotify().api;
        match kind {
            "album" | "albums" => {
                let cached = self
                    .library
                    .albums
                    .read()
                    .unwrap()
                    .iter()
                    .find(|a| a.id.as_deref() == Some(&i))
                    .and_then(|a| a.complete_tracks().map(<[Track]>::to_vec));
                if let Some(tracks) = cached {
                    return page(tracks.iter().map(track).collect(), p, "cache");
                }
                let full = remote(api.album(&i))?;
                let (offset, limit) = bounds(p)?;
                let result = remote(api.album_tracks(&i, limit.min(50) as u32, offset as u32))?;
                let rows = result
                    .items
                    .iter()
                    .map(|t| track(&Track::from_simplified_track(t, &full)))
                    .collect::<Vec<_>>();
                Ok(
                    json!({"items":rows,"offset":offset,"limit":limit.min(50),"total":result.total,"has_more":result.next.is_some(),"source":"spotify"}),
                )
            }
            "artist" | "artists" => {
                let cached = self
                    .library
                    .artists
                    .read()
                    .unwrap()
                    .iter()
                    .find(|a| a.id.as_deref() == Some(&i))
                    .and_then(|a| a.tracks.clone());
                if let Some(tracks) = cached {
                    return page(tracks.iter().map(track).collect(), p, "cache");
                }
                let rows = remote(api.artist_top_tracks(&i))?
                    .iter()
                    .map(track)
                    .collect();
                page(rows, p, "spotify")
            }
            "playlist" | "playlists" => {
                let cached = self
                    .library
                    .playlists
                    .read()
                    .unwrap()
                    .iter()
                    .find(|a| a.id == i)
                    .cloned();
                let list = match cached {
                    Some(list) => list,
                    None => Playlist::from(&remote(api.playlist(&i))?),
                };
                let (offset, limit) = bounds(p)?;
                let limit = limit.min(100);
                let (items, total, source) = if let Some(tracks) = list.tracks {
                    (
                        tracks
                            .iter()
                            .filter(|item| {
                                item.list_index() >= offset
                                    && item.list_index() < offset.saturating_add(limit)
                            })
                            .map(playlist_item)
                            .collect::<Vec<_>>(),
                        list.num_tracks,
                        "cache",
                    )
                } else {
                    let result = remote(api.playlist_tracks_page(&i, offset as u32, limit as u32))?;
                    (
                        result.items.iter().map(playlist_item).collect(),
                        result.total as usize,
                        "spotify",
                    )
                };
                Ok(
                    json!({"items":items,"offset":offset,"limit":limit,"total":total,"has_more":offset.saturating_add(limit)<total,"source":source,"revision":list.snapshot_id}),
                )
            }
            "show" | "shows" => {
                let cached = self
                    .library
                    .shows
                    .read()
                    .unwrap()
                    .iter()
                    .find(|a| a.id == i)
                    .and_then(|s| s.episodes.clone());
                if let Some(episodes) = cached {
                    return page(episodes.iter().map(episode).collect(), p, "cache");
                }
                let result = api.show_episodes(&i);
                if result.failed() {
                    return Err(fail("upstream_error", "Episodes unavailable"));
                }
                let (offset, limit) = bounds(p)?;
                while result.items.read().unwrap().len() < offset.saturating_add(limit)
                    && !result.at_end()
                {
                    if result.next().is_none() {
                        return Err(fail("upstream_error", "Episode page failed"));
                    }
                }
                let rows = result.items.read().unwrap().iter().map(episode).collect();
                let mut page = page(rows, p, "spotify")?;
                page["total"] = json!(result.total);
                page["has_more"] = json!(offset.saturating_add(limit) < result.total as usize);
                Ok(page)
            }
            "track" | "tracks" => {
                let t = self.resolve(&format!("spotify:track:{i}"))?;
                page(vec![playable(&t)], p, "cache")
            }
            "browse" | "category" => self.browse(p, Some(&i)),
            _ => Err(invalid("unknown detail kind")),
        }
    }
    fn browse(&self, p: &Value, category: Option<&str>) -> Result<Value> {
        let api = self.queue.get_spotify().api;
        let (offset, limit) = bounds(p)?;
        macro_rules! paged {
            ($result:expr,$convert:expr) => {{
                let result = $result;
                if result.failed() {
                    return Err(fail("upstream_error", "Browse unavailable"));
                }
                while result.items.read().unwrap().len() < offset.saturating_add(limit)
                    && !result.at_end()
                {
                    if result.next().is_none() {
                        return Err(fail("upstream_error", "Browse page failed"));
                    }
                }
                let rows = result.items.read().unwrap().iter().map($convert).collect();
                let mut page = page(rows, p, "spotify")?;
                page["total"] = json!(result.total);
                page["has_more"] = json!(offset.saturating_add(limit) < result.total as usize);
                Ok(page)
            }};
        }
        match category {
            Some(i) => paged!(api.category_playlists(i), playlist),
            None => paged!(
                api.categories(),
                |c: &crate::model::category::Category| row(&c.id, "category", &c.name, "", None)
            ),
        }
    }
    fn search(&self, p: &Value) -> Result<Value> {
        // Use the same page size for local and remote results, so refreshing
        // cached rows and advancing a page cannot skip Spotify results.
        let (offset, limit) = search_bounds(p)?;
        let mut params = p.clone();
        params["limit"] = json!(limit);
        let p = &params;
        let query = string(p, "query")?.trim();
        let kind = p.get("kind").and_then(Value::as_str).unwrap_or("tracks");
        let ty = match kind {
            "tracks" | "track" => SearchType::Track,
            "albums" | "album" => SearchType::Album,
            "artists" | "artist" => SearchType::Artist,
            "playlists" | "playlist" => SearchType::Playlist,
            "shows" | "show" => SearchType::Show,
            "episodes" | "episode" => SearchType::Episode,
            _ => return Err(invalid("unsupported search kind")),
        };
        let cache_kind = match kind {
            "track" => "tracks",
            "album" => "albums",
            "artist" => "artists",
            "playlist" => "playlists",
            "show" => "shows",
            k => k,
        };
        let refresh = match p.get("refresh") {
            None => false,
            Some(value) => value
                .as_bool()
                .ok_or_else(|| invalid("refresh must be boolean"))?,
        };
        let prefix = format!(
            "search:{cache_kind}:{}:",
            crate::search_cache::normalize(query)
        );
        let key = format!("{prefix}{offset}:{limit}");
        if !refresh {
            if let Some(result) = self.cached_page(&key) {
                return Ok(result);
            }
            let has_remote_result = self.pages.lock().unwrap().iter().any(|(key, page)| {
                key.starts_with(&prefix) && page.fetched_at.elapsed() < PAGE_CACHE_TTL
            });
            if !has_remote_result {
                if cache_kind == "tracks" {
                    let tracks = crate::search_cache::shared().best_effort(query);
                    if !tracks.is_empty() {
                        let mut result =
                            page(tracks.iter().map(track).collect(), p, "search_cache")?;
                        result["refresh_available"] = json!(true);
                        return Ok(result);
                    }
                }
                let f = query.to_lowercase();
                let mut cached = self.cached(cache_kind).unwrap_or_default();
                cached.retain(|r| {
                    format!("{} {}", r["title"], r["subtitle"])
                        .to_lowercase()
                        .contains(&f)
                });
                if !cached.is_empty() {
                    let mut result = page(cached, p, "cache")?;
                    result["refresh_available"] = json!(true);
                    return Ok(result);
                }
            }
        }
        let response = self
            .queue
            .get_spotify()
            .api
            .search_detailed(ty, query, limit as u32, offset as u32)
            .map_err(|error| fail(error.code, error.message))?;
        let (rows, total) = match response {
            SearchResult::Tracks(r) => {
                let tracks = r.items.iter().map(Track::from).collect::<Vec<_>>();
                if offset == 0 {
                    let cache = crate::search_cache::shared();
                    cache.store(query, tracks.clone());
                    cache.save();
                }
                (tracks.iter().map(track).collect::<Vec<_>>(), r.total)
            }
            SearchResult::Albums(r) => (
                r.items.iter().map(|t| album(&Album::from(t))).collect(),
                r.total,
            ),
            SearchResult::Artists(r) => (
                r.items.iter().map(|t| artist(&Artist::from(t))).collect(),
                r.total,
            ),
            SearchResult::Playlists(r) => (
                r.items
                    .iter()
                    .map(|t| playlist(&Playlist::from(t)))
                    .collect(),
                r.total,
            ),
            SearchResult::Shows(r) => (
                r.items.iter().map(|t| show(&Show::from(t))).collect(),
                r.total,
            ),
            SearchResult::Episodes(r) => (
                r.items.iter().map(|t| episode(&Episode::from(t))).collect(),
                r.total,
            ),
        };
        let count = rows.len();
        let result = json!({"items":rows,"offset":offset,"limit":limit,"total":total,"has_more":count > 0 && offset.saturating_add(count) <= 1000 && offset.saturating_add(count)<total as usize,"source":"spotify"});
        self.remember_page(key, &result);
        Ok(result)
    }
    fn player_action(&self, p: &Value) -> Result<Value> {
        let spotify = self.queue.get_spotify();
        match string(p, "action")? {
            "play_pause" => self.queue.toggleplayback(),
            "previous" => self.queue.previous(),
            "next" => self.queue.next(true),
            "stop" => self.queue.stop(),
            "seek" => spotify.seek(number(p, "value", 0, u32::MAX as usize)? as u32),
            "volume" => spotify.set_volume(
                ((number(p, "value", 50, 100)? as u64 * 65535) / 100) as u16,
                true,
            ),
            "repeat" => self.queue.set_repeat(
                match p.get("value").and_then(Value::as_str).unwrap_or("none") {
                    "off" | "none" => RepeatSetting::None,
                    "track" => RepeatSetting::RepeatTrack,
                    "all" | "playlist" | "queue" => RepeatSetting::RepeatPlaylist,
                    _ => return Err(invalid("repeat value must be none, track or playlist")),
                },
            ),
            "shuffle" => {
                let value = p
                    .get("value")
                    .and_then(Value::as_bool)
                    .ok_or_else(|| invalid("shuffle value must be boolean"))?;
                self.queue.set_shuffle(value)
            }
            "play" => {
                if let Some(uri) = p.get("uri").and_then(Value::as_str) {
                    let tracks = self.context(uri)?;
                    self.queue.rpc_append_play_many(&tracks);
                } else {
                    spotify.play();
                }
            }
            _ => return Err(invalid("unknown player action")),
        }
        Ok(json!({"accepted":true}))
    }
    fn player_artwork(&self, p: &Value) -> Result<Value> {
        let width = dimension(p, "width", artwork::DEFAULT_WIDTH, artwork::MAX_WIDTH)?;
        let height = dimension(p, "height", artwork::DEFAULT_HEIGHT, artwork::MAX_HEIGHT)?;
        let want_image = match p.get("format") {
            None | Some(Value::Null) => false,
            Some(Value::String(format)) if format == "pixels" => false,
            Some(Value::String(format)) if format == "png" => true,
            Some(_) => return Err(invalid("format must be pixels or png")),
        };
        let requested_uri = match p.get("uri") {
            None | Some(Value::Null) => None,
            Some(Value::String(uri)) if !uri.trim().is_empty() => Some(uri.clone()),
            Some(_) => return Err(invalid("uri must be a nonempty string when provided")),
        };

        let current = self.queue.get_current();
        let current_uri = current.as_ref().map(Playable::uri);
        let uri = match requested_uri.as_deref() {
            Some(requested) if current_uri.as_deref() != Some(requested) => {
                return Ok(unavailable_artwork(
                    Some(requested),
                    width,
                    height,
                    "current_mismatch",
                ));
            }
            Some(requested) => requested.to_owned(),
            None => match current_uri {
                Some(uri) => uri,
                None => return Ok(unavailable_artwork(None, width, height, "no_current_track")),
            },
        };
        let Some(current) = current else {
            return Ok(unavailable_artwork(
                Some(&uri),
                width,
                height,
                "no_current_track",
            ));
        };
        let Some(cover_url) = current.cover_url().filter(|url| !url.trim().is_empty()) else {
            return Ok(unavailable_artwork(Some(&uri), width, height, "no_cover"));
        };

        let artwork = match self
            .artwork
            .get_or_fetch_with_image(&cover_url, width, height, want_image)
        {
            Ok(artwork) => artwork,
            Err(error) => {
                return Ok(unavailable_artwork(
                    Some(&uri),
                    width,
                    height,
                    error.reason(),
                ));
            }
        };

        // Network/decode work can outlive a queue transition.  Do not return
        // art for the track that was current when the request began.
        let still_current = self
            .queue
            .get_current()
            .is_some_and(|current| current.uri() == uri);
        if !still_current {
            return Ok(unavailable_artwork(
                Some(&uri),
                width,
                height,
                "stale_current",
            ));
        }
        let mut response = json!({
            "available": true,
            "uri": uri,
            "width": width,
            "height": height,
            "pixels": artwork.pixels,
        });
        if let Some(image) = artwork.image {
            response["image"] = json!({
                "mime": "image/png",
                "width": image.width,
                "height": image.height,
                "data": image.data,
            });
        }
        Ok(response)
    }
    fn queue_action(&self, p: &Value) -> Result<Value> {
        let action = string(p, "action")?;
        match action {
            "play" | "remove" | "move" | "clear" => {
                let revision = string(p, "revision")?;
                let entry = if action == "clear" {
                    None
                } else {
                    Some(string(p, "entry_id")?)
                };
                let to = if action == "move" {
                    Some(number(p, "to", 0, usize::MAX)?)
                } else {
                    None
                };
                self.queue
                    .rpc_checked_action(action, revision, entry, to)
                    .map_err(|e| fail("stale_queue", e))?;
            }
            "append" | "play_next" => {
                let tracks = self.context(string(p, "uri")?)?;
                if action == "append" {
                    self.queue.rpc_append_many(&tracks)
                } else {
                    self.queue.append_next(&tracks);
                }
            }
            "save" => {
                let name = string(p, "name")?;
                let tracks = self.queue.queue.read().unwrap().clone();
                let i = self.create_playlist(name)?;
                for chunk in tracks.chunks(100) {
                    remote(self.queue.get_spotify().api.append_tracks(&i, chunk, None))?;
                }
                self.library.update_library();
                return Ok(json!({"id":i}));
            }
            _ => return Err(invalid("unknown queue action")),
        }
        Ok(json!({"applied":true}))
    }
    fn create_playlist(&self, name: &str) -> Result<String> {
        if self.library.user_id().is_none() {
            return Err(fail("not_ready", "User identity is loading"));
        }
        remote(
            self.queue
                .get_spotify()
                .api
                .create_playlist(name, Some(false), None),
        )
    }
    fn playlist_action(&self, p: &Value) -> Result<Value> {
        let api = self.queue.get_spotify().api;
        let action = string(p, "action")?;
        if action == "create" {
            let i = self.create_playlist(string(p, "name")?)?;
            self.library.update_library();
            return Ok(json!({"id":i}));
        }
        let i = id(p)?;
        let transaction = self
            .playlist_mutations
            .lock()
            .unwrap()
            .entry(i.clone())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone();
        let _transaction = transaction.lock().unwrap();
        match action {
            "rename" => remote(api.rename_playlist(&i, string(p, "name")?))?,
            "delete" => {
                remote(api.delete_playlist(&i))?;
                self.library
                    .playlists
                    .write()
                    .unwrap()
                    .retain(|list| list.id != i);
            }
            "add" => {
                let t = self.resolve(string(p, "uri")?)?;
                let position = p
                    .get("position")
                    .map(|_| number(p, "position", 0, u32::MAX as usize))
                    .transpose()?
                    .map(|n| n as u32);
                remote(api.append_tracks(&i, &[t], position))?;
            }
            "remove" => {
                if p.get("position").is_none() {
                    return Err(invalid("position required"));
                }
                let position = number(p, "position", 0, u32::MAX as usize)?;
                let list = Playlist::from(&remote(api.playlist(&i))?);
                if let Some(snapshot) = p
                    .get("snapshot_id")
                    .or_else(|| p.get("revision"))
                    .and_then(Value::as_str)
                    && snapshot != list.snapshot_id
                {
                    return Err(fail(
                        "stale_playlist",
                        "Playlist changed; refresh before editing",
                    ));
                }
                let tracks = remote(api.playlist_tracks_page(&i, position as u32, 1))?;
                let mut t = tracks
                    .items
                    .into_iter()
                    .find(|t| t.list_index() == position)
                    .ok_or_else(|| invalid("playlist position unavailable"))?;
                if let Some(uri) = p.get("uri").and_then(Value::as_str)
                    && t.uri() != uri
                {
                    return Err(fail("stale_playlist", "Track at position has changed"));
                }
                t.set_list_index(position);
                remote(api.delete_tracks(&i, &list.snapshot_id, &[t]))?;
            }
            _ => return Err(invalid("unknown playlist action")),
        }
        self.pages
            .lock()
            .unwrap()
            .retain(|key, _| key.starts_with("search:") && !key.starts_with("search:playlists:"));
        self.library.update_library();
        Ok(json!({"applied":true}))
    }
    fn library_action(&self, p: &Value) -> Result<Value> {
        let action = string(p, "action")?;
        if action == "refresh" {
            self.library.update_library();
            return Ok(json!({"accepted":true,"completed":false}));
        }
        if action != "save" && action != "unsave" {
            return Err(invalid("unknown library action"));
        }
        let save = action == "save";
        let i = id(p)?;
        let api = self.queue.get_spotify().api;
        match string(p, "kind")? {
            "track" | "tracks" => {
                if save {
                    let t = self
                        .resolve(&format!("spotify:track:{i}"))?
                        .track()
                        .ok_or_else(|| invalid("expected a track"))?;
                    remote(api.current_user_saved_tracks_add(vec![&i]))?;
                    let mut tracks = self.library.tracks.write().unwrap();
                    if !tracks.iter().any(|track| track.id.as_deref() == Some(&i)) {
                        tracks.push(t);
                    }
                } else {
                    remote(api.current_user_saved_tracks_delete(vec![&i]))?;
                    self.library
                        .tracks
                        .write()
                        .unwrap()
                        .retain(|t| t.id.as_deref() != Some(&i));
                }
            }
            "album" | "albums" => {
                if save {
                    let a = Album::from(&remote(api.album(&i))?);
                    remote(api.current_user_saved_albums_add(vec![&i]))?;
                    let mut albums = self.library.albums.write().unwrap();
                    if !albums.iter().any(|a| a.id.as_deref() == Some(&i)) {
                        albums.push(a);
                    }
                } else {
                    remote(api.current_user_saved_albums_delete(vec![&i]))?;
                    self.library
                        .albums
                        .write()
                        .unwrap()
                        .retain(|a| a.id.as_deref() != Some(&i));
                }
            }
            "artist" | "artists" => {
                if save {
                    let mut a = Artist::from(&remote(api.artist(&i))?);
                    a.is_followed = true;
                    remote(api.user_follow_artists(vec![&i]))?;
                    let mut artists = self.library.artists.write().unwrap();
                    if !artists.iter().any(|a| a.id.as_deref() == Some(&i)) {
                        artists.push(a);
                    }
                } else {
                    remote(api.user_unfollow_artists(vec![&i]))?;
                    self.library
                        .artists
                        .write()
                        .unwrap()
                        .retain(|a| a.id.as_deref() != Some(&i));
                }
            }
            "playlist" | "playlists" => {
                if save {
                    remote(api.user_playlist_follow_playlist(&i))?;
                } else {
                    remote(api.delete_playlist(&i))?;
                }
            }
            "show" | "shows" => {
                if save {
                    let s = Show::from(&remote(api.show(&i))?);
                    remote(api.save_shows(&[&i]))?;
                    let mut shows = self.library.shows.write().unwrap();
                    if !shows.iter().any(|s| s.id == i) {
                        shows.push(s);
                    }
                } else {
                    remote(api.unsave_shows(&[&i]))?;
                    self.library.shows.write().unwrap().retain(|s| s.id != i);
                }
            }
            _ => return Err(invalid("unknown library kind")),
        }
        self.pages
            .lock()
            .unwrap()
            .retain(|key, _| key.starts_with("search:") && !key.starts_with("search:playlists:"));
        self.library.update_library();
        Ok(json!({"applied":true}))
    }
    fn cast_list(&self, p: &Value) -> Result<Value> {
        let hosts = self.config.values().roku_hosts.clone().unwrap_or_default();
        let targets = crate::cast::targets(&self.queue.get_spotify().api, &hosts);
        let rows=targets.iter().map(|target|match target {
            crate::cast::Target::Connect{id,name,kind}=>{let mut r=row(id,"device",name,kind,None);r["meta"]=json!({"type":kind,"active":self.queue.get_spotify().cast_target().as_deref()==Some(id),"connectable":true});r},
            crate::cast::Target::Roku(r)=>{let mut row=row(&r.base,"roku",&r.name,"Roku",None);let missing=r.spotify==crate::cast::roku::SpotifyApp::Missing;row["meta"]=json!({"type":"roku","active":false,"connectable":!missing,"spotify":if missing {"missing"} else {"available"},"reason":if missing {Some("Install Spotify on the Roku to cast")} else {None}});row},
        }).collect();
        *self.casts.lock().unwrap() = targets;
        page(rows, p, "discovery")
    }
    fn cast_action(&self, p: &Value) -> Result<Value> {
        let spotify = self.queue.get_spotify();
        let resume = self.queue.get_current().map(|t| {
            (
                t,
                spotify
                    .get_current_progress()
                    .as_millis()
                    .min(u32::MAX as u128) as u32,
                matches!(
                    spotify.get_current_status(),
                    crate::spotify::PlayerEvent::Playing(_)
                ),
            )
        });
        match string(p, "action")? {
            "disconnect" => spotify.stop_cast(resume),
            "connect" => {
                let i = string(p, "id")?;
                if self.casts.lock().unwrap().is_empty() {
                    self.cast_list(&json!({}))?;
                }
                let target = self
                    .casts
                    .lock()
                    .unwrap()
                    .iter()
                    .find(|t| match t {
                        crate::cast::Target::Connect { id, .. } => id == i,
                        crate::cast::Target::Roku(r) => r.base == i,
                    })
                    .cloned()
                    .ok_or_else(|| fail("not_found", "Cast device unavailable; refresh devices"))?;
                let (id, name) = crate::cast::connect(&spotify.api, &target, &|message| {
                    self.events.notify(message)
                })
                .map_err(|e| fail("cast_error", e))?;
                spotify.cast_to(id, name, resume);
            }
            _ => return Err(invalid("unknown cast action")),
        }
        Ok(json!({"accepted":true}))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::artwork::ArtworkImage;
    fn service() -> RpcService {
        let config = Config::new_for_test();
        let events = EventManager::new();
        let spotify = crate::spotify::Spotify::new_for_test(config.clone(), events.clone());
        let library = Library::new_for_test(events.clone(), spotify.clone(), config.clone());
        let queue = Arc::new(Queue::new_for_test(
            vec![],
            None,
            spotify,
            config.clone(),
            library.clone(),
        ));
        RpcService::new(queue, library, config, events)
    }
    fn fixture_track(identity: &str, title: &str, index: usize) -> Track {
        Track {
            id: Some(identity.to_owned()),
            uri: format!("spotify:track:{identity}"),
            title: title.to_owned(),
            track_number: 1,
            disc_number: 1,
            duration: 180_000,
            artists: vec!["Fixture Artist".to_owned()],
            artist_ids: vec![],
            album: Some("Fixture Album".to_owned()),
            album_id: Some("FixtureAlbum".to_owned()),
            album_artists: vec![],
            cover_url: None,
            url: String::new(),
            added_at: None,
            list_index: index,
            is_local: false,
            is_playable: Some(true),
        }
    }
    #[test]
    fn radio_status_distinguishes_parked_context_explicit_queue_and_full_catalog() {
        let service = service();
        let seed = fixture_track("StatusStationSeed", "Station Seed", 0);
        service.queue.queue.write().unwrap().extend([
            Playable::Track(seed.clone()),
            Playable::Track(fixture_track("StatusContext", "Previous Playlist", 1)),
        ]);
        service.queue.play(0, false, false);
        service.library.tracks.write().unwrap().push(seed.clone());
        service.queue.start_radio_track(&seed);
        service
            .queue
            .rpc_append_many(&[Playable::Track(fixture_track(
                "StatusExplicit",
                "My Choice",
                2,
            ))]);
        let result = service.dispatch("radio.status", &json!({})).unwrap();
        assert_eq!(result["queue_mode"], "station");
        assert_eq!(result["parked_count"], 1);
        assert_eq!(result["radio_pending_count"], 0);
        assert_eq!(result["explicit_pending_count"], 1);
        assert_eq!(result["cache_tracks"], 1);
        assert!(result["catalog_tracks"].as_u64().unwrap() >= 3);
        assert_eq!(service.queue.queue.read().unwrap().len(), 3);
        service.queue.cancel_radio();
        let stopped = service.dispatch("radio.status", &json!({})).unwrap();
        assert_eq!(stopped["queue_mode"], "context");
        assert_eq!(stopped["parked_count"], 0);
        // Your queued choice keeps priority, then the parked context resumes.
        assert_eq!(service.queue.next_index(), Some(2));
        service.queue.play(2, false, false);
        assert_eq!(service.queue.next_index(), Some(1));
    }

    #[test]
    fn cached_radio_seed_track_uses_queue_then_library_and_handles_misses() {
        let service = service();
        let queue_seed = fixture_track("QueueSeed", "Queue Seed", 0);
        let mut library_copy = queue_seed.clone();
        library_copy.title = "Library Copy".to_owned();
        let library_seed = fixture_track("LibrarySeed", "Library Seed", 1);
        service
            .queue
            .queue
            .write()
            .unwrap()
            .push(Playable::Track(queue_seed.clone()));
        service
            .library
            .tracks
            .write()
            .unwrap()
            .extend([library_copy, library_seed.clone()]);

        assert_eq!(
            cached_radio_seed_track(&service.queue, &service.library, Some(&queue_seed.uri))
                .map(|track| serde_json::to_value(track).unwrap()),
            Some(serde_json::to_value(&queue_seed).unwrap())
        );
        assert_eq!(
            cached_radio_seed_track(&service.queue, &service.library, Some(&library_seed.uri),)
                .map(|track| serde_json::to_value(track).unwrap()),
            Some(serde_json::to_value(&library_seed).unwrap())
        );
        assert!(
            cached_radio_seed_track(
                &service.queue,
                &service.library,
                Some("spotify:track:MissingSeed"),
            )
            .is_none()
        );
        assert!(cached_radio_seed_track(&service.queue, &service.library, None).is_none());
    }

    #[test]
    fn radio_status_reports_seed_track_when_playback_has_moved() {
        let service = service();
        let seed = fixture_track("RadioSeed", "Radio Seed", 0);
        let current = fixture_track("RadioCurrent", "Radio Current", 1);
        service
            .queue
            .rpc_append_play_many(&[Playable::Track(seed.clone()), Playable::Track(current)]);
        service.queue.start_radio(&seed.uri);
        service.queue.play(1, false, false);

        let result = service.dispatch("radio.status", &json!({})).unwrap();
        assert_eq!(result["seed"], seed.uri);
        assert_eq!(result["seed_track"], serde_json::to_value(seed).unwrap());
    }

    #[test]
    fn remote_page_cache_is_bounded_expires_and_skips_fetching() {
        let service = service();
        let p = json!({"kind":"album","id":"FixtureAlbum"});
        let key = format!("detail:{p}");
        service.remember_page(key.clone(), &json!({"items":[],"source":"spotify"}));
        let result = service
            .cached_remote_page("detail", &p, || panic!("cached page must not fetch"))
            .unwrap();
        assert_eq!(result["source"], "memory_cache");
        service
            .pages
            .lock()
            .unwrap()
            .get_mut(&key)
            .unwrap()
            .fetched_at = Instant::now() - PAGE_CACHE_TTL - Duration::from_secs(1);
        assert!(service.cached_page(&key).is_none());
        for i in 0..(PAGE_CACHE_LIMIT + 2) {
            service.remember_page(format!("key{i}"), &json!({"items":[],"source":"spotify"}));
        }
        assert_eq!(service.pages.lock().unwrap().len(), PAGE_CACHE_LIMIT);
    }
    #[test]
    fn cached_search_announces_refresh_and_saved_domains_are_explicit() {
        let service = service();
        let alpha = fixture_track("FixtureAlpha", "Fixture Alpha", 0);
        service.library.tracks.write().unwrap().push(alpha.clone());
        let page = service.search(&json!({"query":"Fixture Alpha"})).unwrap();
        assert_eq!(page["refresh_available"], true);
        assert_eq!(page["source"], "cache");
        assert!(
            service
                .search(&json!({"query":"Fixture Alpha","refresh":"yes"}))
                .is_err()
        );
        let error = service
            .search(&json!({"query":"Fixture Alpha","refresh":true}))
            .unwrap_err();
        assert_eq!(
            error.code, "upstream_error",
            "refresh bypasses local rows and asks the explicitly offline test API"
        );
        assert_eq!(service.queue.len(), 0);
        let saved_album:Album=serde_json::from_value(json!({"id":"FixtureAlbum","title":"Fixture Album","artists":["Fixture Artist"],"artist_ids":[],"year":"2026","cover_url":null,"url":null,"tracks":[alpha],"added_at":null,"total_tracks":1})).unwrap();
        service
            .library
            .albums
            .write()
            .unwrap()
            .push(saved_album.clone());
        let mut result =
            json!({"items":[album(&saved_album),row("MissingAlbum","album","Missing","",None)]});
        service.decorate(&mut result);
        assert_eq!(result["items"][0]["saved"], true);
        assert_eq!(result["items"][1]["saved"], false);
    }
    #[test]
    fn incomplete_cached_album_requires_full_resolution_before_queue_edit() {
        let service = service();
        let alpha = fixture_track("FixtureAlpha", "Alpha", 0);
        let mut album:Album=serde_json::from_value(json!({"id":"PartialAlbum","title":"Partial Album","artists":["Fixture Artist"],"artist_ids":[],"year":"2026","cover_url":null,"url":null,"tracks":[alpha],"added_at":null,"total_tracks":2})).unwrap();
        assert!(album.complete_tracks().is_none());
        let cached = serde_json::to_value(&album).unwrap();
        album.load_all_tracks(service.queue.get_spotify());
        assert_eq!(
            serde_json::to_value(&album).unwrap(),
            cached,
            "failed metadata refresh preserves partial cache and known total"
        );
        service.library.albums.write().unwrap().push(album);
        let before = service.queue.rpc_snapshot().0;
        let error = service
            .player_action(&json!({"action":"play","uri":"spotify:album:PartialAlbum"}))
            .unwrap_err();
        assert_eq!(error.code, "upstream_error");
        assert_eq!(service.queue.rpc_snapshot().0, before);
        assert_eq!(
            service
                .detail(&json!({"kind":"album","id":"PartialAlbum"}))
                .unwrap_err()
                .code,
            "upstream_error"
        );
    }
    #[test]
    fn cached_context_play_and_queue_preserve_duplicate_occurrences() {
        let service = service();
        let before = service.queue.rpc_snapshot().0;
        assert!(
            service
                .player_action(&json!({"action":"play","uri":"spotify:album:MissingAlbum"}))
                .is_err()
        );
        assert!(
            service
                .queue_action(&json!({"action":"append","uri":"spotify:playlist:MissingPlaylist"}))
                .is_err()
        );
        assert_eq!(
            service.queue.rpc_snapshot().0,
            before,
            "failed full-context resolution must leave queue unchanged"
        );
        let alpha = fixture_track("FixtureAlpha", "Alpha", 0);
        let mut duplicate = alpha.clone();
        duplicate.list_index = 1;
        let beta = fixture_track("FixtureBeta", "Beta", 2);
        service.library.playlists.write().unwrap().push(Playlist {
            id: "FixturePlaylist".to_owned(),
            name: "Fixture Playlist".to_owned(),
            owner_id: "FixtureOwner".to_owned(),
            owner_name: None,
            snapshot_id: "fixture1".to_owned(),
            num_tracks: 3,
            tracks: Some(vec![
                Playable::Track(alpha),
                Playable::Track(duplicate),
                Playable::Track(beta),
            ]),
            collaborative: false,
        });
        service
            .player_action(&json!({"action":"play","uri":"spotify:playlist:FixturePlaylist"}))
            .unwrap();
        assert_eq!(service.queue.len(), 3);
        assert_eq!(service.queue.get_current_index(), Some(0));
        let (_, items, _) = service.queue.rpc_snapshot();
        assert_eq!(items[0].uri(), items[1].uri());
        service
            .queue_action(&json!({"action":"play_next","uri":"spotify:playlist:FixturePlaylist"}))
            .unwrap();
        assert_eq!(service.queue.len(), 6);
        service
            .queue_action(&json!({"action":"append","uri":"spotify:playlist:FixturePlaylist"}))
            .unwrap();
        assert_eq!(service.queue.len(), 9);
        assert!(service.context("spotify:show:FixtureShow").is_err());
        assert_eq!(service.queue.len(), 9);
        let alpha = fixture_track("FixtureAlpha", "Alpha", 0);
        let beta = fixture_track("FixtureBeta", "Beta", 1);
        let album:Album=serde_json::from_value(json!({"id":"FixtureAlbum","title":"Fixture Album","artists":["Fixture Artist"],"artist_ids":[],"year":"2026","cover_url":null,"url":null,"tracks":[alpha.clone(),beta.clone()],"added_at":null,"total_tracks":2})).unwrap();
        service.library.albums.write().unwrap().push(album);
        let mut artist = Artist::new("FixtureArtist".to_owned(), "Fixture Artist".to_owned());
        artist.tracks = Some(vec![alpha, beta]);
        service.library.artists.write().unwrap().push(artist);
        service
            .player_action(&json!({"action":"play","uri":"spotify:album:FixtureAlbum"}))
            .unwrap();
        assert_eq!(service.queue.len(), 11);
        assert_eq!(service.queue.get_current_index(), Some(9));
        service
            .queue_action(&json!({"action":"play_next","uri":"spotify:artist:FixtureArtist"}))
            .unwrap();
        assert_eq!(service.queue.len(), 13);
    }
    #[test]
    fn settings_commands_mutate_synchronously_and_reject_view_commands() {
        let service = service();
        let response=service.handle_line(r#"{"protocol":"resonance","version":1,"id":1,"method":"settings.action","params":{"action":"command","command":"discovery 73"}}"#);
        assert_eq!(response["ok"], true);
        assert_eq!(service.config.discovery(), 73);
        assert_eq!(response["result"]["completed"], true);
        let response=service.handle_line(r#"{"protocol":"resonance","version":1,"id":2,"method":"settings.action","params":{"action":"command","command":"help"}}"#);
        assert_eq!(response["error"]["code"], "unsupported_command");
        assert!(
            !service
                .events
                .msg_iter()
                .any(|event| matches!(event, Event::IpcInput(_)))
        );
        let response=service.handle_line(r#"{"protocol":"resonance","version":1,"id":3,"method":"settings.action","params":{"action":"command","command":"quit"}}"#);
        assert_eq!(response["ok"], true);
        assert_eq!(response["result"]["completed"], false);
        assert!(
            service
                .events
                .msg_iter()
                .any(|event| matches!(event, Event::Shutdown))
        );
    }
    #[test]
    fn queue_clear_requires_the_exact_snapshot() {
        let service = service();
        let page = service.dispatch("queue.list", &json!({})).unwrap();
        let revision = page["revision"].as_str().unwrap();
        assert!(
            service
                .dispatch("queue.action", &json!({"action":"clear","revision":"old"}))
                .is_err()
        );
        assert!(
            service
                .dispatch(
                    "queue.action",
                    &json!({"action":"clear","revision":revision})
                )
                .is_ok()
        );
        assert!(
            service
                .dispatch(
                    "queue.action",
                    &json!({"action":"clear","revision":revision})
                )
                .is_err()
        );
    }
    #[test]
    fn protocol_validation_preserves_correlation() {
        let service = service();
        let r = service.handle_line(
            r#"{"protocol":"resonance","version":2,"id":"hello","method":"session.info"}"#,
        );
        assert_eq!(r["id"], "hello");
        assert_eq!(r["error"]["code"], "unsupported_version");
        let r = service
            .handle_line(r#"{"protocol":"resonance","version":1,"id":42,"method":"missing"}"#);
        assert_eq!(r["error"]["code"], "unknown_method");
        assert_eq!(r["id"], 42);
        assert_eq!(service.handle_line("{")["error"]["code"], "invalid_request");
    }
    #[test]
    fn pagination_and_bounds_are_safe() {
        let rows = (0..5).map(|n| json!({"id":n})).collect();
        let p = page(rows, &json!({"offset":3,"limit":2}), "cache").unwrap();
        assert_eq!(p["total"], 5);
        assert_eq!(p["items"][0]["id"], 3);
        assert_eq!(p["has_more"], false);
        assert!(bounds(&json!({"offset":-1})).is_err());
        assert!(bounds(&json!({"limit":0})).is_err());
        assert!(bounds(&json!({"limit":201})).is_err());
    }
    #[test]
    fn search_pages_follow_spotify_bounds_even_for_cached_results() {
        assert_eq!(search_bounds(&json!({})).unwrap(), (0, 10));
        assert_eq!(search_bounds(&json!({"limit":20})).unwrap(), (0, 10));
        assert_eq!(
            search_bounds(&json!({"offset":10,"limit":5})).unwrap(),
            (10, 5)
        );
        assert!(search_bounds(&json!({"offset":1001})).is_err());
        assert!(search_bounds(&json!({"limit":0})).is_err());
        let service = service();
        for n in 0..25 {
            service.library.tracks.write().unwrap().push(fixture_track(
                &format!("SearchPage{n}"),
                &format!("Search Paging {n}"),
                n,
            ));
        }
        let first = service
            .search(&json!({"query":"Search Paging","limit":20}))
            .unwrap();
        let next = service
            .search(&json!({"query":"Search Paging","offset":10,"limit":20}))
            .unwrap();
        assert_eq!(first["limit"], 10);
        assert_eq!(first["items"].as_array().unwrap().len(), 10);
        assert_eq!(next["items"].as_array().unwrap().len(), 10);
        assert_eq!(next["items"][0]["id"], "SearchPage10");
        assert_eq!(next["has_more"], true);
        assert_eq!(service.queue.len(), 0);
    }

    #[test]
    fn artwork_without_current_track_returns_a_clean_unavailable_result() {
        let service = service();
        let result = service
            .dispatch("player.artwork", &json!({"width":2,"height":1}))
            .unwrap();
        assert_eq!(result["available"], false);
        assert_eq!(result["uri"], Value::Null);
        assert_eq!(result["width"], 2);
        assert_eq!(result["height"], 1);
        assert_eq!(result["reason"], "no_current_track");
    }

    #[test]
    fn artwork_rejects_zero_and_oversized_dimensions() {
        let service = service();
        for params in [
            json!({"width":0,"height":1}),
            json!({"width":41,"height":1}),
            json!({"width":1,"height":0}),
            json!({"width":1,"height":21}),
        ] {
            let error = service.dispatch("player.artwork", &params).unwrap_err();
            assert_eq!(error.code, "invalid_params");
        }
    }

    #[test]
    fn artwork_rejects_unsupported_format() {
        let service = service();
        for format in [json!("jpg"), json!(""), json!(true)] {
            let error = service
                .dispatch("player.artwork", &json!({"format":format}))
                .unwrap_err();
            assert_eq!(error.code, "invalid_params");
        }
    }

    #[test]
    fn artwork_cache_hit_returns_exact_requested_grid() {
        let service = service();
        let mut current = fixture_track("ArtworkTrack", "Artwork", 0);
        current.cover_url = Some("https://i.scdn.co/image/artwork-fixture".to_owned());
        let uri = current.uri.clone();
        service
            .queue
            .rpc_append_play_many(&[Playable::Track(current)]);
        service.artwork.remember_for_test(
            "https://i.scdn.co/image/artwork-fixture",
            2,
            1,
            vec!["#FF0000", "#00FF00", "#0000FF", "#FFFFFF"]
                .into_iter()
                .map(String::from)
                .collect(),
        );

        let result = service
            .dispatch("player.artwork", &json!({"uri":uri,"width":2,"height":1}))
            .unwrap();
        assert_eq!(result["available"], true);
        assert_eq!(result["uri"], uri);
        assert_eq!(result["width"], 2);
        assert_eq!(result["height"], 1);
        assert_eq!(result["pixels"].as_array().unwrap().len(), 4);
        assert_eq!(result["pixels"][0], "#FF0000");
        assert_eq!(result["pixels"][3], "#FFFFFF");
        assert!(result.get("image").is_none());

        service.artwork.remember_image_for_test(
            "https://i.scdn.co/image/artwork-fixture",
            ArtworkImage {
                width: 2,
                height: 2,
                data: "cG5n".into(),
            },
        );
        let png = service
            .dispatch(
                "player.artwork",
                &json!({"uri":uri,"width":2,"height":1,"format":"png"}),
            )
            .unwrap();
        assert_eq!(png["pixels"], result["pixels"]);
        assert_eq!(png["image"]["mime"], "image/png");
        assert_eq!(png["image"]["width"], 2);
        assert_eq!(png["image"]["height"], 2);
        assert_eq!(png["image"]["data"], "cG5n");
    }

    #[test]
    fn artwork_uri_mismatch_never_reads_another_current_cover() {
        let service = service();
        let current = fixture_track("CurrentArtworkTrack", "Current", 0);
        service
            .queue
            .rpc_append_play_many(&[Playable::Track(current)]);
        let result = service
            .dispatch(
                "player.artwork",
                &json!({"uri":"spotify:track:stale","width":2,"height":1}),
            )
            .unwrap();
        assert_eq!(result["available"], false);
        assert_eq!(result["reason"], "current_mismatch");
        assert_eq!(result["uri"], "spotify:track:stale");
    }
}
