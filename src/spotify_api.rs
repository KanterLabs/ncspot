use std::sync::{Arc, Mutex, RwLock};
use std::thread;
use std::time::{Duration, Instant};

use chrono::{DateTime, Duration as ChronoDuration, Utc};
use log::{debug, error, info};
use rspotify::http::HttpError;
use rspotify::model::{
    AdditionalType, AlbumId, AlbumType, ArtistId, CurrentPlaybackContext, CursorBasedPage, Device,
    EpisodeId, FullAlbum, FullArtist, FullEpisode, FullPlaylist, FullShow, FullTrack,
    ItemPositions, LibraryId, Market, Page, PlayableId, PlaylistId, PlaylistResult, PrivateUser,
    SavedAlbum, SavedTrack, SearchResult, SearchType, Show, ShowId, SimplifiedTrack, TrackId,
    UserId,
};
use rspotify::{AuthCodePkceSpotify, ClientError, ClientResult, Config, prelude::*};
use tokio::sync::mpsc;

use crate::model::album::Album;
use crate::model::artist::Artist;
use crate::model::category::Category;
use crate::model::episode::Episode;
use crate::model::playable::Playable;
use crate::model::playlist::Playlist;
use crate::model::track::Track;
use crate::spotify_worker::WorkerCommand;
use crate::ui::pagination::{ApiPage, ApiResult};

/// The longest a rate limit will be waited out before the call is abandoned instead.
const MAX_RETRY_AFTER_SECS: u64 = 10;
/// Added to a rate limit window before retrying, so the retry lands after it has
/// expired rather than on the boundary.
const RATE_LIMIT_GRACE: Duration = Duration::from_secs(1);

/// Spotify's current Development Mode limit for a Search API page.
pub const SEARCH_MAX_LIMIT: u32 = 10;

/// A safe, user-facing classification for a failed Web API request.
///
/// The underlying HTTP response can contain account or request details, so callers receive only
/// this classification and a sanitized message.  The code is stable for RPC clients; keep it
/// aligned with the frontend's error handling when adding a new variant.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApiError {
    pub code: &'static str,
    pub message: String,
}

impl ApiError {
    fn upstream() -> Self {
        Self {
            code: "upstream_error",
            message: "Spotify request failed".to_owned(),
        }
    }

    fn from_status(status: u16, retry_after: Option<Duration>) -> Self {
        match status {
            400 => Self {
                code: "invalid_request",
                message: "Spotify rejected the request (HTTP 400)".to_owned(),
            },
            401 => Self {
                code: "authentication",
                message: "Spotify authentication failed (HTTP 401)".to_owned(),
            },
            403 => Self {
                code: "access_denied",
                message: "Spotify denied access (HTTP 403)".to_owned(),
            },
            429 => Self::rate_limited(retry_after),
            _ => Self {
                code: "upstream_error",
                message: format!("Spotify request failed (HTTP {status})"),
            },
        }
    }

    fn rate_limited(wait: Option<Duration>) -> Self {
        let message = match wait {
            Some(wait) if wait.as_secs() > 0 => {
                format!("Spotify rate limit reached; retry in {}s", wait.as_secs())
            }
            _ => "Spotify rate limit reached; retry later".to_owned(),
        };
        Self {
            code: "rate_limited",
            message,
        }
    }

    fn network() -> Self {
        Self {
            code: "network_error",
            message: "Could not reach Spotify".to_owned(),
        }
    }
}

/// Convenient wrapper around the rspotify web API functionality.
#[derive(Clone)]
pub struct WebApi {
    #[cfg(test)]
    offline_for_test: bool,
    /// Rspotify web API.
    api: AuthCodePkceSpotify,
    /// The username of the logged in user.
    user: Option<String>,
    /// Sender of the mpsc channel to the [Spotify](crate::spotify::Spotify) worker thread.
    worker_channel: Arc<RwLock<Option<mpsc::UnboundedSender<WorkerCommand>>>>,
    /// Time at which the token expires.
    token_expiration: Arc<RwLock<DateTime<Utc>>>,
    /// Held while the token is being renewed, so that concurrent API calls wait for a single
    /// renewal instead of each starting one of their own.
    token_renewal: Arc<Mutex<()>>,
    /// When the rate limit Spotify last reported is due to expire, if a call has been
    /// abandoned for one. Callers that can retry ask for the remaining wait.
    rate_limited_until: Arc<RwLock<Option<Instant>>>,
}

impl Default for WebApi {
    fn default() -> Self {
        let config = Config {
            token_refreshing: false,
            ..Default::default()
        };
        let api = AuthCodePkceSpotify::with_config(
            rspotify::Credentials::new_pkce(&crate::authentication::client_id()),
            rspotify::OAuth::default(),
            config,
        );
        Self {
            #[cfg(test)]
            offline_for_test: false,
            api,
            user: None,
            worker_channel: Arc::new(RwLock::new(None)),
            token_expiration: Arc::new(RwLock::new(Utc::now())),
            token_renewal: Arc::new(Mutex::new(())),
            rate_limited_until: Arc::new(RwLock::new(None)),
        }
    }
}

