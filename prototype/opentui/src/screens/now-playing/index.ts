import { BoxRenderable, TextRenderable, MouseButton } from "@opentui/core";
import type { ScreenFactory, Params } from "../../workspace/contracts.js";
import { createSurface } from "../../workspace/surface.js";
import { formatTime, initials, playableTitle, playableArtists, playableAlbum, positionAt } from "../../status.js";
import { paletteForTheme } from "../../theme.js";

/** Ambient graphics follow playback progress; they are not audio samples. */
export function ambientProgress(position: number, duration: number, width = 36): string {
  const fraction = duration > 0 ? Math.min(1, Math.max(0, position / duration)) : 0;
  return Array.from({ length: width }, (_, i) => i < Math.floor(width * fraction) ? "━" : "─").join("");
}

/** Draw only supplied, normalized audio measurements; zero remains silent. */
export function audioSpectrum(bands: readonly number[], width = 36): string {
  const chars = "▁▂▃▄▅▆▇█";
  if (!bands.length) return "─".repeat(width);
  return Array.from({ length: width }, (_, index) => {
    const value = Math.min(1, Math.max(0, bands[Math.floor(index * bands.length / width)] ?? 0));
    return value <= 0 ? "─" : chars[Math.min(7, Math.floor(value * 8))];
  }).join("");
}

