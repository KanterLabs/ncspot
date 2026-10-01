<div align="center" style="text-align:center">
  <img alt="Resonance logo" height="128" src="images/resonance.svg">
  <h1>Resonance</h1>
  <h3>A KanterLabs native terminal Spotify client built on ncspot</h3>

  <img alt="Resonance search tab" src="images/screenshot.png">
</div>

Resonance is a KanterLabs-maintained terminal client for Spotify. Its Rust backend uses
[librespot](https://github.com/librespot-org/librespot) for playback, with an OpenTUI workspace
as the default interface on Unix. The retained ncspot Cursive interface is available with
`--legacy-ui`; Windows currently uses that interface.

This fork carries substantial code and design from [ncspot](https://github.com/hrkfdn/ncspot),
the original project by Henrik Friedrichsen and contributors. Please see the upstream project for
its history and the retained [BSD-2-Clause license](LICENSE). KanterLabs' changes are maintained in
the [KanterLabs/ncspot repository](https://github.com/KanterLabs/ncspot).

Resonance requires a Spotify Premium account for playback features that Spotify does not expose to
free accounts.

## Features

- Browse tracks, albums, playlists, genres, podcasts, and search results
- Keyboard and mouse navigation across a complete OpenTUI workspace
- Light and dark appearance, reduced motion, and sampled audio visualization
- IPC socket and MPRIS controls for desktop integrations
- Spotify Connect and Roku casting
- A local radio station ranked from cached library metadata
- Deterministic radio diagnostics and offline replay snapshots with `radio-debug`

## Installation

Build the Rust backend and the standalone OpenTUI executable from this repository with a
current [Rust toolchain](https://www.rust-lang.org/tools/install) and Bun 1.4.2:

```sh
cargo build --release
cd prototype/opentui
bun install --frozen-lockfile
bun run build
cd ../..
install -m 0755 prototype/opentui/dist/resonance-opentui target/release/resonance-opentui
./target/release/resonance
```

Keep `resonance` and `resonance-opentui` together when installing. The packaged frontend is
standalone and does not require Bun at runtime. The Rust build also provides `ncspot` as a legacy
executable alias; it runs Resonance with the same configuration and authentication identity.
Fedora release archives contain all three executables and dependency notices. Restart Resonance
after updating so the backend and frontend use the same version.

Use `resonance --headless` to run only the Rust engine, or `resonance --legacy-ui` for the retained
Cursive interface. Windows currently uses the legacy interface. The upstream
[user guide](/doc/users.md) retains ncspot's historical distribution instructions; those package
channels are separate from KanterLabs' Resonance builds.

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

## OpenTUI workspace

`resonance` starts the Rust playback engine and OpenTUI in the same terminal on Unix. Closing this
launched interface stops the engine and saves queue and listening state. An interface attached
manually to `resonance --headless` closes independently of that engine. Authentication, audio,
library caches, queue, radio, and persisted user data remain owned by Rust. Existing ncspot
configuration and cache paths continue to be used when present.

The workspace includes Now Playing, Queue, Library, Search, Browse, Playlists, Podcasts, Radio,
Settings, Help, and Cast. Use **1–9**, **B**, and **?** to switch screens, **Space** to play/pause,
**Shift+R** to start radio, **L** to change appearance, **:** for commands, and **q** or **Ctrl+C**
to quit. Focused text inputs capture normal typing. **Esc** cancels a prompt or returns to Now
Playing. Each screen shows its own action keys.

Light/dark preferences and reduced motion are saved separately from Rust configuration. Now
Playing displays sampled audio bands when Rust supplies them; otherwise its graphic is labeled
**Ambient · playback progress**. Cover art currently uses initials. See the
[workspace guide](doc/opentui-prototype.md) for routes, controls, attachment, and debug logging.

## Local radio and diagnostics

The Radio screen's Discovery level goes from familiar favorites (0) to locally unplayed songs
and unfamiliar artists (100), with a balanced default of 50. Use left/right or **[ / ]** to adjust
it, or enter `:discovery 75`. Changing the level leaves playback and existing queue entries alone.
Press **Shift+R** to start radio from the current song.

Ranking uses cached metadata and local listening history, with no Spotify
recommendation calls. “Unplayed” refers to this device’s history. When history
is sparse, saved songs provide a familiarity hint and diagnostics explain any
shortfall in the requested mix.

Use the Radio screen or press **Shift+R** to build a station from metadata already cached
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
