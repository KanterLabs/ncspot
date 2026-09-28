//! Finding Rokus on the local network and opening Spotify on them.
//!
//! Rokus answer SSDP searches for `roku:ecp` and take commands over their External
//! Control Protocol, plain HTTP on port 8060. Nothing here sends audio to the TV:
//! it only wakes the Roku's own Spotify app, which then shows up as an ordinary
//! Spotify Connect device for [`super`] to play on.

use std::collections::BTreeMap;
use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use std::time::{Duration, Instant};

use log::{debug, warn};

/// The Spotify app's id in the Roku channel store.
const SPOTIFY_APP_ID: &str = "22297";
const SSDP_ADDRESS: (Ipv4Addr, u16) = (Ipv4Addr::new(239, 255, 255, 250), 1900);
const ECP_PORT: u16 = 8060;
/// How long a single HTTP request to a Roku may take. They are on the LAN and
/// answer in milliseconds; one that does not is not there.
const HTTP_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Roku {
    /// What the owner named it, such as "Living Room TV".
    pub name: String,
    /// `http://<address>:8060`
    pub base: String,
    /// Whether the Spotify app is installed. Without it there is nothing to cast to.
    pub spotify: SpotifyApp,
}

/// What the Roku said about its Spotify app.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SpotifyApp {
    /// Installed, under this app id.
    Installed(String),
    /// The Roku listed its apps, and Spotify is not among them.
    Missing,
    /// The Roku would not list its apps. Rokus set to limit what the network may
    /// ask of them refuse this while still answering everything else, so it says
    /// nothing about whether Spotify is there; opening it is worth a try.
    Unknown,
}

/// Find the Rokus on the network: those that answer an SSDP search within
/// `timeout`, plus any listed by address in `hosts`, for networks that drop
/// multicast.
pub fn discover(timeout: Duration, hosts: &[String]) -> Vec<Roku> {
    let mut bases: BTreeMap<String, ()> = hosts.iter().map(|host| (base_for(host), ())).collect();
    match search(timeout) {
        Ok(found) => bases.extend(found.into_iter().map(|base| (base, ()))),
        Err(e) => warn!("roku search failed: {e}"),
    }

    let client = client();
    bases
        .into_keys()
        .filter_map(|base| {
            let info = get(&client, &format!("{base}/query/device-info"))?;
            let name = device_name(&info)?;
            let spotify = match get(&client, &format!("{base}/query/apps")) {
                Some(apps) => {
                    find_spotify(&apps).map_or(SpotifyApp::Missing, SpotifyApp::Installed)
                }
                None => SpotifyApp::Unknown,
            };
            Some(Roku {
                name,
                base,
                spotify,
            })
        })
        .collect()
}

/// Open the Spotify app on `roku`, bringing it to the front if it is already open.
pub fn launch_spotify(roku: &Roku) -> Result<(), String> {
    let id = match &roku.spotify {
        SpotifyApp::Installed(id) => id.as_str(),
        _ => SPOTIFY_APP_ID,
    };
    let url = format!("{}/launch/{id}", roku.base);
    let response = client()
        .post(&url)
        .send()
        .map_err(|e| format!("Could not reach {}: {e}", roku.name))?;
    match response.status().as_u16() {
        200..=299 => Ok(()),
        // Refused, rather than failed: the Roku is limiting what the network may do.
        401 | 403 => Err(format!(
            "{} refused to open Spotify. On the Roku, set Settings > System > Advanced \
             system settings > Control by mobile apps > Network access to Permissive.",
            roku.name
        )),
        404 => Err(format!(
            "{} does not have the Spotify app installed.",
            roku.name
        )),
        status => Err(format!(
            "{} could not open Spotify (HTTP {status}).",
            roku.name
        )),
    }
}

