use std::fs;
use std::path::Path;
use std::sync::OnceLock;

use librespot_core::authentication::Credentials as RespotCredentials;
use librespot_core::cache::Cache;
use librespot_oauth::OAuthClientBuilder;
use log::{error, info, warn};
use url::Url;

use crate::config;

const CLIENT_ID_ENV: &str = "RESONANCE_SPOTIFY_CLIENT_ID";
const DEFAULT_REDIRECT_URI: &str = "http://127.0.0.1:8989/login";
pub const DEFAULT_CLIENT_ID: &str = "b7f3b8a9271c4848bd9d36d7b0b3d997";

#[cfg(test)]
const TEST_CLIENT_ID: &str = "00000000000000000000000000000000";

#[derive(Clone, Debug, PartialEq, Eq)]
struct AuthConfig {
    client_id: String,
    redirect_uri: String,
}

static AUTH_CONFIG: OnceLock<AuthConfig> = OnceLock::new();

/// OAuth scopes used by ncspot's streaming, library, playlist, and playback features.
///
/// Keep this list to scopes supported by Spotify's public authorization flow. In particular,
/// avoid the old aggregate scopes (`playlist-read`, `playlist-modify`, and `user-modify`) which
/// are not valid Spotify scope names.
static OAUTH_SCOPES: &[&str] = &[
    "streaming",
    "user-read-private",
    "user-library-read",
    "user-library-modify",
    "user-read-playback-state",
    "user-modify-playback-state",
    "user-read-currently-playing",
    "playlist-read-private",
    "playlist-read-collaborative",
    "playlist-modify-public",
    "playlist-modify-private",
    "user-follow-read",
    "user-follow-modify",
];

fn validate_client_id(value: &str) -> Result<String, String> {
    let value = value.trim();
    if value.len() != 32 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(format!(
            "{} Spotify app client ID must be exactly 32 hexadecimal characters",
            ncspot::BIN_NAME
        ));
    }
    Ok(value.to_string())
}

fn validate_redirect_uri(value: &str) -> Result<String, String> {
    let parsed = Url::parse(value).map_err(|error| {
        format!(
            "{} Spotify redirect URI is invalid: {error}",
            ncspot::BIN_NAME
        )
    })?;
    let valid = parsed.scheme() == "http"
        && parsed.host_str() == Some("127.0.0.1")
        && parsed.path() == "/login"
        && parsed.query().is_none()
        && parsed.fragment().is_none()
        && parsed.username().is_empty()
        && parsed.password().is_none();
    if !valid {
        return Err(format!(
            "{} Spotify redirect URI must use http://127.0.0.1[:port]/login",
            ncspot::BIN_NAME
        ));
    }
    Ok(value.to_string())
}

/// Resolve and validate authentication settings without changing the process configuration.
///
/// The explicit client ID wins over the environment, compile-time option, and built-in default.
fn resolve_config(
    client_id: Option<&str>,
    redirect_uri: Option<&str>,
) -> Result<AuthConfig, String> {
    let client_id = match client_id {
        Some(value) => validate_client_id(value)?,
        None => {
            let value = std::env::var(CLIENT_ID_ENV)
                .ok()
                .or_else(|| option_env!("SPOTIFY_CLIENT_ID").map(str::to_owned))
                .unwrap_or_else(|| DEFAULT_CLIENT_ID.to_string());
            validate_client_id(&value)?
        }
    };
    let redirect_uri = redirect_uri
        .map(str::to_owned)
        .unwrap_or_else(|| DEFAULT_REDIRECT_URI.to_string());
    let redirect_uri = validate_redirect_uri(&redirect_uri)?;

    Ok(AuthConfig {
        client_id,
        redirect_uri,
    })
}

/// Configure the Spotify app identity before opening credentials or an API session.
pub fn configure(client_id: Option<&str>, redirect_uri: Option<&str>) -> Result<(), String> {
    let resolved = resolve_config(client_id, redirect_uri)?;
    match AUTH_CONFIG.get() {
        Some(existing) if existing == &resolved => Ok(()),
        Some(_) => Err(format!(
            "{} Spotify authentication is already configured with a different app identity",
            ncspot::BIN_NAME
        )),
        None => AUTH_CONFIG.set(resolved).map_err(|_| {
            format!(
                "{} Spotify authentication configuration raced",
                ncspot::BIN_NAME
            )
        }),
    }
}

/// Return the configured Spotify app client ID.
///
/// Test-only callers that construct a disconnected API wrapper without application startup use a
/// harmless dummy identity. Production startup calls [`configure`] before any network operation.
pub fn client_id() -> String {
    if let Some(config) = AUTH_CONFIG.get() {
        return config.client_id.clone();
    }

    #[cfg(test)]
    {
        TEST_CLIENT_ID.to_string()
    }

    #[cfg(not(test))]
    DEFAULT_CLIENT_ID.to_string()
}