impl WebApi {
    pub fn new() -> Self {
        Self::default()
    }

    /// Explicit disconnected test worker mode; ordinary WebApi instances keep real behavior.
    #[cfg(test)]
    pub fn new_offline_for_test() -> Self {
        let mut api = Self::new();
        api.offline_for_test = true;
        api
    }

    /// Set the username for use with the API.
    pub fn set_user(&mut self, user: Option<String>) {
        self.user = user;
    }

    /// Set the sending end of the channel to the worker thread, managed by
    /// [Spotify](crate::spotify::Spotify).
    pub(crate) fn set_worker_channel(
        &mut self,
        channel: Arc<RwLock<Option<mpsc::UnboundedSender<WorkerCommand>>>>,
    ) {
        self.worker_channel = channel;
    }

    /// Whether the stored token has more than five minutes of life left in it.
    fn token_is_fresh(&self) -> bool {
        let delta = *self.token_expiration.read().unwrap() - Utc::now();
        delta.num_seconds() > 60 * 5
    }

    /// Make sure a usable token is stored before an API call goes out, renewing it if it is
    /// missing or about to expire.
    ///
    /// This runs on the calling thread rather than the async runtime: every caller has to wait
    /// for the token anyway, and the renewal lock means that a burst of parallel calls (the
    /// library bootstrap, a search fanning out over several endpoints) performs one renewal
    /// between them instead of one each.
    pub fn refresh_token_if_needed(&self) {
        if self.token_is_fresh() {
            return;
        }

        let _renewal = self.token_renewal.lock().unwrap();

        // Another thread may have renewed the token while this one waited for the lock.
        if self.token_is_fresh() {
            return;
        }

        info!("API token missing or about to expire, renewing");
        match crate::authentication::get_rspotify_token() {
            Ok(token) => {
                let expires_at = token
                    .expires_at
                    .unwrap_or_else(|| Utc::now() + ChronoDuration::hours(1));
                *self.api.token.lock().unwrap() = Some(token);
                *self.token_expiration.write().unwrap() = expires_at;
            }
            Err(e) => {
                error!("Failed to update token: {e}");
                crate::ui::osd::notify("couldn't renew the Spotify token; restart to log in again");
            }
        }
    }

    /// Force a token renewal, used when the API rejects the current one.
    fn force_token_refresh(&self) {
        *self.token_expiration.write().unwrap() = Utc::now();
        self.refresh_token_if_needed();
    }

    /// Record a rate limit that was too long to wait out, so that callers able to come
    /// back later know when to.
    fn note_rate_limit(&self, wait: Duration) {
        let until = Instant::now() + wait;
        let mut limited = self.rate_limited_until.write().unwrap();

        // One rate limit takes down every call in flight, and one notice says as much as
        // seven identical ones would.
        if !limited.is_some_and(|previous| previous > Instant::now()) {
            crate::ui::osd::notify(format!("Spotify is busy, retrying in {}s", wait.as_secs()));
        }

        // Keep the longer window, so a concurrent call reporting a shorter one cannot
        // bring the retry forward into the limit that is still running.
        if limited.is_none_or(|previous| previous < until) {
            *limited = Some(until);
        }
    }

    /// How long until the rate limit that abandoned a call expires, if one is running.
    pub fn rate_limit_wait(&self) -> Option<Duration> {
        let until = (*self.rate_limited_until.read().unwrap())?;
        until
            .checked_duration_since(Instant::now())
            .map(|remaining| remaining + RATE_LIMIT_GRACE)
    }