/// Send an SSDP search and collect the ECP base URL of every Roku that answers.
fn search(timeout: Duration) -> std::io::Result<Vec<String>> {
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0))?;
    socket.set_multicast_ttl_v4(2)?;
    let request = "M-SEARCH * HTTP/1.1\r\n\
                   Host: 239.255.255.250:1900\r\n\
                   Man: \"ssdp:discover\"\r\n\
                   ST: roku:ecp\r\n\
                   MX: 1\r\n\r\n";
    let target = SocketAddr::from(SSDP_ADDRESS);
    // UDP can drop a datagram, so ask twice.
    socket.send_to(request.as_bytes(), target)?;
    socket.send_to(request.as_bytes(), target)?;

    let deadline = Instant::now() + timeout;
    let mut found = Vec::new();
    let mut buffer = [0u8; 2048];
    while let Some(left) = deadline.checked_duration_since(Instant::now()) {
        socket.set_read_timeout(Some(left.max(Duration::from_millis(1))))?;
        match socket.recv_from(&mut buffer) {
            Ok((length, from)) => {
                let reply = String::from_utf8_lossy(&buffer[..length]);
                let base = location(&reply).unwrap_or_else(|| base_for(&from.ip().to_string()));
                debug!("roku answered from {from}: {base}");
                if !found.contains(&base) {
                    found.push(base);
                }
            }
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                break;
            }
            Err(e) => return Err(e),
        }
    }
    Ok(found)
}

fn client() -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        .timeout(HTTP_TIMEOUT)
        .build()
        .unwrap_or_default()
}

fn get(client: &reqwest::blocking::Client, url: &str) -> Option<String> {
    client
        .get(url)
        .send()
        .and_then(|response| response.error_for_status())
        .and_then(|response| response.text())
        .map_err(|e| debug!("roku request {url} failed: {e}"))
        .ok()
}

/// `http://host:8060` for a bare host or address, leaving a full URL alone.
fn base_for(host: &str) -> String {
    let host = host.trim().trim_end_matches('/');
    if host.starts_with("http://") {
        host.to_string()
    } else if host.contains(':') {
        format!("http://{host}")
    } else {
        format!("http://{host}:{ECP_PORT}")
    }
}

/// The `LOCATION` header of an SSDP reply, without its trailing slash.
fn location(reply: &str) -> Option<String> {
    reply.lines().find_map(|line| {
        let (key, value) = line.split_once(':')?;
        key.trim()
            .eq_ignore_ascii_case("location")
            .then(|| value.trim().trim_end_matches('/').to_string())
    })
}

/// The text of the first `<tag>` in `xml`.
fn element<'a>(xml: &'a str, tag: &str) -> Option<&'a str> {
    let open = format!("<{tag}>");
    let start = xml.find(&open)? + open.len();
    let end = start + xml[start..].find(&format!("</{tag}>"))?;
    let text = xml[start..end].trim();
    (!text.is_empty()).then_some(text)
}

/// The name to show for a Roku: the one its owner gave it, or failing that the
/// one it came with.
fn device_name(info: &str) -> Option<String> {
    [
        "user-device-name",
        "friendly-device-name",
        "friendly-model-name",
        "model-name",
    ]
    .into_iter()
    .find_map(|tag| element(info, tag))
    .map(unescape)
}

/// The id of the Spotify app in a Roku's app list: the store's usual id, or any
/// app named Spotify, since the id is not the same in every region.
fn find_spotify(apps: &str) -> Option<String> {
    let mut named = None;
    for entry in apps.split("<app ").skip(1) {
        let Some((attributes, rest)) = entry.split_once('>') else {
            continue;
        };
        let Some(id) = attribute(attributes, "id") else {
            continue;
        };
        if id == SPOTIFY_APP_ID {
            return Some(id.to_string());
        }
        let title = rest.split("</app>").next().unwrap_or_default();
        if named.is_none() && title.to_lowercase().contains("spotify") {
            named = Some(id.to_string());
        }
    }
    named
}

/// The value of `name="..."` among an element's attributes.
fn attribute<'a>(attributes: &'a str, name: &str) -> Option<&'a str> {
    let key = format!("{name}=\"");
    let start = attributes.find(&key)? + key.len();
    let end = start + attributes[start..].find('"')?;
    Some(&attributes[start..end])
}