/// Return the configured Spotify OAuth callback URI.
pub fn redirect_uri() -> String {
    AUTH_CONFIG
        .get()
        .map(|config| config.redirect_uri.clone())
        .unwrap_or_else(|| DEFAULT_REDIRECT_URI.to_string())
}

fn configured_client_id() -> Result<String, String> {
    if let Some(config) = AUTH_CONFIG.get() {
        return Ok(config.client_id.clone());
    }

    #[cfg(test)]
    {
        Ok(TEST_CLIENT_ID.to_string())
    }

    #[cfg(not(test))]
    Ok(DEFAULT_CLIENT_ID.to_string())
}

/// Path to the current app identity's cached Web API token.
pub fn api_token_path() -> std::path::PathBuf {
    config::cache_path(&format!("rspotify_token-{}.json", client_id()))
}

/// Path to the current app identity's librespot cache.
pub fn playback_cache_path() -> std::path::PathBuf {
    config::cache_path(&format!("librespot-{}", client_id()))
}

/// Get credentials for use with librespot. This returns cached credentials if there are any, and
/// only falls back to the OAuth2 login process when there are none.
///
/// The credentials are deliberately not verified here. Doing so used to cost a whole extra
/// session handshake on every startup, and the session that playback needs is opened moments
/// later anyway: that one reports a rejection, and the caller offers a fresh login then.
pub fn get_credentials() -> Result<RespotCredentials, String> {
    let _ = configured_client_id()?;
    let cache = Cache::new(Some(playback_cache_path()), None, None, None)
        .expect("Could not create librespot cache");

    match cache.credentials() {
        Some(credentials) => {
            info!("Using cached credentials");
            Ok(credentials)
        }
        None => {
            info!("Attempting to login via OAuth2");
            credentials_prompt(None)
        }
    }
}

pub fn credentials_prompt(error_message: Option<String>) -> Result<RespotCredentials, String> {
    if let Some(message) = error_message {
        eprintln!("Connection error: {message}");
    }

    create_credentials()
}

pub fn create_credentials() -> Result<RespotCredentials, String> {
    println!("To login you need to perform OAuth2 authorization using your web browser\n");

    let client_id = configured_client_id()?;
    let client_builder =
        OAuthClientBuilder::new(&client_id, &redirect_uri(), OAUTH_SCOPES.to_vec());
    let oauth_client = client_builder.build().map_err(|e| e.to_string())?;

    oauth_client
        .get_access_token()
        .map(|token| RespotCredentials::with_access_token(token.access_token))
        .map_err(|e| e.to_string())
}

/// Read the cached Web API token, if one has been stored and can still be parsed.
fn cached_rspotify_token() -> Option<rspotify::Token> {
    let token_json = fs::read_to_string(api_token_path()).ok()?;
    serde_json::from_str::<rspotify::Token>(&token_json).ok()
}

/// Perform the Web API authorization if, and only if, it needs the user's browser.
///
/// This has to happen before the TUI takes over the terminal, since the prompt is written to
/// stdout. A token that is merely expired needs no prompt: it is renewed over the network by the
/// first API call that wants it, off the main thread, so startup doesn't wait for it here.
pub fn ensure_rspotify_token() -> Result<(), String> {
    let _ = configured_client_id()?;
    let usable = cached_rspotify_token().is_some_and(|token| {
        !token.is_expired()
            || token
                .refresh_token
                .as_deref()
                .is_some_and(|t| !t.is_empty())
    });

    if usable {
        return Ok(());
    }

    let token = create_rspotify_token()?;
    write_token(&api_token_path(), &token);
    Ok(())
}

/// Get a usable Web API token, renewing the cached one over the network when it has expired.
///
/// This never falls back to a fresh authorization, which would write a prompt to a stdout the TUI
/// owns and then block on a browser. [`ensure_rspotify_token`] covers that case before the TUI
/// exists.
pub fn get_rspotify_token() -> Result<rspotify::Token, String> {
    let client_id = configured_client_id()?;
    let path = api_token_path();

    if let Some(t) = cached_rspotify_token() {
        if !t.is_expired() {
            return Ok(t);
        }

        // Token is expired, try to refresh if we have a refresh token. Spotify's refresh
        // responses may omit the refresh token, in which case it must be reused, so an
        // empty/missing value here means we don't actually have a usable refresh token.
        let refresh_token = t.refresh_token.as_deref().filter(|s| !s.is_empty());
        if let Some(refresh_token) = refresh_token {
            info!("Access token expired, attempting to refresh..");
            let client_builder =
                OAuthClientBuilder::new(&client_id, &redirect_uri(), OAUTH_SCOPES.to_vec());
            if let Ok(oauth_client) = client_builder.build() {
                match oauth_client.refresh_token(refresh_token) {
                    Ok(new_token) => {
                        let mapped = map_token(new_token, Some(refresh_token));
                        write_token(&path, &mapped);
                        return Ok(mapped);
                    }
                    Err(e) => {
                        error!("Failed to refresh token: {e}");
                    }
                }
            }
        }
    }

    Err(format!(
        "no usable API token; restart {} to authorize again",
        ncspot::BIN_NAME
    ))
}