    /// Execute `api_call`, retrying once for a short rate limit or an expired token.
    ///
    /// This is the detailed form used by endpoints whose callers need to tell authentication,
    /// request, and rate-limit failures apart.  It deliberately keeps response bodies out of the
    /// returned error and logs only safe classifications, because an upstream body may contain
    /// account or request details.
    fn api_with_retry_result<F, R>(&self, api_call: F) -> Result<R, ApiError>
    where
        F: Fn(&AuthCodePkceSpotify) -> ClientResult<R>,
    {
        #[cfg(test)]
        if self.offline_for_test {
            return Err(ApiError::upstream());
        }

        // Inside a rate limit every request is refused, and sending them anyway only keeps the
        // limit going; callers that can come back later ask for the wait.
        if let Some(wait) = self.rate_limit_wait() {
            debug!("Spotify API request skipped during a rate-limit window");
            return Err(ApiError::rate_limited(Some(wait)));
        }

        self.refresh_token_if_needed();
        let mut rate_limit_retried = false;
        let mut auth_retried = false;

        loop {
            // Another request may have reported a longer wait while this one
            // slept or renewed its token.
            if let Some(wait) = self.rate_limit_wait() {
                return Err(ApiError::rate_limited(Some(wait)));
            }
            let result = { api_call(&self.api) };
            match result {
                Ok(value) => return Ok(value),
                Err(ClientError::Http(error)) => match *error {
                    HttpError::StatusCode(response) => {
                        let status = response.status();
                        let retry_after = response
                            .header("Retry-After")
                            .and_then(|value| value.parse::<u64>().ok())
                            .map(Duration::from_secs);
                        debug!("Spotify API request failed with HTTP status {status}");

                        if status == 429 {
                            let wait = retry_after
                                .unwrap_or(Duration::from_secs(1))
                                .max(Duration::from_secs(1));
                            self.note_rate_limit(wait);

                            // Spotify can ask for a wait of minutes, which is longer than any
                            // caller should be held for. Keep this process-wide window so cloned
                            // WebApi handles do not continue sending requests into it.
                            if wait.as_secs() > MAX_RETRY_AFTER_SECS {
                                return Err(ApiError::rate_limited(self.rate_limit_wait()));
                            }

                            // A short rate limit is safe to wait out once. If Spotify sends a
                            // second 429, return it instead of silently converting it to None.
                            if !rate_limit_retried {
                                rate_limit_retried = true;
                                thread::sleep(wait + RATE_LIMIT_GRACE);
                                continue;
                            }

                            return Err(ApiError::rate_limited(self.rate_limit_wait()));
                        }

                        // Retry a 401 once after forcing the shared token to renew. The second
                        // response is classified below rather than being discarded.
                        if status == 401 && !auth_retried {
                            auth_retried = true;
                            self.force_token_refresh();
                            continue;
                        }

                        return Err(ApiError::from_status(status, retry_after));
                    }
                    _ => {
                        error!("Spotify API request failed before receiving an HTTP status");
                        return Err(ApiError::network());
                    }
                },
                Err(ClientError::ParseJson(_)) => {
                    error!("Spotify API response could not be decoded");
                    return Err(ApiError {
                        code: "invalid_response",
                        message: "Spotify returned an incompatible response".to_owned(),
                    });
                }
                Err(ClientError::Io(_)) => return Err(ApiError::network()),
                Err(ClientError::InvalidToken) => return Err(ApiError::from_status(401, None)),
                Err(_) => {
                    error!("Spotify API request failed");
                    return Err(ApiError::upstream());
                }
            }
        }
    }

    /// Execute `api_call` with the shared retry policy, preserving the historical `Option` API.
    fn api_with_retry<F, R>(&self, api_call: F) -> Option<R>
    where
        F: Fn(&AuthCodePkceSpotify) -> ClientResult<R>,
    {
        self.api_with_retry_result(api_call).ok()
    }

    /// Append `tracks` at `position` in the playlist with `playlist_id`.
    pub fn append_tracks(
        &self,
        playlist_id: &str,
        tracks: &[Playable],
        position: Option<u32>,
    ) -> Result<PlaylistResult, ()> {
        self.api_with_retry(|api| {
            let trackids: Vec<PlayableId> = tracks
                .iter()
                .filter_map(|playable| playable.into())
                .collect();
            api.playlist_add_items(
                PlaylistId::from_id(playlist_id).unwrap(),
                trackids.iter().map(|id| id.as_ref()),
                position,
            )
        })
        .ok_or(())
    }

    pub fn delete_tracks(
        &self,
        playlist_id: &str,
        snapshot_id: &str,
        playables: &[Playable],
    ) -> Result<PlaylistResult, ()> {
        self.api_with_retry(move |api| {
            let playable_ids: Vec<PlayableId> = playables
                .iter()
                .filter_map(|playable| playable.into())
                .collect();
            let positions = playables
                .iter()
                .map(|playable| [playable.list_index() as u32])
                .collect::<Vec<_>>();
            let item_pos: Vec<ItemPositions> = playable_ids
                .iter()
                .zip(positions.iter())
                .map(|(id, positions)| ItemPositions {
                    id: id.as_ref(),
                    positions,
                })
                .collect();
            api.playlist_remove_specific_occurrences_of_items(
                PlaylistId::from_id(playlist_id).unwrap(),
                item_pos,
                Some(snapshot_id),
            )
        })
        .ok_or(())
    }

    /// Set the playlist with `id` to contain only `tracks`. If the playlist already contains
    /// tracks, they will be removed.
    pub fn overwrite_playlist(&self, id: &str, tracks: &[Playable]) {
        // create mutable copy for chunking
        let mut tracks: Vec<Playable> = tracks.to_vec();

        // we can only send 100 tracks per request
        let mut remainder = if tracks.len() > 100 {
            Some(tracks.split_off(100))
        } else {
            None
        };

        let replace_items = self.api_with_retry(|api| {
            let playable_ids: Vec<PlayableId> = tracks
                .iter()
                .filter_map(|playable| playable.into())
                .collect();
            api.playlist_replace_items(
                PlaylistId::from_id(id).unwrap(),
                playable_ids.iter().map(|p| p.as_ref()),
            )
        });

        if replace_items.is_some() {
            debug!("saved {} tracks to playlist {}", tracks.len(), id);
            while let Some(ref mut tracks) = remainder.clone() {
                // grab the next set of 100 tracks
                remainder = if tracks.len() > 100 {
                    Some(tracks.split_off(100))
                } else {
                    None
                };

                debug!("adding another {} tracks to playlist", tracks.len());
                if self.append_tracks(id, tracks, None).is_ok() {
                    debug!("{} tracks successfully added", tracks.len());
                } else {
                    error!("error saving tracks to playlists {id}");
                    return;
                }
            }
        } else {
            error!("error saving tracks to playlist {id}");
        }
    }

