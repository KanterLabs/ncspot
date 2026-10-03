# Resonance development

Resonance is the maintained KanterLabs product. The canonical source repository is hosted in
[KanterLabs' Gitea](https://gitea.home.shanekanterman.dev); the public
[KanterLabs/resonance GitHub mirror](https://github.com/KanterLabs/resonance)
is suitable for browsing, cloning, and issue links. The retained Cursive interface is available
with `resonance --legacy-ui`; the default Unix launcher also builds and starts the standalone
OpenTUI frontend.

## Prerequisites
- A working [Rust installation](https://www.rust-lang.org/tools/install)
- Python 3 (needed for building `rust-xcb` dependency)

On Linux, you also need:

- `pkgconf` (sometimes called `pkg-config`)
- Development headers for the aforementioned runtime dependencies:
  - Debian and derivatives:
    ```sh
    sudo apt install libdbus-1-dev libncursesw5-dev libpulse-dev libssl-dev libxcb1-dev libxcb-render0-dev libxcb-shape0-dev libxcb-xfixes0-dev
    ```
  - Fedora:
    ```sh
    sudo dnf install dbus-devel libxcb-devel ncurses-devel openssl-devel pulseaudio-libs-devel
    ```
  - Arch and derivatives:
    ```sh
    # headers are included in the base packages
    sudo pacman -S dbus libpulse libxcb ncurses openssl
    ```

## Debugging
For simple debugging, you can pass a debug log filename:

```sh
cargo run -- -d debug.log
```

It can be difficult to debug a TUI application as it might not run well in an IDE terminal or the
terminal could be used by the text editor. It is however possible to run Resonance in its own process
and attach a debugger. On Linux this can be achieved with `gdb` or `lldb`. It is important that
[ptrace](https://www.kernel.org/doc/html/latest/admin-guide/LSM/Yama.html) is disabled for this to
work. To disable it, execute `echo 0 | sudo tee /proc/sys/kernel/yama/ptrace_scope`. This will allow
any process to inspect the memory of another process. It is automatically re-enabled after a reboot.

If Resonance has crashed, run `resonance info` and use the reported `USER_CACHE_PATH` to find the
latest backtrace at `$USER_CACHE_PATH/backtrace.log`. The path is selected at runtime, so do not
assume an `~/.cache/ncspot` location or a legacy cache environment variable. For example:

```sh
USER_CACHE_PATH="$(resonance info | sed -n 's/^USER_CACHE_PATH //p')"
sed -n '1,120p' "$USER_CACHE_PATH/backtrace.log"
```

## Compiling
Build the maintained Resonance fork from a checkout of the current source. Installing
`ncspot` from crates.io installs the upstream project. To install only the Rust executable into Cargo's bin directory:

```sh
git clone https://github.com/KanterLabs/resonance
cd resonance
cargo install --path . --locked --bin resonance
```

For the default Unix interface, build the Rust backend and the standalone OpenTUI executable
together. Bun is needed only while building the frontend; the resulting executable is standalone.

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

Keep `target/release/resonance` and `target/release/resonance-opentui` together when installing.
The build also produces `target/release/ncspot`, a legacy executable alias. Use
`resonance --legacy-ui` when the retained Cursive interface is needed. Existing Rust source
consumers should rename imports from `ncspot::...` to `resonance::...` when moving to the renamed
library crate; the executable alias and legacy data-directory lookup are compatibility surfaces.

**You may need to manually set the audio backend on non-Linux OSes.** See [Audio
Backends](#audio-backends).

## Audio Backends
Resonance uses PulseAudio by default. Support for other backends can be enabled with the following
commands.

PortAudio for BSD's or macOS
```sh
cargo build --no-default-features --features portaudio_backend,pancurses_backend
```

Rodio for Windows
```sh
cargo build --no-default-features --features rodio_backend,pancurses_backend
```

## Other Features
Here are some auxiliary features you may wish to enable:

| Feature           | Default | Description                                                                                |
|-------------------|---------|--------------------------------------------------------------------------------------------|
| `album_art`       | on      | Draw the cover in the Now Playing view and beside quick search results, and take the interface tint from it. |
| `cover`           | off     | Add a screen to show the album art.                                                        |
| `mpris`           | on      | Control Resonance via dbus. See [Arch Wiki: MPRIS](https://wiki.archlinux.org/title/MPRIS). |
| `notify`          | on      | Send a notification to show what's playing.                                                |
| `share_clipboard` | on      | Ability to copy the URL of a song/playlist/etc. to system clipboard.                       |

Consult [Cargo.toml](/Cargo.toml) for the full list of supported features.
