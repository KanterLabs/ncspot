<div align="center" style="text-align:center">
  <img alt="Resonance logo" height="128" src="images/resonance.svg">
  <h1>Resonance</h1>
  <h3>A KanterLabs native terminal Spotify client built on ncspot</h3>

  <img alt="Resonance search tab" src="images/screenshot.png">
</div>

Resonance is a KanterLabs-maintained terminal client for Spotify. It is built in Rust with
[librespot](https://github.com/librespot-org/librespot), and keeps the small, keyboard-focused
interface that made the upstream project useful on servers, laptops, and the BSDs.

This fork carries substantial code and design from [ncspot](https://github.com/hrkfdn/ncspot),
the original project by Henrik Friedrichsen and contributors. Please see the upstream project for
its history and the retained [BSD-2-Clause license](LICENSE). KanterLabs' changes are maintained in
the [KanterLabs/ncspot repository](https://github.com/KanterLabs/ncspot).

Resonance requires a Spotify Premium account for playback features that Spotify does not expose to
free accounts.

## Features

- Browse tracks, albums, playlists, genres, podcasts, and search results
- Vim keybindings and a low resource footprint
- IPC socket and MPRIS controls for desktop integrations
- Spotify Connect and Roku casting
- A local radio station ranked from cached library metadata
- Deterministic radio diagnostics and offline replay snapshots with `radio-debug`

## Installation

KanterLabs has not published Resonance packages yet. Build it from this repository with a current
[Rust toolchain](https://www.rust-lang.org/tools/install):

```sh
cargo build --release
./target/release/resonance
```

The build also provides a compatibility executable at `target/release/ncspot` for existing update
scripts. Both commands run Resonance and use the same configuration and authentication identity;
`resonance` is the canonical command for new integrations.

The upstream [user guide](/doc/users.md) contains ncspot's historical package and distribution
instructions. Those channels are not release channels for Resonance; use the source build above
until a Resonance package is published.

## Spotify app setup

Resonance uses a KanterLabs Spotify app identity for Web API requests by default. You can use a
client ID from your own Spotify developer dashboard instead. The client ID is public application metadata; Resonance uses
the browser authorization flow with PKCE and does not require a client secret.

Add the callback URI below to the app's Redirect URIs, exactly as written:

```
http://127.0.0.1:8989/login
```

Set a client ID in the configuration file:

```toml
spotify_client_id = "0123456789abcdef0123456789abcdef"
spotify_redirect_uri = "http://127.0.0.1:8989/login"
```

Or provide an override for one run or shell session:

```sh
RESONANCE_SPOTIFY_CLIENT_ID=0123456789abcdef0123456789abcdef resonance
```

The redirect URI defaults to `http://127.0.0.1:8989/login`, so the configuration entry is only
needed when you want to make it explicit. Use `resonance info` to inspect platform paths and
`-b PATH` to select a custom base path.

Resonance keeps an existing ncspot configuration directory when it finds one, so upgrading does
not reset a user's settings or library state. Its credentials are stored in separate client-ID-
specific cache names, so changing the client ID cannot silently reuse or replace an upstream
account's cached credentials.

Audio playback uses librespot's separate streaming authentication identity. The custom developer
app ID applies to the Web API; it does not replace that streaming identity. Existing legacy
playback credentials can be reused without modifying their original files. After an affected
upgrade, a fresh playback login may be needed.

## Configuration

The default configuration file is `config.toml` in Resonance's platform configuration directory.
The file accepts the playback, appearance, keybinding, casting, and Spotify app settings used by
the client. See the [configuration reference](/doc/users.md#configuration) for the shared ncspot
settings retained by this fork.

## Experimental OpenTUI view

Press **F5** (or `:prototype`) to open the [OpenTUI Now Playing prototype](doc/opentui-prototype.md)
in a separate desktop terminal. It follows the same playback session and offers transport,
radio, Discovery, and Up Next controls. **F5**, **Esc**, or **q** in that window closes only the
prototype. **L** toggles the Liquid Glass inspired light theme and dark theme; the choice is
remembered. Fedora packages include its standalone `resonance-opentui` executable.

## Local radio and diagnostics

The Now Playing Discovery dial goes from familiar favorites (0) to locally
unplayed songs and unfamiliar artists (100), with a balanced default of 50.
Click its arrows or use `:discovery 75`, then press **Shift+R** to start radio.
After clicking the dial, left/right arrows adjust it; the mouse wheel also works
over the dial. Changing it leaves playback and the existing queue alone. A brief animated
transition shows the new level; `visualizer_fps = 0` disables animation.

Ranking uses cached metadata and local listening history, with no Spotify
recommendation calls. “Unplayed” refers to this device’s history. When history
is sparse, saved songs provide a familiarity hint and diagnostics explain any
shortfall in the requested mix.

Click **Radio** in Now Playing or press **Shift+R** to build a station from metadata already cached
by Resonance, keeping the current song and playback position. It can warm a station in
the background while the current track plays and records explainable scores and exclusions for
debugging. Radio keeps topping up the queue and never automatically repeats a
song played earlier in this terminal session. Explicitly queued duplicates remain
playable. Stop playback to end the station. If no unheard candidates remain, the
station stays active and waits for more cached metadata rather than replaying
songs. The command-line report is local and deterministic:

```sh
resonance radio-debug --seed spotify:track:TRACK_ID --rng-seed 42 --limit 20 --discovery 75
resonance radio-debug --replay ~/.cache/resonance/radio-replay.json
```

`--limit` accepts values from 1 through 100. A replay reads an immutable snapshot and does not log
in or make a network request.

## License

Resonance is distributed under the [BSD-2-Clause license](LICENSE), with the original ncspot
copyright and attribution retained.