pub fn create_rspotify_token() -> Result<rspotify::Token, String> {
    println!(
        "To fully enable Web API features, you need to perform a second OAuth2 authorization\n"
    );

    let client_id = configured_client_id()?;
    let client_builder =
        OAuthClientBuilder::new(&client_id, &redirect_uri(), OAUTH_SCOPES.to_vec());
    let oauth_client = client_builder.build().map_err(|e| e.to_string())?;

    oauth_client
        .get_access_token()
        .map(|token| map_token(token, None))
        .map_err(|e| e.to_string())
}

/// Write `token` to `path`, logging (rather than silently discarding) any failure, and
/// restricting file permissions since the file holds a long-lived credential.
fn write_token(path: &Path, token: &rspotify::Token) {
    let json = match serde_json::to_string_pretty(token) {
        Ok(json) => json,
        Err(e) => {
            error!("Failed to serialize rspotify token: {e}");
            return;
        }
    };

    if let Err(e) = fs::write(path, json) {
        error!("Failed to write rspotify token cache: {e}");
        return;
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Err(e) = fs::set_permissions(path, fs::Permissions::from_mode(0o600)) {
            warn!("Failed to restrict permissions on rspotify token cache: {e}");
        }
    }
}

/// Map an OAuth token obtained from librespot into the `rspotify::Token` type used for Web
/// API calls.
///
/// Spotify's refresh-token responses may omit `refresh_token` entirely, in which case the
/// previously issued refresh token must be reused (it has not been rotated or invalidated).
/// `librespot_oauth` maps that omission to an empty string, so `fallback_refresh_token` is
/// substituted whenever the token returned by librespot is empty, to avoid ever persisting an
/// unusable refresh token.
fn map_token(
    token: librespot_oauth::OAuthToken,
    fallback_refresh_token: Option<&str>,
) -> rspotify::Token {
    let duration = if token.expires_at > std::time::Instant::now() {
        token.expires_at.duration_since(std::time::Instant::now())
    } else {
        std::time::Duration::from_secs(0)
    };
    let expires_in = chrono::Duration::from_std(duration).unwrap_or(chrono::Duration::seconds(0));

    let refresh_token = if token.refresh_token.is_empty() {
        fallback_refresh_token.map(str::to_string)
    } else {
        Some(token.refresh_token)
    };

    rspotify::Token {
        access_token: token.access_token,
        expires_in,
        scopes: std::collections::HashSet::new(),
        expires_at: Some(chrono::Utc::now() + expires_in),
        refresh_token,
    }
}

#[cfg(test)]
mod test {
    use super::*;

    fn oauth_token(refresh_token: &str) -> librespot_oauth::OAuthToken {
        librespot_oauth::OAuthToken {
            access_token: "access".to_string(),
            refresh_token: refresh_token.to_string(),
            expires_at: std::time::Instant::now() + std::time::Duration::from_secs(3600),
            token_type: "Bearer".to_string(),
            scopes: Vec::new(),
        }
    }

    #[test]
    fn map_token_keeps_new_refresh_token_when_present() {
        let mapped = map_token(oauth_token("new-refresh-token"), Some("old-refresh-token"));
        assert_eq!(mapped.refresh_token, Some("new-refresh-token".to_string()));
    }

    #[test]
    fn map_token_falls_back_to_previous_refresh_token_when_omitted() {
        let mapped = map_token(oauth_token(""), Some("old-refresh-token"));
        assert_eq!(mapped.refresh_token, Some("old-refresh-token".to_string()));
    }

    #[test]
    fn map_token_yields_no_refresh_token_when_omitted_without_fallback() {
        let mapped = map_token(oauth_token(""), None);
        assert_eq!(mapped.refresh_token, None);
    }
    #[test]
    fn explicit_app_identity_and_loopback_callback_are_validated() {
        let resolved = resolve_config(Some(DEFAULT_CLIENT_ID), None).unwrap();
        assert_eq!(resolved.client_id, "b7f3b8a9271c4848bd9d36d7b0b3d997");
        assert_eq!(resolved.redirect_uri, "http://127.0.0.1:8989/login");
        assert!(validate_client_id("secret-or-invalid-id").is_err());
        assert!(validate_redirect_uri("http://localhost:8989/login").is_err());
        assert!(validate_redirect_uri("http://127.0.0.1:8989/login?token=value").is_err());
        assert!(validate_redirect_uri("http://127.0.0.1:8989/other").is_err());
        assert!(validate_redirect_uri("http://127.0.0.1:8990/login").is_ok());
    }
}