    /// Delete the playlist with the given `id`.
    pub fn delete_playlist(&self, id: &str) -> Result<(), ()> {
        self.api_with_retry(|api| {
            api.library_remove([LibraryId::Playlist(PlaylistId::from_id(id).unwrap())])
        })
        .ok_or(())
    }

    /// Create a playlist with the given `name`, `public` visibility and `description`. Returns the
    /// id of the newly created playlist.
    pub fn create_playlist(
        &self,
        name: &str,
        public: Option<bool>,
        description: Option<&str>,
    ) -> Result<String, ()> {
        let result = self.api_with_retry(|api| {
            api.user_playlist_create(
                UserId::from_id(self.user.as_ref().unwrap()).unwrap(),
                name,
                public,
                None,
                description,
            )
        });
        result.map(|r| r.id.id().to_string()).ok_or(())
    }

    /// Rename a playlist through the same retry and rate limit policy as other mutations.
    pub fn rename_playlist(&self, id: &str, name: &str) -> Result<(), ()> {
        let id = PlaylistId::from_id(id).map_err(|_| ())?;
        self.api_with_retry(|api| {
            api.playlist_change_detail(id.as_ref(), Some(name), None, None, None)
        })
        .map(|_| ())
        .ok_or(())
    }

    /// Fetch the album with the given `album_id`.
    pub fn album(&self, album_id: &str) -> Result<FullAlbum, ()> {
        debug!("fetching album {album_id}");
        let aid = AlbumId::from_id(album_id).map_err(|_| ())?;
        self.api_with_retry(|api| api.album(aid.clone(), Some(Market::FromToken)))
            .ok_or(())
    }

    /// Fetch the artist with the given `artist_id`.
    pub fn artist(&self, artist_id: &str) -> Result<FullArtist, ()> {
        let aid = ArtistId::from_id(artist_id).map_err(|_| ())?;
        self.api_with_retry(|api| api.artist(aid.clone())).ok_or(())
    }

    /// Fetch the playlist with the given `playlist_id`.
    pub fn playlist(&self, playlist_id: &str) -> Result<FullPlaylist, ()> {
        let pid = PlaylistId::from_id(playlist_id).map_err(|_| ())?;
        self.api_with_retry(|api| api.playlist(pid.clone(), None, Some(Market::FromToken)))
            .ok_or(())
    }

    /// Fetch the track with the given `track_id`.
    pub fn track(&self, track_id: &str) -> Result<FullTrack, ()> {
        let tid = TrackId::from_id(track_id).map_err(|_| ())?;
        self.api_with_retry(|api| api.track(tid.clone(), Some(Market::FromToken)))
            .ok_or(())
    }

    /// Fetch the show with the given `show_id`.
    pub fn show(&self, show_id: &str) -> Result<FullShow, ()> {
        let sid = ShowId::from_id(show_id).map_err(|_| ())?;
        self.api_with_retry(|api| api.get_a_show(sid.clone(), Some(Market::FromToken)))
            .ok_or(())
    }

    /// Fetch the episode with the given `episode_id`.
    pub fn episode(&self, episode_id: &str) -> Result<FullEpisode, ()> {
        let eid = EpisodeId::from_id(episode_id).map_err(|_| ())?;
        self.api_with_retry(|api| api.get_an_episode(eid.clone(), Some(Market::FromToken)))
            .ok_or(())
    }

    /// Search for items of `searchtype` using the provided `query`. Limit the results to `limit`
    /// items with the given `offset` from the start.
    pub fn search(
        &self,
        searchtype: SearchType,
        query: &str,
        limit: u32,
        offset: u32,
    ) -> Result<SearchResult, ()> {
        self.search_detailed(searchtype, query, limit, offset)
            .map_err(|_| ())
    }

    /// Search with a sanitized error classification for RPC callers.
    pub fn search_detailed(
        &self,
        searchtype: SearchType,
        query: &str,
        limit: u32,
        offset: u32,
    ) -> Result<SearchResult, ApiError> {
        self.api_with_retry_result(|api| {
            api.search(
                query,
                searchtype,
                Some(Market::FromToken),
                None,
                Some(limit.min(SEARCH_MAX_LIMIT)),
                Some(offset),
            )
        })
    }

    /// Fetch all the current user's playlists.
    pub fn current_user_playlist(&self) -> ApiResult<Playlist> {
        const MAX_LIMIT: u32 = 50;
        let spotify = self.clone();
        let fetch_page = move |offset: u32| {
            debug!("fetching user playlists, offset: {offset}");
            spotify.api_with_retry(|api| {
                match api.current_user_playlists_manual(Some(MAX_LIMIT), Some(offset)) {
                    Ok(page) => Ok(ApiPage {
                        offset: page.offset,
                        total: page.total,
                        items: page.items.iter().map(|sp| sp.into()).collect(),
                    }),
                    Err(e) => Err(e),
                }
            })
        };
        ApiResult::new(MAX_LIMIT, Arc::new(fetch_page))
    }

