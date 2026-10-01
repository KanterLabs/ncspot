# Resonance OpenTUI workspace

This is KanterLabs Resonance's full terminal workspace, the default frontend on Unix. Resonance
is built on ncspot by Henrik Friedrichsen and contributors, with the original license and
attribution retained. The historical `prototype/opentui` directory now contains the complete
workspace: Now Playing, Queue, Library, Search, Browse, Playlists, Podcasts, Radio, Settings,
Help, and Cast.

Rust owns authentication, librespot playback, queue/radio state, library caches, and persisted
config/data. This frontend uses `@opentui/core` and the versioned local Unix socket API; it does
not access Spotify credentials or call Spotify directly. Windows currently uses the legacy Rust
Cursive interface.

## Build and run

Use the repository's pinned Bun 1.4.2:

```sh
bun install --frozen-lockfile
bun run typecheck
bun test
bun run build
./dist/resonance-opentui --demo
```

`dist/resonance-opentui` is standalone. Install it beside the Rust `resonance` executable for the
default launcher; Bun is needed for development/building only. Fedora archives include both
executables, the legacy `ncspot` alias, and dependency notices. Restart Resonance after updates.

```sh
resonance                          # engine plus workspace
resonance --headless               # Rust backend only
resonance --legacy-ui              # retained ncspot interface
resonance-opentui --socket PATH    # attach to an existing backend
resonance-opentui --demo --route radio --theme dark --reduced-motion
resonance-opentui --smoke          # parser/API fixture check
```

A launched workspace closes its managed engine when it exits. A manually attached frontend
closes independently; it never automatically reconnects after losing the original instance.
`RESONANCE_OPENTUI_BIN` selects an absolute frontend executable path for the Rust launcher.

## Controls and rendering

Use 1–9 for Now Playing, Queue, Library, Search, Playlists, Podcasts, Radio, Settings, and Cast;
B for Browse and ? for Help. Space toggles playback, Shift+R starts radio, L changes appearance,
: opens commands, and q/F5/Ctrl+C quits. Esc cancels prompts or returns to Now Playing. Focused
inputs capture text; individual screens show their action keys. Lists support keyboard navigation,
mouse selection and double-click activation.

Live sessions save light/dark and reduced-motion preferences separately from Rust settings. Demo
previews do not save them. Now Playing groups artwork, metadata and controls in a centered player
card, with an Up Next card on wide terminals. Real cover images use Rust's existing disk cache;
only a missing cover needs a CDN download, without a Spotify Web API call. Initials remain the
fallback when an image is unavailable. Sampled audio drives the spectrum; without samples the
view shows playback progress. Reduced motion disables interpolation while status still updates.

For visual review, `bun run test/visual-capture.ts --theme both` captures the actual native
renderer cells at 189×34 and 80×24 as JSON, SVG and text under `/tmp/resonance-visual`.

## API and debug behavior

[workspace/contracts.ts](src/workspace/contracts.ts) defines route names and the RPC vocabulary.
Requests are newline JSON tagged `resonance`, version 1, with IDs for response correlation.
Responses identify the engine instance. Queue edits use exact revisions and per-occurrence entry
IDs; stale mutations fail instead of acting on a changed selection. Playlist removal validates
position/identity. Refresh after a stale selection or timed-out mutation before retrying.

`--debug FILE` logs method, outcome/error code, and elapsed timing without parameters. The Rust
launcher passes a `.ui.log` companion when backend `--debug` is enabled. Local radio selection
uses cached metadata/history and never calls Spotify recommendation endpoints. Rust's existing
configuration, caches, credentials and data directories remain authoritative.

See the [workspace guide](../../doc/opentui-prototype.md) for installation, all routes, lifecycle,
preferences, and packaging. Retain ncspot's BSD-2-Clause attribution and OpenTUI/bundled dependency
license notices when distributing builds.
