# Resonance OpenTUI workspace

The OpenTUI workspace is Resonance's default interface on Unix. The directory and this document
retain their historical prototype names. KanterLabs maintains Resonance as a fork of
[ncspot](https://github.com/hrkfdn/ncspot), by Henrik Friedrichsen and contributors; its Rust
playback, configuration, library, and queue code and BSD-2-Clause attribution remain central to
the application.

## Start and attach

```sh
resonance                 # Rust engine plus OpenTUI, in this terminal
resonance --headless      # Rust engine and local socket only
resonance --legacy-ui     # retained ncspot Cursive interface
```

Windows currently uses the legacy interface. On Unix, Rust finishes authentication before the
frontend takes over the terminal. Rust owns the Spotify session, playback, library, queue, radio,
listening history, and saved state. Existing config/cache/data paths are preserved; the frontend
does not read Spotify credentials or make Spotify requests itself.

The launcher finds `resonance-opentui` using the absolute `RESONANCE_OPENTUI_BIN` override,
then beside the Rust executable, then on `PATH`. Install the Rust and OpenTUI executables together.
If OpenTUI is unavailable, use `--legacy-ui` while installing it.

To attach to an existing headless engine:

```sh
resonance-opentui --socket "$XDG_RUNTIME_DIR/resonance/resonance.sock"
```

The socket path varies with the runtime directory and concurrent instances; `resonance info`
reports runtime paths. Closing the default launched workspace stops its engine and saves state.
Closing a manually attached frontend leaves the independent engine running. The client does not
automatically reconnect to another process after disconnection; reopen it to attach deliberately.
In the legacy interface, F5 / `:prototype` still opens a separate companion terminal.

## Routes and controls

| Key | Screen | Actions |
| --- | --- | --- |
| 1 | Now Playing | Transport, seek, volume, repeat, shuffle, save, share, quick search, appearance and motion |
| 2 | Queue | Play/remove an entry, reorder, append/play next, clear, save as playlist |
| 3 | Library | Saved tracks, albums and artists; details and save/unsave |
| 4 | Search | Query tracks, albums, artists, playlists, shows and episodes |
| B | Browse | Categories and their playlists |
| 5 | Playlists | Create, rename, delete, inspect tracks, add/remove tracks |
| 6 | Podcasts | Saved shows, episodes, playback and queue actions |
| 7 | Radio | Station start/stop, Discovery adjustment, local diagnostics |
| 8 | Settings | Configuration inspection, reload, reconnect, logout, motion preference |
| ? | Help | Effective workspace/configured bindings and command entry |
| 9 | Cast | Discover Spotify Connect/Roku targets, connect, disconnect |

**Space** toggles playback; **Shift+R** starts radio from the current track; **L** changes
appearance; **:** opens command entry. **q**, **F5**, or **Ctrl+C** closes the workspace. **Esc**
cancels a prompt or returns to Now Playing. Normal shortcuts yield to focused text inputs.
Configured keybindings are loaded from Rust settings; unsupported view commands report errors.

Lists support arrows, Page Up/Down, Home/End, mouse selection and double-click activation.
Screen hints describe the available action keys. For example, Queue uses Enter to play,
Delete/Backspace to remove, Shift+Up/Down to reorder, C to clear, and S to save as a playlist.
Prompts use Enter to submit and Esc to cancel; confirmation prompts use Y to confirm and N to
cancel. The shared player footer follows playback across screens.

In Now Playing, **/** opens quick search without leaving the player. Type a song or artist;
cached track results appear first while a background refresh can update them. Use **↑/↓**
to select a song, **Enter** to play now, **Ctrl+N** to play next, or **Ctrl+E** to add to the
queue. **Esc** closes the popup. Clickable buttons provide the same three actions. Play next
and add to queue keep the current song playing and count as explicit user choices in radio.
Live search uses ten results per request to match Spotify's current Search API limit.
The Search screen's **[ / ]** keys move through consecutive ten-result pages.
Cached results remain usable when a refresh fails; **Ctrl+R** retries in quick search.
Search errors distinguish rejected requests, authentication, access, network, incompatible
responses, and actual rate limits. Rate-limit errors include the remaining wait when available.

## Appearance, audio, and covers

Light appearance uses pearl surfaces, blue highlights, and layered borders inspired by Liquid
Glass. Terminal colors approximate those surfaces. Press L or click the header control for dark
appearance. Live sessions save the theme in `opentui-theme.json` and reduced motion in
`opentui-workspace.json` under the frontend's Resonance config directory, normally
`$XDG_CONFIG_HOME/resonance` or `~/.config/resonance`. Rust settings remain in their existing path.
Use the Now Playing Motion control or Settings to change reduced motion.

Now Playing renders **Audio spectrum** from sampled bands supplied by Rust when available,
including a silence label when appropriate. Without audio samples, it shows playback progress.
Reduced motion disables animation interpolation while keeping live status updates. A centered
player card displays real album artwork, with a separate Up Next card on wide terminals.
Artwork loads from the existing Rust cover cache. Terminals supporting Kitty graphics or Sixel
display a sharp image (up to 640 pixels), with rounded corners, a subtle reflection, and a
theme tint that retains fine detail. Theme changes reuse the same cached image. Other terminals,
including Ptyxis without Sixel support, use the portable two RGB pixels per terminal cell.
Cache misses download the cover from Spotify's image CDN without a Web API call.
Unavailable covers fall back to initials, and compact terminals retain the playback controls.

To try native artwork in WezTerm without changing its saved configuration, close Resonance
and reopen it with:

```sh
wezterm --config enable_kitty_graphics=true start --always-new-process -- resonance
```

```sh
resonance-opentui --demo --theme dark --route queue --reduced-motion
```

`--demo` is an offline interactive fixture and does not save preferences. `--route` accepts every
route name in the workspace contract: `now-playing`, `queue`, `library`, `search`, `browse`,
`playlists`, `podcasts`, `radio`, `settings`, `help`, and `cast`.

## Local API and debugging

The Unix socket carries versioned newline JSON requests with protocol `resonance`, version `1`,
a correlation ID, method, and parameters. Typed frontend contracts cover playback, queue,
library/search/browse, playlists, radio, settings, casting, and sharing. Responses include the
engine instance identity; unsolicited status updates include playback and runtime notices.

Queue mutations use a revision and per-occurrence entry identity, so duplicate tracks remain
distinct and outdated selections are rejected. Playlist removals also check the selected position
and identity against current data. Refresh after a stale-data error or an uncertain request timeout
before retrying a mutation.

Frontend debug logs record RPC method, result/error code, and timing without request parameters:

```sh
resonance-opentui --socket /path/to/resonance.sock --debug /tmp/resonance-ui.log
resonance --debug /tmp/resonance.log
```

The Rust launcher passes its frontend a companion log path ending in `.ui.log`. Rust's existing
backend debug logging is separate from the redacted frontend request timings.

Radio ranks cached metadata and local listening history. It does not call Spotify recommendation
endpoints. Catalog enrichment may fetch other Spotify metadata; station selection stays local.
Use the Radio diagnostics screen or `resonance radio-debug` for explainable ranking and exclusions.

## Build, package, and update

From the repository root:

```sh
cargo build --release
cd prototype/opentui
bun install --frozen-lockfile
bun run typecheck
bun test
bun run build
cd ../..
install -m 0755 prototype/opentui/dist/resonance-opentui target/release/resonance-opentui
./target/release/resonance
```

The Fedora workflow pins Bun 1.4.2 and packages `resonance`, the legacy `ncspot` executable alias,
and standalone `resonance-opentui` together with dependency licenses/notices. Runtime installation
needs those executables, not Bun or Node:

```sh
mkdir -p "$HOME/.local/bin"
tar -xzf resonance-<version>-fedora<release>-x86_64.tar.gz
install -m 0755 resonance ncspot resonance-opentui "$HOME/.local/bin/"
```

Keep the archive's license notices with the installation. If the repository updater is installed,
`ncspot-update` installs the three executables together. Restart the running Resonance process
after updating; replacing executable files does not update an existing session.
Before installation, the updater verifies a rollback snapshot of existing binaries, configuration,
queue state, and cached metadata in `~/.local/share/resonance/rollback`. Re-downloadable cover and
streaming audio files are excluded. Restore data only when deliberately needed; keeping the old
binaries lets you return to the retained interface without resetting your library.

Resonance retains ncspot's [BSD-2-Clause license](../LICENSE). OpenTUI is MIT-licensed; bundled
native dependencies carry their own notices. See the
[OpenTUI repository](https://github.com/anomalyco/opentui) for upstream attribution.