    /// A raw-position playlist page; unavailable entries keep their original positions.
    pub fn playlist_tracks_page(
        &self,
        playlist_id: &str,
        offset: u32,
        limit: u32,
    ) -> Result<ApiPage<Playable>, ()> {
        let id = PlaylistId::from_id(playlist_id).map_err(|_| ())?;
        self.api_with_retry(|api| {
            api.playlist_items_manual(
                id.as_ref(),
                None,
                Some(Market::FromToken),
                Some(limit),
                Some(offset),
            )
        })
        .map(|page| ApiPage {
            offset: page.offset,
            total: page.total,
            items: page
                .items
                .iter()
                .enumerate()
                .filter_map(|(index, item)| {
                    let playable = item.item.as_ref().filter(|item| !item.is_unknown())?;
                    let mut playable: Playable = playable.into();
                    playable.set_added_at(item.added_at);
                    playable.set_list_index(page.offset as usize + index);
                    Some(playable)
                })
                .collect(),
        })
        .ok_or(())
    }

    /// Get the tracks in the playlist given by `playlist_id`.
    pub fn user_playlist_tracks(&self, playlist_id: &str) -> ApiResult<Playable> {
        const MAX_LIMIT: u32 = 100;
        let spotify = self.clone();
        let playlist_id = playlist_id.to_string();
        let fetch_page = move |offset: u32| {
            debug!("fetching playlist {playlist_id} tracks, offset: {offset}");
            spotify.api_with_retry(|api| {
                match api.playlist_items_manual(
                    PlaylistId::from_id(&playlist_id).unwrap(),
                    None,
                    Some(Market::FromToken),
                    Some(MAX_LIMIT),
                    Some(offset),
                ) {
                    Ok(page) => Ok(ApiPage {
                        offset: page.offset,
                        total: page.total,
                        items: page
                            .items
                            .iter()
                            .enumerate()
                            .filter(|(_, pt)| {
                                if let Some(t) = pt.item.as_ref()
                                    && !t.is_unknown()
                                {
                                    true
                                } else {
                                    error!("Could not process item {pt:?}, ignoring");
                                    false
                                }
                            })
                            .flat_map(|(index, pt)| {
                                pt.item.as_ref().map(|t| {
                                    let mut playable: Playable = t.into();
                                    // TODO: set these
                                    playable.set_added_at(pt.added_at);
                                    playable.set_list_index(page.offset as usize + index);
                                    playable
                                })
                            })
                            .collect(),
                    }),
                    Err(e) => Err(e),
                }
            })
        };
        ApiResult::new(MAX_LIMIT, Arc::new(fetch_page))
    }

    /// Fetch all the tracks in the album with the given `album_id`. Limit the results to `limit`
    /// items, with `offset` from the beginning.
    pub fn album_tracks(
        &self,
        album_id: &str,
        limit: u32,
        offset: u32,
    ) -> Result<Page<SimplifiedTrack>, ()> {
        debug!("fetching album tracks {album_id}");
        self.api_with_retry(|api| {
            api.album_track_manual(
                AlbumId::from_id(album_id).unwrap(),
                Some(Market::FromToken),
                Some(limit),
                Some(offset),
            )
        })
        .ok_or(())
    }

    /// Fetch all the albums of the given `artist_id`. `album_type` determines which type of albums
    /// to fetch.
    pub fn artist_albums(
        &self,
        artist_id: &str,
        album_type: Option<AlbumType>,
    ) -> ApiResult<Album> {
        const MAX_SIZE: u32 = 50;
        let spotify = self.clone();
        let artist_id = artist_id.to_string();
        let fetch_page = move |offset: u32| {
            debug!("fetching artist {artist_id} albums, offset: {offset}");
            spotify.api_with_retry(|api| {
                match api.artist_albums_manual(
                    ArtistId::from_id(&artist_id).unwrap(),
                    album_type.as_ref().copied(),
                    Some(Market::FromToken),
                    Some(MAX_SIZE),
                    Some(offset),
                ) {
                    Ok(page) => {
                        let mut albums: Vec<Album> =
                            page.items.iter().map(|sa| sa.into()).collect();
                        albums.sort_by(|a, b| b.year.cmp(&a.year));
                        Ok(ApiPage {
                            offset: page.offset,
                            total: page.total,
                            items: albums,
                        })
                    }
                    Err(e) => Err(e),
                }
            })
        };

        ApiResult::new(MAX_SIZE, Arc::new(fetch_page))
    }

    /// Get all the episodes of the show with the given `show_id`.
    pub fn show_episodes(&self, show_id: &str) -> ApiResult<Episode> {
        const MAX_SIZE: u32 = 50;
        let spotify = self.clone();
        let show_id = show_id.to_string();
        let fetch_page = move |offset: u32| {
            debug!("fetching show {} episodes, offset: {}", show_id, offset);
            spotify.api_with_retry(|api| {
                match api.get_shows_episodes_manual(
                    ShowId::from_id(&show_id).unwrap(),
                    Some(Market::FromToken),
                    Some(50),
                    Some(offset),
                ) {
                    Ok(page) => Ok(ApiPage {
                        offset: page.offset,
                        total: page.total,
                        items: page.items.iter().map(|se| se.into()).collect(),
                    }),
                    Err(e) => Err(e),
                }
            })
        };

        ApiResult::new(MAX_SIZE, Arc::new(fetch_page))
    }

