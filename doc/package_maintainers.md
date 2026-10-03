# Packaging Resonance

Resonance is the maintained KanterLabs product. The canonical source repository is hosted in
[KanterLabs' Gitea](https://gitea.home.shanekanterman.dev); the public
[KanterLabs/resonance GitHub mirror](https://github.com/KanterLabs/resonance)
is the appropriate public source and issue link. Packaging metadata and release artifacts should
use the `resonance` product name while retaining the original ncspot attribution and license.

## Compilation instructions

Resonance uses Cargo for the Rust backend and a pinned Bun toolchain for the default Unix OpenTUI
frontend. From the repository root, build both executables:

```sh
cargo build --release
cd prototype/opentui
bun install --frozen-lockfile
bun run typecheck
bun test
bun run build
cd ../..
install -m 0755 prototype/opentui/dist/resonance-opentui target/release/resonance-opentui
```

The Rust build produces `target/release/resonance` and the retained `target/release/ncspot`
compatibility alias. The frontend output is a standalone `resonance-opentui` executable and does
not require Bun or Node at runtime. A complete Unix package should install all three executables
together so that the default launcher works; Windows currently uses `resonance --legacy-ui`.

## Generated assets

Generate the man page and shell completions from the same source revision as the binaries. These
files are generated artifacts, not hand-written copies:

```sh
cargo xtask generate-manpage --output misc
cargo xtask generate-shell-completion \
  --shells bash,zsh,fish,elvish,powershell --output misc
```

The resulting package assets are:

- `misc/resonance.1` (generated man page)
- `misc/resonance.bash`, `misc/_resonance`, `misc/resonance.fish`, and `misc/resonance.elv`
  (generated Bash, Zsh, Fish, and Elvish completions)
- `misc/_resonance.ps1` (generated PowerShell completion)
- `misc/resonance.desktop` and `images/resonance.svg` (desktop launcher and icon)
- `LICENSE` and any dependency license/notice files required by the package

If the optional WezTerm launcher is shipped, install
[`scripts/resonance-wezterm`](../scripts/resonance-wezterm) as `resonance-wezterm` in the user's
executable path. It opens or attaches to Resonance in a native terminal window; it is a helper,
not a replacement for `resonance` or `resonance-opentui`.

## Debian packages

The [`cargo-deb`](https://github.com/kornelski/cargo-deb#readme) package can build a Debian package
from the repository metadata:

```sh
python3 scripts/package-release.py --platform linux-x86_64 --version deb \
  --notices-only prototype/opentui/dist/licenses
cargo install --locked --version 3.8.0 cargo-deb
cargo deb --no-build --no-strip
```

The package is generated in `target/debian/`. Verify that its contents include the Resonance
backend, the `ncspot` alias, the standalone OpenTUI executable, desktop/icon assets, generated
completion/man assets, and notices promised by the installation instructions.

## Existing ncspot packages

Homebrew, Scoop, winget, Flatpak, Snap, and distribution packages named `ncspot` are upstream
distribution channels. Keep those names and links explicitly labeled as upstream compatibility or
historical information; do not describe them as Resonance packages. New KanterLabs packages and
release archives use `resonance` and include the retained `ncspot` alias for upgrades.