export const createNowPlayingScreen: ScreenFactory = (ctx) => {
  const surface = createSurface(ctx, "Now Playing", "Space play/pause · [ ] seek · − + volume · R radio · f save");
  let status = ctx.status();
  let receivedAt = Date.now();
  let positionBase = status ? positionAt(status, receivedAt) : 0;
  let disposed = false;
  let timer: ReturnType<typeof setInterval> | undefined;
  let repeat = "unknown";
  let shuffle: boolean | undefined;
  const saved = new Set<string>();
  let pending = false;
  let theme = ctx.theme();
  let displayedBands: number[] = [];
  const texts: TextRenderable[] = [];
  const panels: BoxRenderable[] = [];
  const text = (id: string, parent: BoxRenderable, height = 1) => {
    const item = new TextRenderable(ctx.renderer, { id: `np-${id}`, height, width: "100%", content: "" });
    parent.add(item); texts.push(item); return item;
  };
  const hero = new BoxRenderable(ctx.renderer, { id: "np-hero", width: "100%", height: 7, border: true, paddingLeft: 2, paddingRight: 2, flexDirection: "column" });
  panels.push(hero); surface.body.add(hero);
  const cover = text("cover", hero);
  const title = text("title", hero);
  const artist = text("artist", hero);
  const album = text("album", hero);
  const mode = text("mode", hero);
  const timeline = text("timeline", surface.body, 2);
  const ambient = text("ambient", surface.body, 2);
  const controlRow = (id: string) => {
    const row = new BoxRenderable(ctx.renderer, { id, height: 3, width: "100%", flexDirection: "row", gap: 1 });
    surface.body.add(row); return row;
  };
  function button(parent: BoxRenderable, id: string, label: string, action: () => void): TextRenderable {
    const node = new TextRenderable(ctx.renderer, { id: `np-${id}`, content: label, height: 3, flexGrow: 1, paddingTop: 1, paddingLeft: 1, onMouseDown: (event) => { if (event.button === MouseButton.LEFT) action(); } });
    texts.push(node); parent.add(node); return node;
  }
  const transport = controlRow("np-transport");
  button(transport, "previous", "│◀ Previous", () => void player("previous", "Previous requested"));
  const playButton = button(transport, "play", "▶ Play", () => void player("play_pause", "Playback toggled"));
  button(transport, "next", "Next ▶│", () => void player("next", "Next requested"));
  button(transport, "stop", "■ Stop", () => void player("stop", "Playback stopped"));
  const options = controlRow("np-options");
  const repeatButton = button(options, "repeat", "Repeat", () => void cycleRepeat());
  const shuffleButton = button(options, "shuffle", "Shuffle", () => void toggleShuffle());
  const savedButton = button(options, "save", "♡ Save", () => void toggleSaved());
  button(options, "radio", "R Radio", () => void startRadio());
  const volume = controlRow("np-volume");
  button(volume, "seek-back", "[ −5s", () => void seek(-5000));
  button(volume, "seek-forward", "] +5s", () => void seek(5000));
  button(volume, "quieter", "− Quieter", () => void changeVolume(-5));
  button(volume, "louder", "+ Louder", () => void changeVolume(5));
  const detail = text("detail", surface.body);
  const next = text("up-next", surface.body, 2);
  const navigation = controlRow("np-navigation");
  button(navigation, "queue", "Queue →", () => ctx.navigate("queue"));
  button(navigation, "radio-page", "Radio studio →", () => ctx.navigate("radio"));
  button(navigation, "share", "Y Share", () => void shareCurrent());
  button(navigation, "motion", "Motion", () => { ctx.setReducedMotion(!ctx.reducedMotion()); });

  function currentPosition() {
    return Math.min(status?.playable?.duration ?? 0, positionBase + (status?.mode.kind === "playing" ? Math.max(0, Date.now() - receivedAt) : 0));
  }
  function uri() {
    const p = status?.playable;
    return p?.uri ?? (p?.id ? `spotify:${p.type === "Episode" ? "episode" : "track"}:${p.id}` : undefined);
  }
  function notice(message: string, error = false) { if (!disposed) { surface.setMessage(message, error); ctx.notify(message); } }
  async function mutate(method: string, params: Params, message: string, acknowledged?: () => void) {
    if (pending || disposed) return;
    pending = true; surface.setMessage("Applying…");
    try { await ctx.api.call(method, params); if (!disposed) { acknowledged?.(); notice(message); render(); } }
    catch (error) { notice(`Unable to apply: ${error instanceof Error ? error.message : String(error)}`, true); }
    finally { pending = false; }
  }
  function player(action: string, message: string, value?: unknown, acknowledged?: () => void) {
    return mutate("player.action", { action, ...(value === undefined ? {} : { value }) }, message, acknowledged);
  }
  function seek(delta: number) {
    if (!status?.playable) { notice("Choose a track or episode before seeking"); return; }
    const target = Math.max(0, Math.min(status.playable.duration, currentPosition() + delta));
    return player("seek", `Seeked to ${formatTime(target)}`, Math.round(target));
  }
  function changeVolume(delta: number) {
    if (!status?.prototype) { notice("Volume is unavailable until playback status arrives"); return; }
    const value = Math.max(0, Math.min(100, status.prototype.volume_percent + delta));
    return player("volume", `Volume set to ${value}%`, value);
  }
  function cycleRepeat() {
    const value = repeat === "off" || repeat === "unknown" ? "all" : repeat === "all" ? "track" : "off";
    return player("repeat", `Repeat ${value}`, value, () => { repeat = value; });
  }
  function toggleShuffle() { const value = !shuffle; return player("shuffle", `Shuffle ${value ? "on" : "off"}`, value, () => { shuffle = value; }); }
  function toggleSaved() {
    const identity = uri(); if (!identity) { notice("Choose an item before saving"); return; }
    const value = !saved.has(identity);
    return mutate("library.action", { action: value ? "save" : "unsave", kind: status?.playable?.type === "Episode" ? "episode" : "track", uri: identity }, value ? "Saved to your library" : "Removed from your library", () => { value ? saved.add(identity) : saved.delete(identity); });
  }
  function startRadio() {
    const identity = uri(); if (!identity || status?.playable?.type === "Episode") { notice("Choose a track to start radio"); return; }
    return mutate("radio.action", { action: "start", uri: identity }, "Radio started from this track");
  }
  async function shareCurrent() {
    const identity = uri();
    if (!identity) { notice("Choose an item before sharing"); return; }
    if (disposed || pending) return;
    pending = true; surface.setMessage("Creating share link…");
    try {
      const result = await ctx.api.call<{ url: string }>("share", { uri: identity });
      if (typeof result.url !== "string" || !result.url) throw new Error("Share link unavailable");
      if (!disposed) notice(result.url);
    } catch (error) { notice(`Unable to share: ${error instanceof Error ? error.message : String(error)}`, true); }
    finally { pending = false; }
  }
  function render() {
    if (disposed) return;
    const p = status?.playable ?? null;
    const position = currentPosition();
    const duration = p?.duration ?? 0;
    const width = Math.max(12, Math.min(64, ctx.renderer.width - 25));
    cover.content = `◈  ${initials(p)}  ·  Cover initials`;
    title.content = playableTitle(p);
    artist.content = playableArtists(p);
    album.content = playableAlbum(p);
    mode.content = `${status?.mode.kind.toUpperCase() ?? "WAITING FOR STATUS"}${status?.prototype?.radio_active ? "  ·  RADIO" : ""}`;
    timeline.content = `${formatTime(position)}  ${ambientProgress(position, duration, width)}  ${formatTime(duration)}`;
    const audio = status?.prototype?.audio;
    const compact = ctx.renderer.height < 29;
    if (audio) {
      const active = status?.mode.kind === "playing" && audio.level > 0;
      const target = active ? audio.bands : audio.bands.map(() => 0);
      if (ctx.reducedMotion() || !active || displayedBands.length !== target.length) displayedBands = [...target];
      const spectrum = audioSpectrum(displayedBands, width);
      const label = active && audio.level > 0 ? "Audio spectrum" : "Audio spectrum · silence";
      ambient.content = compact ? `${label}  ${audioSpectrum(displayedBands, Math.max(12, width - 12))}` : `${label}\n${spectrum}`;
    } else {
      const phase = ctx.reducedMotion() ? 0 : Math.floor(position / 1000);
      ambient.content = `Ambient · playback progress\n${Array.from({ length: width }, (_, i) => "▁▂▃▄▃▂"[(i + phase) % 6]).join("")}`;
    }
    playButton.content = status?.mode.kind === "playing" ? "Ⅱ Pause" : "▶ Play";
    repeatButton.content = `Repeat: ${repeat}`;
    shuffleButton.content = `Shuffle: ${shuffle === undefined ? "—" : shuffle ? "on" : "off"}`;
    savedButton.content = saved.has(uri() ?? "") ? "♥ Saved" : "♡ Save";
    detail.content = `Volume ${status?.prototype ? `${status.prototype.volume_percent}%` : "—"}  ·  ${ctx.reducedMotion() ? "Reduced motion" : "Ambient motion"}`;
    const upNext = status?.prototype?.up_next ?? [];
    next.content = upNext.length ? `UP NEXT  ${upNext[0]!.title}\n${upNext[0]!.artists.join(" · ")}` : "UP NEXT  Queue is empty";
    // Short terminals retain track details and all transport controls.
    hero.height = compact ? 5 : 7;
    album.visible = !compact;
    mode.visible = !compact;
    timeline.height = compact ? 1 : 2;
    for (const row of [transport, options, volume]) {
      row.height = compact ? 1 : 3;
      for (const child of row.getChildren()) { child.height = compact ? 1 : 3; child.paddingTop = compact ? 0 : 1; }
    }
    ambient.visible = !!audio || !compact;
    ambient.height = compact ? 1 : 2;
    next.visible = ctx.renderer.height >= 26;
    navigation.visible = !compact;
  }
  function syncTimer() {
    if (timer) { clearInterval(timer); timer = undefined; }
    if (!disposed && status?.mode.kind === "playing" && !ctx.reducedMotion()) {
      timer = setInterval(() => {
        if (disposed || !surface.root.visible || !surface.root.parent || ctx.reducedMotion()) { if (timer) clearInterval(timer); timer = undefined; return; }
        const audio = status?.prototype?.audio;
        if (audio) {
          displayedBands = audio.bands.map((band, index) => {
            const value = audio.level > 0 ? band : 0;
            const previous = displayedBands[index] ?? 0;
            return value <= 0 ? 0 : previous + (value - previous) * 0.35;
          });
        }
        render();
      }, status?.prototype?.audio ? 80 : 1000);
    }
  }
  async function loadMetadata() {
    const identity = uri();
    try {
      const result = await ctx.api.call<{ repeat?: string; shuffle?: boolean; saved?: boolean | null; current?: { uri?: string } | null }>("player.status");
      if (disposed || identity !== uri()) return;
      if (result.repeat) repeat = result.repeat === "playlist" ? "all" : result.repeat;
      if (typeof result.shuffle === "boolean") shuffle = result.shuffle;
      if (identity && result.current?.uri === identity && typeof result.saved === "boolean") result.saved ? saved.add(identity) : saved.delete(identity);
      render();
    } catch (error) { notice(`Playback details unavailable: ${error instanceof Error ? error.message : String(error)}`, true); }
  }
  const unsubscribe = ctx.onStatus((value) => { const previous = uri(); status = value; receivedAt = Date.now(); positionBase = positionAt(value, receivedAt); render(); syncTimer(); if (uri() !== previous) void loadMetadata(); });
  const onResize = () => render(); ctx.renderer.on("resize", onResize);
  function applyTheme() {
    const colors = paletteForTheme(theme);
    surface.setTheme(theme);
    for (const node of texts) { node.fg = colors.text; node.bg = colors.panel; }
    for (const panel of panels) { panel.borderColor = colors.coverBorder; panel.backgroundColor = colors.cover; }
    cover.fg = colors.accent; artist.fg = colors.muted; album.fg = colors.dim; ambient.fg = colors.accent; mode.fg = colors.teal;
  }
  applyTheme(); render(); syncTimer(); void loadMetadata();
  return {
    root: surface.root, title: "Now Playing", editing: () => surface.editing(),
    handleKey(key) {
      if (surface.editing()) return surface.handleKey(key);
      const name = key.name ?? key.sequence ?? "";
      if (key.ctrl || key.meta) return false;
      if (name === "space" || name === " ") { void player("play_pause", "Playback toggled"); return true; }
      if (name === ",") { void player("previous", "Previous requested"); return true; }
      if (name === ".") { void player("next", "Next requested"); return true; }
      if (name === "[" || name === "leftbracket" || key.sequence === "[") { void seek(-5000); return true; }
      if (name === "]" || name === "rightbracket" || key.sequence === "]") { void seek(5000); return true; }
      if (name === "-" || name === "minus" || key.sequence === "-") { void changeVolume(-5); return true; }
      if (name === "+" || name === "plus" || key.sequence === "+") { void changeVolume(5); return true; }
      if (name === "R" || (name === "r" && key.shift)) { void startRadio(); return true; }
      if (name === "r") { void cycleRepeat(); return true; }
      if (name === "s") { void toggleShuffle(); return true; }
      if (name === "f") { void toggleSaved(); return true; }
      if (name === "y") { void shareCurrent(); return true; }
      return false;
    },
    async refresh() { render(); syncTimer(); await loadMetadata(); },
    setTheme(value) { theme = value; applyTheme(); render(); },
    dispose() { if (disposed) return; disposed = true; if (timer) clearInterval(timer); unsubscribe(); ctx.renderer.off("resize", onResize); surface.dispose(); },
  };
};
