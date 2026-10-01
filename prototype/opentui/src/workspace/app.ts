import { BoxRenderable, TextAttributes, TextRenderable, type CliRenderer, type KeyEvent } from "@opentui/core";
import { paletteForTheme, type ThemeName } from "../theme.js";
import { formatTime, type ParsedStatus } from "../status.js";
import { ROUTES, type Params, type Route, type RpcApi, type Screen, type ScreenContext, type ScreenFactory } from "./contracts.js";
import { createNowPlayingScreen } from "../screens/now-playing/index.js";
import { createQueueScreen } from "../screens/queue/index.js";
import { createLibraryScreen } from "../screens/library/index.js";
import { createSearchScreen } from "../screens/search/index.js";
import { createBrowseScreen } from "../screens/browse/index.js";
import { createPlaylistsScreen } from "../screens/playlists/index.js";
import { createPodcastsScreen } from "../screens/podcasts/index.js";
import { createRadioScreen } from "../screens/radio/index.js";
import { createSettingsScreen } from "../screens/settings/index.js";
import { createHelpScreen } from "../screens/help/index.js";
import { createCastScreen } from "../screens/cast/index.js";
import { keyBindingName, routeForCommand, WORKSPACE_BINDINGS } from "./bindings.js";

const FACTORIES: Record<Route, ScreenFactory> = { "now-playing": createNowPlayingScreen, queue: createQueueScreen, library: createLibraryScreen, search: createSearchScreen, browse: createBrowseScreen, playlists: createPlaylistsScreen, podcasts: createPodcastsScreen, radio: createRadioScreen, settings: createSettingsScreen, help: createHelpScreen, cast: createCastScreen };
const SHORTCUTS: Record<string, Route> = { "1": "now-playing", "2": "queue", "3": "library", "4": "search", "5": "playlists", "6": "podcasts", "7": "radio", "8": "settings", "9": "cast", b: "browse", "?": "help" };
const LABELS: Record<Route, string> = { "now-playing": "1 Play", queue: "2 Queue", library: "3 Library", search: "4 Search", browse: "B Browse", playlists: "5 Lists", podcasts: "6 Pods", radio: "7 Radio", settings: "8 Setup", help: "? Help", cast: "9 Cast" };
export interface WorkspaceOptions {
  api: RpcApi;
  theme: ThemeName;
  reducedMotion?: boolean;
  route?: Route;
  onThemeChange?(theme: ThemeName): void;
  onReducedMotionChange?(value: boolean): void;
  onQuit?(): void;
}
export function mountWorkspace(renderer: CliRenderer, options: WorkspaceOptions) {
  let theme = options.theme;
  let reducedMotion = options.reducedMotion ?? false;
  let status: ParsedStatus | null = null;
  let screen: Screen | undefined;
  let route: Route = options.route ?? "now-playing";
  let disposed = false;
  let connection = "Connecting…";
  let noticeText = "";
  let receivedAt = Date.now();
  let lastNoticeId = 0;
  let customBindings: Record<string, string> = {};
  const listeners = new Set<(status: ParsedStatus) => void>();
  const initialPalette = paletteForTheme(theme);
  const root = new BoxRenderable(renderer, { id: "workspace", width: "100%", height: "100%", flexDirection: "column", alignItems: "center", minWidth: 0, minHeight: 0, backgroundColor: initialPalette.background });
  // Keep the terminal-wide background on `root`, while this centered shell
  // owns all of the content chrome and screens.  The 136-column cap leaves
  // quiet horizontal margins on wide terminals without changing compact
  // layouts.
  const shell = new BoxRenderable(renderer, { id: "workspace-shell", width: "100%", maxWidth: 136, height: "100%", flexDirection: "column", paddingX: 2, minWidth: 0, minHeight: 0, backgroundColor: "transparent" });
  const header = new BoxRenderable(renderer, { width: "100%", height: 2, flexShrink: 0, flexDirection: "row", alignItems: "center", justifyContent: "space-between", minWidth: 0 });
  const brand = new TextRenderable(renderer, { content: "RESONANCE  /  KanterLabs", truncate: true, flexGrow: 1, minWidth: 0, fg: initialPalette.text, bg: initialPalette.background, attributes: TextAttributes.BOLD, selectable: false });
  const appearance = new TextRenderable(renderer, { content: "", width: 15, fg: initialPalette.accent, bg: initialPalette.background, textAlign: "right", selectable: false, onMouseDown: () => context.setTheme(theme === "light" ? "dark" : "light") });
  header.add(brand); header.add(appearance);
  const nav = new BoxRenderable(renderer, { width: "100%", height: 3, flexShrink: 0, flexDirection: "row", flexWrap: "wrap", gap: 1, minWidth: 0 });
  const tabs = ROUTES.map((target) => {
    const tab = new BoxRenderable(renderer, { id: `route-tab-${target}`, height: 1, width: LABELS[target].length, flexShrink: 0, justifyContent: "center", backgroundColor: initialPalette.background, onMouseDown: () => navigate(target) });
    const label = new TextRenderable(renderer, { id: `route-${target}`, content: LABELS[target], height: 1, width: "100%", minWidth: 0, bg: "transparent", truncate: true, textAlign: "center", selectable: false });
    tab.add(label); nav.add(tab); return { target, tab, label };
  });
  const content = new BoxRenderable(renderer, { id: "workspace-content", width: "100%", flexGrow: 1, minHeight: 0, minWidth: 0, flexDirection: "column" });
  const footer = new TextRenderable(renderer, { id: "workspace-player", width: "100%", height: 1, flexShrink: 0, truncate: true, fg: initialPalette.text, bg: initialPalette.background, selectable: false, onMouseDown: () => void action("player.action", { action: "play_pause" }) });
  const notice = new TextRenderable(renderer, { id: "workspace-notice", width: "100%", height: 1, flexShrink: 0, truncate: true, fg: initialPalette.muted, bg: initialPalette.background, selectable: false });
  shell.add(header); shell.add(nav); shell.add(content); shell.add(footer); shell.add(notice); root.add(shell); renderer.root.add(root);
  const context: ScreenContext = {
    renderer, api: options.api, theme: () => theme,
    setTheme(next) {
      if (disposed || theme === next) return;
      theme = next; paint(); screen?.setTheme(theme);
      try { options.onThemeChange?.(theme); } catch (error) { notify(`Appearance changed; saving failed: ${error instanceof Error ? error.message : error}`); }
    },
    reducedMotion: () => reducedMotion,
    setReducedMotion(value) {
      if (disposed) return;
      reducedMotion = value;
      try { options.onReducedMotionChange?.(value); } catch (error) { notify(`Motion preference saving failed: ${error instanceof Error ? error.message : error}`); }
      // Re-mount to release any screen animation timer immediately.
      navigate(route);
    },
    status: () => status, onStatus(listener) { listeners.add(listener); return () => listeners.delete(listener); },
    navigate, notify,
  };
  function notify(message: string) { if (disposed) return; noticeText = message; renderFooter(); renderer.requestRender(); }
  async function action(method: string, params: Params = {}) {
    try { await options.api.call(method, params); } catch (error) { notify(error instanceof Error ? error.message : String(error)); }
  }
  async function command(text: string) {
    if (["quit", "q", "x"].includes(text.trim())) { quit(); return { completed: true }; }
    const destination = routeForCommand(text);
    if (destination) { navigate(destination.route, destination.params); return { completed: true }; }
    const movement = /^move\s+(up|down|top|bottom)(?:\s+(\d+))?$/.exec(text.trim());
    if (movement) {
      const keys: Record<string, string> = { up: "up", down: "down", top: "home", bottom: "end" };
      for (let i = 0; i < Math.min(Number(movement[2] ?? 1), 100); i++) screen?.handleKey({ name: keys[movement[1]!] });
      return { completed: true };
    }
    return options.api.call("settings.action", { action: "command", command: text });
  }
  function navigate(target: Route, params: Params = {}) {
    if (disposed) return;
    if (screen) { content.remove(screen.root); screen.dispose(); }
    route = target; screen = FACTORIES[target](context, params); content.add(screen.root);
    if (target === "settings" || target === "help") Promise.resolve(screen.refresh()).catch(error => notify(String(error)));
    paint();
    renderer.requestRender();
  }
  function paint() {
    const p = paletteForTheme(theme);
    root.backgroundColor = p.background; renderer.setBackgroundColor(p.background);
    brand.fg = p.text; appearance.fg = p.accent; appearance.content = `${theme === "light" ? "☼ LIGHT" : "☾ DARK"} · L`;
    tabs.forEach(({ target, tab, label }) => { tab.backgroundColor = target === route ? p.panelRaised : p.background; label.fg = target === route ? p.accentBright : p.muted; label.bg = "transparent"; });
    brand.bg = p.background; appearance.bg = p.background; footer.fg = p.text; footer.bg = p.background; notice.fg = p.muted; notice.bg = p.background;
    renderFooter();
  }
  function renderFooter() {
    const playable = status?.playable;
    const title = playable ? (playable.type === "Episode" ? playable.name : playable.title) : "Choose a track";
    const playing = status?.mode.kind === "playing";
    const position = status?.prototype?.position_ms ?? (status?.mode.kind === "paused" ? status.mode.positionMs : 0);
    const elapsed = position + (playing ? Math.max(0, Date.now() - receivedAt) : 0);
    footer.content = `${playing ? "▶" : "Ⅱ"} ${title}  ${formatTime(Math.min(playable?.duration ?? 0, elapsed))}/${formatTime(playable?.duration ?? 0)}  · Vol ${status?.prototype?.volume_percent ?? "—"}%`;
    notice.content = noticeText || `${connection} · Space play/pause · Shift+R radio · : commands · q quit`;
  }
  function onKey(key: KeyEvent) {
    if (disposed) return;
    // Focused prompts must receive normal text, including navigation digits.
    const wasEditing = screen?.editing?.() ?? false;
    const name = (key.name ?? key.sequence ?? "").toLowerCase();
    if (key.ctrl && name === "c") { quit(); key.preventDefault(); return; }
    if (!wasEditing) {
      const custom = customBindings[keyBindingName(key)];
      if (custom) { void command(custom).catch(error => notify(error instanceof Error ? error.message : String(error))); key.preventDefault(); return; }
    }
    if (!wasEditing && !key.ctrl && !key.meta) {
      if (SHORTCUTS[name]) { navigate(SHORTCUTS[name]); key.preventDefault(); return; }
      if (name === "l") { context.setTheme(theme === "light" ? "dark" : "light"); key.preventDefault(); return; }
      if (name === "q" || name === "f5") { quit(); key.preventDefault(); return; }
      if (name === ":" || key.sequence === ":") { navigate("help", { command: true }); key.preventDefault(); return; }
      if (name === "r" && key.shift) {
        // Let the player own its action feedback and current-track seed.
        if (route !== "now-playing" || !screen?.handleKey(key)) void action("radio.action", { action: "start" });
        key.preventDefault(); return;
      }
    }
    if (screen?.handleKey(key)) { key.preventDefault(); return; }
    if (wasEditing || screen?.editing?.() || key.ctrl || key.meta) return;
    if (name === "q" || name === "f5") { quit(); key.preventDefault(); }
    else if (name === "space" || key.sequence === " ") { void action("player.action", { action: "play_pause" }); key.preventDefault(); }
    else if (name === "escape") { navigate("now-playing"); key.preventDefault(); }
  }
  function dispose() {
    if (disposed) return; disposed = true;
    clearInterval(timer); renderer.keyInput.off("keypress", onKey); renderer.off("resize", onResize);
    screen?.dispose(); screen = undefined; listeners.clear(); root.destroyRecursively();
  }
  function quit() { if (disposed) return; dispose(); options.onQuit?.(); renderer.destroy(); }
  function onResize() {
    const available = Math.max(1, Math.min(136, renderer.width) - 4);
    const total = tabs.reduce((sum, tab) => sum + tab.tab.width, 0) + Math.max(0, tabs.length - 1);
    nav.height = total <= available ? 1 : 3;
    renderer.requestRender();
  }
  const timer = setInterval(() => { if (status?.mode.kind === "playing") { renderFooter(); renderer.requestRender(); } }, 1000);
  renderer.keyInput.on("keypress", onKey); renderer.on("resize", onResize);
  navigate(route); onResize();
  return {
    navigate, dispose, quit, command, refresh() { return screen?.refresh(); }, get route() { return route; }, get context() { return context; },
    setBindings(bindings: unknown) {
      customBindings = bindings && typeof bindings === "object" && !Array.isArray(bindings)
        ? Object.fromEntries(Object.entries(bindings).filter((entry): entry is [string, string] => typeof entry[1] === "string")) : {};
    },
    bindings() { return { ...WORKSPACE_BINDINGS, ...customBindings }; },
    setConnection(connected: boolean, message: string) { if (disposed) return; connection = connected ? "Live engine" : message; renderFooter(); },
    setStatus(next: ParsedStatus) {
      if (disposed) return; status = next; receivedAt = Date.now();
      for (const entry of next.notifications ?? []) {
        if (entry.id > lastNoticeId) { lastNoticeId = entry.id; notify(entry.message); }
      }
      for (const listener of listeners) listener(next);
      renderFooter();
    },
    notify,
  };
}