    /// Get the user's saved shows.
    pub fn get_saved_shows(&self, offset: u32) -> Result<Page<Show>, ()> {
        self.api_with_retry(|api| api.get_saved_show_manual(Some(50), Some(offset)))
            .ok_or(())
    }

    /// Add the shows with the given `ids` to the user's library.
    pub fn save_shows(&self, ids: &[&str]) -> Result<(), ()> {
        self.api_with_retry(|api| {
            api.library_add(
                ids.iter()
                    .map(|id| LibraryId::Show(ShowId::from_id(*id).unwrap()))
                    .collect::<Vec<LibraryId>>(),
            )
        })
        .ok_or(())
    }

    /// Remove the shows with `ids` from the user's library.
    pub fn unsave_shows(&self, ids: &[&str]) -> Result<(), ()> {
        self.api_with_retry(|api| {
            api.library_remove(
                ids.iter()
                    .map(|id| LibraryId::Show(ShowId::from_id(*id).unwrap()))
                    .collect::<Vec<LibraryId>>(),
            )
        })
        .ok_or(())
    }

    /// Get the user's followed artists. `last` is an artist id. If it is specified, the artists
    /// after the one with this id will be retrieved.
    pub fn current_user_followed_artists(
        &self,
        last: Option<&str>,
    ) -> Result<CursorBasedPage<FullArtist>, ()> {
        self.api_with_retry(|api| api.current_user_followed_artists(last, Some(50)))
            .ok_or(())
    }

    /// Add the logged in user to the followers of the artists with the given `ids`.
    pub fn user_follow_artists(&self, ids: Vec<&str>) -> Result<(), ()> {
        self.api_with_retry(|api| {
            api.library_add(
                ids.iter()
                    .map(|id| LibraryId::Artist(ArtistId::from_id(*id).unwrap()))
                    .collect::<Vec<LibraryId>>(),
            )
        })
        .ok_or(())
    }

    /// Remove the logged in user to the followers of the artists with the given `ids`.
    pub fn user_unfollow_artists(&self, ids: Vec<&str>) -> Result<(), ()> {
        self.api_with_retry(|api| {
            api.library_remove(
                ids.iter()
                    .map(|id| LibraryId::Artist(ArtistId::from_id(*id).unwrap()))
                    .collect::<Vec<LibraryId>>(),
            )
        })
        .ok_or(())
    }

    /// Get the user's saved albums, starting at the given `offset`. The result is paginated.
    pub fn current_user_saved_albums(&self, offset: u32) -> Result<Page<SavedAlbum>, ()> {
        self.api_with_retry(|api| {
            api.current_user_saved_albums_manual(Some(Market::FromToken), Some(50), Some(offset))
        })
        .ok_or(())
    }

    /// Add the albums with the given `ids` to the user's saved albums.
    pub fn current_user_saved_albums_add(&self, ids: Vec<&str>) -> Result<(), ()> {
        self.api_with_retry(|api| {
            api.library_add(
                ids.iter()
                    .map(|id| LibraryId::Album(AlbumId::from_id(*id).unwrap()))
                    .collect::<Vec<LibraryId>>(),
            )
        })
        .ok_or(())
    }

    /// Remove the albums with the given `ids` from the user's saved albums.
    pub fn current_user_saved_albums_delete(&self, ids: Vec<&str>) -> Result<(), ()> {
        self.api_with_retry(|api| {
            api.library_remove(
                ids.iter()
                    .map(|id| LibraryId::Album(AlbumId::from_id(*id).unwrap()))
                    .collect::<Vec<LibraryId>>(),
            )
        })
        .ok_or(())
    }

    /// Get the user's saved tracks, starting at the given `offset`. The result is paginated.
    pub fn current_user_saved_tracks(&self, offset: u32) -> Result<Page<SavedTrack>, ()> {
        self.api_with_retry(|api| {
            api.current_user_saved_tracks_manual(Some(Market::FromToken), Some(50), Some(offset))
        })
        .ok_or(())
    }

    /// Add the tracks with the given `ids` to the user's saved tracks.
    pub fn current_user_saved_tracks_add(&self, ids: Vec<&str>) -> Result<(), ()> {
        self.api_with_retry(|api| {
            api.library_add(
                ids.iter()
                    .map(|id| LibraryId::Track(TrackId::from_id(*id).unwrap()))
                    .collect::<Vec<LibraryId>>(),
            )
        })
        .ok_or(())
    }

