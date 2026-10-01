import type { Route, ScreenKey } from "./contracts.js";

export const WORKSPACE_BINDINGS: Record<string, string> = {
  "1": "focus now-playing", "2": "focus queue", "3": "focus library", "4": "focus search",
  "5": "focus playlists", "6": "focus podcasts", "7": "focus radio", "8": "focus settings",
  "9": "focus cast", b: "focus browse", "?": "help", ":": "command palette",
  Space: "playpause", "Shift+r": "radio", l: "toggle appearance", q: "quit", "Ctrl+c": "quit",
};
export function keyBindingName(key: ScreenKey): string {
  const name = (key.name ?? key.sequence ?? "").toLowerCase();
  const aliases: Record<string, string> = { space: "Space", " ": "Space", return: "Enter", enter: "Enter", escape: "Esc", up: "Up", down: "Down", left: "Left", right: "Right", pageup: "PageUp", pagedown: "PageDown", home: "Home", end: "End", backspace: "Backspace", delete: "Delete", tab: "Tab" };
  const base = aliases[name] ?? (/^f\d+$/.test(name) ? name.toUpperCase() : name);
  return `${key.ctrl ? "Ctrl+" : ""}${key.meta ? "Alt+" : ""}${key.shift ? "Shift+" : ""}${base}`;
}
export function routeForCommand(command: string): { route: Route; params?: Record<string, unknown> } | null {
  const trimmed = command.trim();
  if (trimmed === "help") return { route: "help" };
  if (trimmed === "prototype" || trimmed === "nowplaying") return { route: "now-playing" };
  const targets: Record<string, Route> = { playing: "now-playing", nowplaying: "now-playing", "now-playing": "now-playing", queue: "queue", library: "library", search: "search", browse: "browse", playlists: "playlists", podcasts: "podcasts", radio: "radio", settings: "settings", cast: "cast", help: "help" };
  const focus = /^focus\s+(\S+)$/.exec(trimmed);
  if (focus && targets[focus[1]!]) return { route: targets[focus[1]!] };
  const search = /^search\s+(.+)$/.exec(trimmed);
  if (search) return { route: "search", params: { query: search[1] } };
  return null;
}
