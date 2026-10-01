# Resonance OpenTUI prototype

This is the KanterLabs Resonance now-playing surface for ncspot. It uses
`@opentui/core` directly and talks to one existing ncspot Unix IPC socket. It
does not read Spotify credentials or call a network API. Album art stays local
to the prototype as a generated signal tile; the Rust status may still provide
`cover_url` for a later cached-art pass.

From this directory, install Bun 1.4.2 (the worker used
`.tools/bun-linux-x64/bun`) and run:

```sh
PATH="$PWD/.tools/bun-linux-x64:$PATH" bun install --frozen-lockfile
PATH="$PWD/.tools/bun-linux-x64:$PATH" bun run typecheck
PATH="$PWD/.tools/bun-linux-x64:$PATH" bun test
PATH="$PWD/.tools/bun-linux-x64:$PATH" bun run build
```

The standalone binary is `dist/resonance-opentui`. Use `--socket PATH` for a
live ncspot session, `--demo` for an offline preview, and `--smoke` for a
non-interactive parser/command check. The socket client never reconnects after
disconnecting, so a new ncspot process cannot be controlled accidentally.