    /// Remove the tracks with the given `ids` from the user's saved tracks.
    pub fn current_user_saved_tracks_delete(&self, ids: Vec<&str>) -> Result<(), ()> {
        self.api_with_retry(|api| {
            api.library_remove(
                ids.iter()
                    .map(|id| LibraryId::Track(TrackId::from_id(*id).unwrap()))
                    .collect::<Vec<LibraryId>>(),
            )
        })
        .ok_or(())
    }

    /// Add the logged in user to the followers of the playlist with the given `id`.
    pub fn user_playlist_follow_playlist(&self, id: &str) -> Result<(), ()> {
        self.api_with_retry(|api| {
            api.library_add([LibraryId::Playlist(PlaylistId::from_id(id).unwrap())])
        })
        .ok_or(())
    }

    /// Get the top tracks of the artist with the given `id`.
    pub fn artist_top_tracks(&self, id: &str) -> Result<Vec<Track>, ()> {
        #[allow(deprecated)]
        self.api_with_retry(|api| {
            api.artist_top_tracks(ArtistId::from_id(id).unwrap(), Some(Market::FromToken))
        })
        .map(|ft| ft.iter().map(|t| t.into()).collect())
        .ok_or(())
    }

    /// Get artists related to the artist with the given `id`.
    pub fn artist_related_artists(&self, id: &str) -> Result<Vec<Artist>, ()> {
        #[allow(deprecated)]
        self.api_with_retry(|api| api.artist_related_artists(ArtistId::from_id(id).unwrap()))
            .map(|fa| fa.iter().map(|a| a.into()).collect())
            .ok_or(())
    }

    /// Get the available categories.
    pub fn categories(&self) -> ApiResult<Category> {
        const MAX_LIMIT: u32 = 50;
        let spotify = self.clone();
        let fetch_page = move |offset: u32| {
            debug!("fetching categories, offset: {offset}");
            spotify.api_with_retry(|api| {
                #[allow(deprecated)]
                match api.categories_manual(
                    None,
                    Some(Market::FromToken),
                    Some(MAX_LIMIT),
                    Some(offset),
                ) {
                    Ok(page) => Ok(ApiPage {
                        offset: page.offset,
                        total: page.total,
                        items: page.items.iter().map(|cat| cat.into()).collect(),
                    }),
                    Err(e) => Err(e),
                }
            })
        };
        ApiResult::new(MAX_LIMIT, Arc::new(fetch_page))
    }

    /// Get the playlists in the category given by `category_id`.
    pub fn category_playlists(&self, category_id: &str) -> ApiResult<Playlist> {
        const MAX_LIMIT: u32 = 50;
        let spotify = self.clone();
        let category_id = category_id.to_string();
        let fetch_page = move |offset: u32| {
            debug!("fetching category playlists, offset: {offset}");
            spotify.api_with_retry(|api| {
                #[allow(deprecated)]
                match api.category_playlists_manual(
                    &category_id,
                    Some(Market::FromToken),
                    Some(MAX_LIMIT),
                    Some(offset),
                ) {
                    Ok(page) => Ok(ApiPage {
                        offset: page.offset,
                        total: page.total,
                        items: page.items.iter().map(|sp| sp.into()).collect(),
                    }),
                    Err(e) => Err(e),
                }
            })
        };
        ApiResult::new(MAX_LIMIT, Arc::new(fetch_page))
    }

    /// Get details about the logged in user.
    pub fn current_user(&self) -> Result<PrivateUser, ()> {
        self.api_with_retry(|api| api.current_user()).ok_or(())
    }

    /// The Spotify Connect devices the account can play on right now.
    pub fn devices(&self) -> Option<Vec<Device>> {
        self.api_with_retry(|api| api.device())
    }

    /// Play `playable` from `position_ms` on the Connect device `device_id`.
    pub fn play_on(&self, device_id: &str, playable: &Playable, position_ms: u32) -> bool {
        let uri = playable.uri();
        let id = match playable {
            Playable::Track(_) => {
                TrackId::from_uri(&uri).map(|id| PlayableId::Track(id.into_static()))
            }
            Playable::Episode(_) => {
                EpisodeId::from_uri(&uri).map(|id| PlayableId::Episode(id.into_static()))
            }
        };
        let Ok(id) = id else {
            error!("cannot cast {uri}: not a playable id");
            return false;
        };
        let position = ChronoDuration::milliseconds(i64::from(position_ms));
        self.api_with_retry(|api| {
            api.start_uris_playback([id.clone()], Some(device_id), None, Some(position))
        })
        .is_some()
    }

    pub fn resume_on(&self, device_id: &str) -> bool {
        self.api_with_retry(|api| api.resume_playback(Some(device_id), None))
            .is_some()
    }

    pub fn pause_on(&self, device_id: &str) -> bool {
        self.api_with_retry(|api| api.pause_playback(Some(device_id)))
            .is_some()
    }

    pub fn seek_on(&self, device_id: &str, position_ms: u32) -> bool {
        let position = ChronoDuration::milliseconds(i64::from(position_ms));
        self.api_with_retry(|api| api.seek_track(position, Some(device_id)))
            .is_some()
    }

    pub fn volume_on(&self, device_id: &str, percent: u8) -> bool {
        self.api_with_retry(|api| api.volume(percent.min(100), Some(device_id)))
            .is_some()
    }

