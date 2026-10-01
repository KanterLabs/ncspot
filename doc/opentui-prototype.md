# OpenTUI prototype

The OpenTUI frontend is an experimental Linux desktop companion for Resonance. It opens in a
second terminal while the existing Rust process keeps ownership of the Spotify session, audio,
queue, radio, and discovery state. The frontend subscribes to the running process through its
local Unix socket and sends playback commands over the same connection. It does not start another
Spotify session or ask for additional Spotify credentials.

## Open the prototype

From the main Resonance interface, press <kbd>F5</kbd> or enter `:prototype`. Resonance starts
`resonance-opentui` in a separate terminal and passes it the current IPC socket.

The executable is selected in this order:

1. The absolute path in `RESONANCE_OPENTUI_BIN`, when set.
2. `resonance-opentui` beside the running `resonance` executable. This is the layout used by the
   Fedora release tarball.
3. `resonance-opentui` found on `PATH`.

The environment override is useful for a locally built prototype:

```sh
RESONANCE_OPENTUI_BIN="$PWD/prototype/opentui/dist/resonance-opentui" resonance
```

The prototype is Linux desktop scoped for this first iteration. If no compatible terminal launcher
or local socket is available, Resonance reports the launch error and continues running.

## Prototype controls

The view follows the current track, playback position, volume, queue, radio status, and discovery
level published by the Rust engine. Its playback, radio, and discovery actions are sent back to that
engine, so the main Resonance view and the prototype remain one session.

Press `q`, <kbd>Esc</kbd>, or <kbd>F5</kbd> in the prototype to close the prototype window. These
keys close only the OpenTUI process; the main Resonance process and playback continue.

## Run it manually

The packaged executable is standalone. Running it does not require Bun, Node, npm, or a second
Spotify login. Give it an existing Resonance Unix socket explicitly when testing outside the
launcher:

```sh
resonance-opentui --socket "$XDG_RUNTIME_DIR/resonance/resonance.sock"
```

Use the built-in fixture when iterating on layout without a running Resonance instance:

```sh
resonance-opentui --demo
```

The actual socket path depends on the active runtime directory. `resonance info` shows the paths
used by the current installation.

## Install a Fedora archive

Keep the three executables from the archive together in one directory. The sibling executable is
what lets `resonance` find the prototype without Bun or a separate JavaScript installation:

```sh
mkdir -p "$HOME/.local/bin"
tar -xzf resonance-<version>-fedora<release>-x86_64.tar.gz
install -m 0755 resonance ncspot resonance-opentui "$HOME/.local/bin/"
```

Use `resonance` as the main executable. `ncspot` remains in the archive as the compatibility name;
installing only `ncspot` leaves the sibling lookup incomplete.

If the repository updater is installed on the machine, it downloads the latest successful Fedora
artifact, verifies its checksum, and installs all three executables together:

```sh
source "$HOME/.ncspot-update.sh"  # once per shell, if it is not already sourced
ncspot-update
```

Restart the running Resonance process after an update before pressing <kbd>F5</kbd>.

## Build from source

The checked-in package and Bun lockfile are the source of truth for the prototype. From the
repository root:

```sh
cd prototype/opentui
bun install --frozen-lockfile
bun run build
./dist/resonance-opentui --demo
```

CI builds with a pinned official Bun release and the Fedora workflow places the resulting
`dist/resonance-opentui` beside `resonance` and `ncspot` in the downloadable tarball. The archive
also carries the project license and the license/notice files installed with the OpenTUI
dependencies under `licenses/`.

OpenTUI is MIT-licensed. Its native terminal core includes notices for bundled third-party code;
keep those notices when changing the dependency set. See the [OpenTUI repository](https://github.com/anomalyco/opentui)
for the upstream source and license details.