fn unescape(text: &str) -> String {
    text.replace("&apos;", "'")
        .replace("&quot;", "\"")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use super::{
        SpotifyApp, base_for, device_name, discover, find_spotify, launch_spotify, location,
    };

    /// A stand in for a Roku's ECP server: answers the two queries and records
    /// every request line it is sent.
    fn fake_roku(refuse_apps: bool) -> (String, Arc<Mutex<Vec<String>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap().to_string();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let mut stream = stream.unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut request = String::new();
                reader.read_line(&mut request).unwrap();
                // Drain the headers.
                let mut line = String::new();
                while reader.read_line(&mut line).unwrap() > 2 {
                    line.clear();
                }
                let body = if request.contains("/query/device-info") {
                    "<device-info><user-device-name>Living Room TV</user-device-name></device-info>"
                } else if request.contains("/query/apps") {
                    r#"<apps><app id="22297" type="appl" version="7.3">Spotify Music</app></apps>"#
                } else {
                    ""
                };
                log.lock().unwrap().push(request.trim().to_string());
                // A Roku limiting network access refuses the app list with a 403.
                let refused = refuse_apps && request.contains("/query/apps");
                let (status, body) = if refused {
                    ("403 Forbidden", "")
                } else {
                    ("200 OK", body)
                };
                write!(
                    stream,
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .unwrap();
            }
        });
        (address, seen)
    }

    #[test]
    fn a_configured_roku_is_named_and_spotify_opened_on_it() {
        let (address, seen) = fake_roku(false);
        let found = discover(Duration::from_millis(50), std::slice::from_ref(&address));
        // Anything real on the network that answers the search is listed too, so
        // pick the fake out by its address.
        let roku = found
            .iter()
            .find(|roku| roku.base == format!("http://{address}"))
            .expect("the configured roku is found");
        assert_eq!(roku.name, "Living Room TV");
        assert_eq!(roku.spotify, SpotifyApp::Installed("22297".into()));

        launch_spotify(roku).expect("the launch is accepted");
        let seen = seen.lock().unwrap();
        assert!(
            seen.iter()
                .any(|line| line.starts_with("POST /launch/22297 ")),
            "{seen:?}"
        );
    }

    #[test]
    fn the_ecp_address_comes_from_the_ssdp_reply() {
        let reply = "HTTP/1.1 200 OK\r\nCache-Control: max-age=3600\r\nST: roku:ecp\r\n\
                     LOCATION: http://10.0.0.41:8060/\r\nUSN: uuid:roku:ecp:X00000000001\r\n\r\n";
        assert_eq!(location(reply).as_deref(), Some("http://10.0.0.41:8060"));
    }

    #[test]
    fn configured_hosts_can_be_bare_addresses() {
        assert_eq!(base_for("10.0.0.41"), "http://10.0.0.41:8060");
        assert_eq!(base_for("10.0.0.41:8061"), "http://10.0.0.41:8061");
        assert_eq!(base_for("http://tv.lan:8060/"), "http://tv.lan:8060");
    }

    #[test]
    fn the_owners_name_for_the_roku_wins() {
        let info = "<device-info><model-name>Roku Ultra</model-name>\
                    <friendly-device-name>Roku Ultra - X001</friendly-device-name>\
                    <user-device-name>Shane&apos;s TV</user-device-name></device-info>";
        assert_eq!(device_name(info).as_deref(), Some("Shane's TV"));

        let unnamed = "<device-info><user-device-name></user-device-name>\
                       <friendly-device-name>Roku Ultra - X001</friendly-device-name></device-info>";
        assert_eq!(device_name(unnamed).as_deref(), Some("Roku Ultra - X001"));
    }

    #[test]
    fn a_roku_that_will_not_list_its_apps_is_still_offered() {
        let (address, _) = fake_roku(true);
        let found = discover(Duration::from_millis(50), std::slice::from_ref(&address));
        let roku = found
            .iter()
            .find(|roku| roku.base == format!("http://{address}"))
            .expect("the configured roku is found");
        assert_eq!(roku.spotify, SpotifyApp::Unknown);
    }

    #[test]
    fn spotify_is_found_in_the_app_list() {
        let apps = r#"<apps><app id="12" type="appl" version="5.1">Netflix</app>
                      <app id="22297" type="appl" version="7.3">Spotify Music</app></apps>"#;
        assert_eq!(find_spotify(apps).as_deref(), Some("22297"));

        // Listed under another id, it is still found by its name.
        let elsewhere = r#"<apps><app id="12" type="appl">Netflix</app>
                           <app id="551012" type="appl" version="1.0">Spotify</app></apps>"#;
        assert_eq!(find_spotify(elsewhere).as_deref(), Some("551012"));

        let without = r#"<apps><app id="12" type="appl">Netflix</app></apps>"#;
        assert_eq!(find_spotify(without), None);
    }
}