    /// What the account is playing and where, or `None` when nothing is active or
    /// the request failed.
    pub fn playback(&self) -> Option<CurrentPlaybackContext> {
        self.api_with_retry(|api| {
            api.current_playback(
                None,
                Some([&AdditionalType::Track, &AdditionalType::Episode]),
            )
        })
        .flatten()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_failures_have_safe_classifications() {
        assert_eq!(ApiError::from_status(400, None).code, "invalid_request");
        assert_eq!(ApiError::from_status(401, None).code, "authentication");
        assert_eq!(ApiError::from_status(403, None).code, "access_denied");

        let rate_limited = ApiError::from_status(429, Some(Duration::from_secs(12)));
        assert_eq!(rate_limited.code, "rate_limited");
        assert!(rate_limited.message.contains("12s"));
        assert_eq!(ApiError::from_status(503, None).code, "upstream_error");
    }

    #[test]
    fn invalid_requests_and_responses_are_not_reported_as_rate_limits() {
        let api = WebApi::new();
        *api.token_expiration.write().unwrap() = Utc::now() + ChronoDuration::hours(1);
        let error = api
            .api_with_retry_result(|_| -> ClientResult<()> {
                Err(ClientError::Http(Box::new(HttpError::StatusCode(
                    "HTTP/1.1 400 Bad Request\r\n\r\nprivate response detail"
                        .parse()
                        .unwrap(),
                ))))
            })
            .unwrap_err();
        assert_eq!(error.code, "invalid_request");
        assert!(error.message.contains("HTTP 400"));
        assert!(!error.message.contains("private"));
        assert!(api.rate_limit_wait().is_none());
        let error = api
            .api_with_retry_result(|_| -> ClientResult<()> {
                Err(ClientError::ParseJson(
                    serde_json::from_str::<serde_json::Value>("private invalid payload")
                        .unwrap_err(),
                ))
            })
            .unwrap_err();
        assert_eq!(error.code, "invalid_response");
        assert!(!error.message.contains("private"));
        assert!(api.rate_limit_wait().is_none());
    }

    #[test]
    fn a_second_short_rate_limit_records_cooldown_for_every_handle() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let api = WebApi::new();
        *api.token_expiration.write().unwrap() = Utc::now() + ChronoDuration::hours(1);
        let calls = AtomicUsize::new(0);
        let error = api
            .api_with_retry_result(|_| -> ClientResult<()> {
                calls.fetch_add(1, Ordering::SeqCst);
                Err(ClientError::Http(Box::new(HttpError::StatusCode(
                    "HTTP/1.1 429 Too Many Requests\r\nRetry-After: 0\r\n\r\n"
                        .parse()
                        .unwrap(),
                ))))
            })
            .unwrap_err();
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert_eq!(error.code, "rate_limited");
        assert!(api.rate_limit_wait().is_some());
        let blocked = api
            .clone()
            .api_with_retry_result(|_| -> ClientResult<()> {
                panic!("shared cooldown must block requests")
            })
            .unwrap_err();
        assert_eq!(blocked.code, "rate_limited");
    }

    #[test]
    fn detailed_search_keeps_offline_failures_generic() {
        let api = WebApi::new_offline_for_test();
        let detailed = api
            .search_detailed(SearchType::Track, "offline", 20, 0)
            .expect_err("the offline fixture must not call Spotify");
        assert_eq!(detailed.code, "upstream_error");
        assert_eq!(api.search(SearchType::Track, "offline", 20, 0), Err(()));
    }

    #[test]
    fn cloned_api_handles_report_the_shared_rate_limit() {
        let api = WebApi::new();
        let clone = api.clone();
        api.note_rate_limit(Duration::from_secs(30));

        let error = clone
            .api_with_retry_result(|_| -> ClientResult<()> {
                panic!("a shared cooldown must prevent the request")
            })
            .expect_err("the clone must see the original handle's cooldown");
        assert_eq!(error.code, "rate_limited");
        assert!(error.message.contains("retry in"));
    }

    #[test]
    fn a_rate_limit_is_reported_until_it_expires() {
        let api = WebApi::new();
        assert!(api.rate_limit_wait().is_none());

        api.note_rate_limit(Duration::from_secs(17));
        let wait = api
            .rate_limit_wait()
            .expect("the limit should still be running");
        assert!(
            wait > Duration::from_secs(17),
            "the grace period is included"
        );

        *api.rate_limited_until.write().unwrap() = Some(Instant::now() - Duration::from_secs(1));
        assert!(api.rate_limit_wait().is_none(), "an expired limit is over");
    }

    #[test]
    fn the_longer_of_two_rate_limits_wins() {
        let api = WebApi::new();

        api.note_rate_limit(Duration::from_secs(30));
        api.note_rate_limit(Duration::from_secs(5));

        let wait = api
            .rate_limit_wait()
            .expect("the limit should still be running");
        assert!(
            wait > Duration::from_secs(29),
            "a shorter concurrent limit must not bring the retry forward, got {wait:?}"
        );
    }
}
