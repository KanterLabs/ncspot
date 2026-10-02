import { BoxRenderable, TextRenderable, MouseButton, RGBA, StyledText, TextAttributes } from "@opentui/core";
import type { Row, ScreenFactory } from "../../workspace/contracts.js";
import { formatTime, initials, parseTrack, playableArtists, playableTitle, type ParsedStatus, type Track, type UpNextOrigin } from "../../status.js";
import { paletteForTheme, type ThemePalette } from "../../theme.js";
import { createSurface } from "../../workspace/surface.js";
import { artworkLines, type Artwork } from "../now-playing/index.js";
import { playingBars } from "../now-playing/motion.js";
import { artworkTint, blendHex } from "../now-playing/motion-colors.js";
import { RadioController } from "./controller.js";
import { discoveryLevel } from "./model.js";

type QueueMode = "station" | "context";

function nonNegativeCount(value: unknown): number | undefined {
  return typeof value === "number" && Number.isFinite(value) && value >= 0 ? Math.round(value) : undefined;
}

function formatCount(value: number): string {
  return value.toLocaleString("en-US");
}

function queueMode(value: unknown): QueueMode | undefined {
  return value === "station" || value === "context" ? value : undefined;
}

function originMark(origin: UpNextOrigin | undefined): string {
  if (origin === "explicit") return "+";
  if (origin === "radio") return "◇";
  if (origin === "context") return "·";
  return "";
}

export const createRadioScreen: ScreenFactory = ctx => {
  let colors = paletteForTheme(ctx.theme());
  let live: ParsedStatus | null = ctx.status();
  let controller: RadioController | undefined;
  let disposed = false;
  let diagnostics = false;
  let details = false;
  let selectedId: string | undefined;
  let lines: string[] = [];
  let candidates: Row[] = [];
  let artworkKey = "";
  let artworkGeneration = 0;
  let artWidth = 12;
  let artHeight = 6;
  let artLines: StyledText[] | null = null;
  let tint: string | null = null;
  let diagnosticSeed = "42";

  const root = new BoxRenderable(ctx.renderer, { id: "radio-root", width: "100%", height: "100%", minWidth: 0, minHeight: 0, flexDirection: "column", justifyContent: "center", alignItems: "center" });
  const columns = new BoxRenderable(ctx.renderer, { id: "radio-columns", width: "100%", maxWidth: 132, minWidth: 0, flexDirection: "row", gap: 3, justifyContent: "center" }); root.add(columns);
  const card = new BoxRenderable(ctx.renderer, { id: "radio-station-card", width: 85, height: 24, flexShrink: 0, border: true, borderStyle: "rounded", paddingX: 2, paddingY: 1, flexDirection: "column" });
  const nextCard = new BoxRenderable(ctx.renderer, { id: "radio-next-card", width: 44, height: 24, flexShrink: 0, minWidth: 0, border: true, borderStyle: "rounded", paddingX: 2, paddingY: 1, flexDirection: "column" }); columns.add(card); columns.add(nextCard);
  const texts: Array<{ node: TextRenderable; tone: keyof ThemePalette }> = [];
  const buttons: Array<{ box: BoxRenderable; text: TextRenderable; primary: boolean }> = [];
  const noBorders: [] = [];

  function text(parent: BoxRenderable, id: string, content = "", options: ConstructorParameters<typeof TextRenderable>[1] = {}, tone: keyof ThemePalette = "text") {
    const node = new TextRenderable(ctx.renderer, { id: `radio-${id}`, content, height: 1, flexShrink: 0, selectable: false, truncate: true, fg: colors[tone], bg: "transparent", ...options }); parent.add(node); texts.push({ node, tone }); return node;
  }
  function row(parent: BoxRenderable, id: string, height = 1, options: ConstructorParameters<typeof BoxRenderable>[1] = {}) {
    const node = new BoxRenderable(ctx.renderer, { id: `radio-${id}`, width: "100%", height, flexShrink: 0, minWidth: 0, flexDirection: "row", alignItems: "center", ...options }); parent.add(node); return node;
  }
  function button(parent: BoxRenderable, id: string, label: string, action: () => void, primary = false, width?: number) {
    const box = new BoxRenderable(ctx.renderer, { id: `radio-${id}`, ...(width ? { width, flexShrink: 0 } : { flexGrow: 1, flexBasis: 0, minWidth: 0 }), height: 3, border: true, borderStyle: "rounded", alignItems: "center", justifyContent: "center", onMouseDown: event => { if (event.button === MouseButton.LEFT) { action(); event.preventDefault(); } } });
    parent.add(box); const labelNode = text(box, `${id}-label`, label, { width: "100%", textAlign: "center", attributes: primary ? TextAttributes.BOLD : undefined }, primary ? "accentBright" : "muted"); buttons.push({ box, text: labelNode, primary }); return { box, text: labelNode };
  }

  const heading = row(card, "heading", 2, { justifyContent: "space-between" });
  text(heading, "heading-title", "RADIO STUDIO", { width: 15, attributes: TextAttributes.BOLD });
  const state = text(heading, "state", "CONNECTING", { flexGrow: 1, minWidth: 0, textAlign: "right" }, "teal");
  const hero = row(card, "hero", 6, { gap: 3 });
  const cover = new BoxRenderable(ctx.renderer, { id: "radio-cover", width: 14, height: 6, flexShrink: 0, border: ["left", "right"], flexDirection: "column", justifyContent: "center", alignItems: "center" }); hero.add(cover);
  const fallback = text(cover, "cover-fallback", "R", { width: "100%", textAlign: "center", attributes: TextAttributes.BOLD }, "accent");
  const artNodes: TextRenderable[] = [];
  const metadata = new BoxRenderable(ctx.renderer, { id: "radio-seed", flexGrow: 1, minWidth: 0, height: "100%", flexDirection: "column", gap: 1 }); hero.add(metadata);
  const seedLabel = text(metadata, "seed-label", "STATION SEED", { width: "100%" }, "accent");
  const seedTitle = text(metadata, "seed-title", "Choose a song", { width: "100%", height: 2, wrapMode: "word", attributes: TextAttributes.BOLD });
  const seedArtist = text(metadata, "seed-artist", "Start radio from the song playing now.", { width: "100%" }, "muted");
  row(card, "spacer");
  const discoveryHeading = row(card, "discovery-heading", 1, { justifyContent: "space-between" });
  text(discoveryHeading, "discovery-label", "DISCOVERY", { width: 12 }, "dim");
  const discoveryValue = text(discoveryHeading, "discovery-value", "—", { flexGrow: 1, minWidth: 0, textAlign: "right" }, "accentBright");
  const slider = text(card, "slider", "", { width: "100%", onMouseDown: event => {
    if (event.button !== MouseButton.LEFT) return;
    const value = Math.round(Math.max(0, Math.min(1, (event.x - slider.x) / Math.max(1, slider.width - 1))) * 100);
    void controller?.setDiscovery(value); event.preventDefault();
  } }, "accent");
  const scale = row(card, "scale", 1, { justifyContent: "space-between" });
  text(scale, "scale-familiar", "Familiar", { width: 10 }, "muted");
  text(scale, "scale-balanced", "Balanced", { width: 10, textAlign: "center" }, "muted");
  text(scale, "scale-explore", "Explore", { width: 10, textAlign: "right" }, "muted");
  const presets = row(card, "presets", 3, { gap: 2 });
  const familiar = button(presets, "familiar", "Familiar  F", () => void controller?.setDiscovery(0));
  const balanced = button(presets, "balanced", "Balanced  M", () => void controller?.setDiscovery(50));
  const explore = button(presets, "explore", "Explore  E", () => void controller?.setDiscovery(100));
  const caption = text(card, "caption", "", { width: "100%", height: 2 }, "muted");
  const actions = row(card, "actions", 3, { gap: 2, justifyContent: "space-between" });
  const toggle = button(actions, "toggle", "Start radio  S", () => toggleRadio(), true, 20);
  const reseed = button(actions, "reseed", "Use current song", () => startCurrent(), false, 23);
  button(actions, "diagnostics", "Diagnostics D", () => openDiagnostics(), false, 15);

  const nextHeading = row(nextCard, "next-heading", 2, { justifyContent: "space-between" });
  const nextTitle = text(nextHeading, "next-title", "UP NEXT", { width: 18 }, "dim");
  text(nextHeading, "queue-link", "Queue →", { width: 9, textAlign: "right", onMouseDown: event => { if (event.button === MouseButton.LEFT) { ctx.navigate("queue"); event.preventDefault(); } } }, "accent");
  const nextBody = new BoxRenderable(ctx.renderer, { id: "radio-next-body", flexGrow: 1, minHeight: 0, minWidth: 0, flexDirection: "column", overflow: "hidden" }); nextCard.add(nextBody);
  const nextItems = Array.from({ length: 15 }, (_, index) => {
    const entry = row(nextBody, `next-${index}`, 1, { gap: 1, onMouseDown: event => { if (event.button === MouseButton.LEFT) { ctx.navigate("queue"); event.preventDefault(); } } });
    text(entry, `next-number-${index}`, String(index + 1).padStart(2, "0"), { width: 2 }, "dim");
    const origin = text(entry, `next-origin-${index}`, "", { width: 2, textAlign: "center" }, "dim");
    return { root: entry, origin, title: text(entry, `next-name-${index}`, "", { flexGrow: 1, flexShrink: 1, flexBasis: 0, minWidth: 0 }), duration: text(entry, `next-duration-${index}`, "", { width: 5, textAlign: "right" }, "muted") };
  });
  const empty = text(nextBody, "next-empty", "", { width: "100%", height: 4 }, "muted");
  const nextFoot = text(nextCard, "next-footer", "", { width: "100%", height: 3 }, "dim");

  const debug = createSurface(ctx, "Radio diagnostics", "↑/↓ candidates · Enter score details · d rerun · Esc studio"); debug.root.visible = false; root.add(debug.root);

  function currentTrack(): Track | null { return live?.playable && live.playable.type !== "Episode" ? live.playable : null; }
  function seedTrack(): Track | null {
    const status = controller?.status ?? {};
    if (status.active === undefined && live?.prototype?.radio_active) return null;
    const cached = status.seed_track ? parseTrack(status.seed_track) : null;
    if (status.active === true) {
      if (cached) return cached;
      const current = currentTrack();
      return current && typeof status.seed === "string" && status.seed === current.uri ? current : null;
    }
    return currentTrack();
  }
  function startCurrent() {
    const track = currentTrack();
    const uri = track?.uri || (track?.id ? `spotify:track:${track.id}` : undefined);
    if (!uri) { ctx.notify("Choose a song before starting radio"); ctx.navigate("search"); return; }
    void controller?.action("start", undefined, uri);
  }
  function toggleRadio() { if (controller?.status.active === true) void controller.action("stop"); else startCurrent(); }
  function showDebug() {
    diagnostics = true; details = false; columns.visible = false; debug.root.visible = true;
    debug.setLines(lines); debug.setRows(candidates, selectedId, showDetail);
  }
  function openDiagnostics() {
    showDebug();
    debug.prompt("Diagnostic RNG seed", diagnosticSeed, seed => { diagnosticSeed = seed; void controller?.debug(seed); });
  }
  function showDetail(value: Row) {
    if (value.kind !== "radio-candidate") return;
    details = true; selectedId = value.id;
    debug.setRows([]); debug.setLines([value.title, value.uri ?? "URI unavailable", ...value.subtitle.split(" · "), "Score components", ...(value.detail ?? "unavailable").split(" · "), "Esc to return to candidates"]);
  }
  function closeDebug() { diagnostics = details = false; debug.root.visible = false; columns.visible = true; render(); }
  function paintCover() {
    while (artNodes.length > (artLines?.length ?? 0)) { const node = artNodes.pop()!; cover.remove(node); node.destroy(); }
    fallback.visible = !artLines;
    artLines?.forEach((line, index) => {
      let node = artNodes[index];
      if (!node) { node = new TextRenderable(ctx.renderer, { id: `radio-art-${index}`, content: line, width: artWidth, height: 1, flexShrink: 0, selectable: false }); artNodes.push(node); cover.add(node); }
      else { node.width = artWidth; node.content = line; }
    });
    cover.backgroundColor = colors.cover; cover.borderColor = blendHex(colors.coverBorder, tint ?? colors.accent, 0.4);
  }
  async function loadArtwork(track: Track | null) {
    const uri = track?.uri || (track?.id ? `spotify:track:${track.id}` : "");
    const key = `${uri}:${artWidth}:${artHeight}`;
    if (key === artworkKey || disposed) return;
    artworkKey = key; const generation = ++artworkGeneration; artLines = null; tint = null; paintCover();
    if (!uri) return;
    try {
      const result = await ctx.api.call<Artwork>("player.artwork", { uri, width: artWidth, height: artHeight });
      if (disposed || generation !== artworkGeneration || result.uri !== uri || result.width !== artWidth || result.height !== artHeight) return;
      artLines = artworkLines(result); tint = artLines ? artworkTint(result.pixels!) : null; paintCover();
    } catch { /* A missing cover never prevents station controls. */ }
  }
  function render() {
    if (disposed) return;
    const status = controller?.status ?? {};
    const compact = ctx.renderer.height < 30;
    const sideBySide = !compact && ctx.renderer.width >= 112;
    const available = Math.min(132, ctx.renderer.width - 4);
    const width = sideBySide ? Math.max(68, available - 47) : Math.min(85, available);
    card.width = width; card.height = compact ? Math.max(16, ctx.renderer.height - 7) : 24; card.paddingY = compact ? 0 : 1;
    nextCard.visible = sideBySide; if (sideBySide) nextCard.width = available - width - 3; nextCard.height = card.height; columns.height = card.height;
    heading.height = compact ? 1 : 2; hero.height = compact ? 4 : 6; metadata.gap = compact ? 0 : 1; seedTitle.height = compact ? 1 : 2;
    presets.height = actions.height = compact ? 1 : 3;
    for (const item of buttons) { item.box.height = compact ? 1 : 3; item.box.border = compact ? noBorders : true; }
    const wantedHeight = compact ? 4 : 6;
    if (artHeight !== wantedHeight) { artHeight = wantedHeight; artWidth = wantedHeight * 2; cover.width = artWidth + 2; cover.height = artHeight; }
    const active = status.active === true;
    const waiting = status.waiting === true;
    const audio = live?.prototype?.audio;
    const upcoming = live?.prototype?.up_next ?? [];
    const parsedOrigins = live?.prototype?.up_next_origins ?? [];
    const origins = parsedOrigins.length === upcoming.length ? parsedOrigins : [];
    const mode = queueMode(status.queue_mode);
    const stationKnown = active && (mode === "station" || origins.includes("radio"));
    const bars = playingBars(live?.mode.kind === "playing", audio?.bands, audio?.level, Date.now(), ctx.reducedMotion());
    state.content = active
      ? waiting && !upcoming.length
        ? "◌ RELATED CACHE EXHAUSTED"
        : waiting
          ? "◌ STATION QUEUED"
          : `${bars}  LIVE STATION`
      : status.active === false ? "○ RADIO OFF" : "CONNECTING…";
    state.fg = waiting ? colors.amber : active ? colors.teal : colors.muted;
    const seed = seedTrack();
    seedLabel.content = active ? "STATION SEED" : "START FROM NOW PLAYING";
    seedTitle.content = seed ? playableTitle(seed) : active ? "Your radio station" : live?.playable?.type === "Episode" ? playableTitle(live.playable) : "Choose a song";
    seedArtist.content = seed ? playableArtists(seed) : active ? "Seed details are unavailable in the cache." : live?.playable?.type === "Episode" ? "Choose a song to start radio." : "Pick a track in Search or your Library.";
    fallback.content = seed ? initials(seed) : "R";
    const confirmed = discoveryLevel(status.discovery) ?? discoveryLevel(live?.prototype?.discovery);
    const desired = controller?.desiredDiscovery;
    const level = desired ?? confirmed;
    const label = level === undefined ? "Unavailable" : level < 25 ? "Familiar" : level > 75 ? "Explore" : "Balanced";
    discoveryValue.content = level === undefined ? "Unavailable · R refresh" : `${label}  ${level}%${desired === undefined ? "" : " · saving…"}`;
    const sliderWidth = Math.max(12, width - 6);
    const knob = level === undefined ? -1 : Math.round((sliderWidth - 1) * level / 100);
    slider.content = new StyledText(Array.from({ length: sliderWidth }, (_, index) => ({ __isChunk: true as const, text: index === knob ? "●" : index < knob ? "━" : "─", fg: RGBA.fromHex(index <= knob ? colors.accent : colors.progressTrack), bg: RGBA.fromHex(colors.panel) })));
    for (const [preset, selected] of [[familiar, level === 0], [balanced, level === 50], [explore, level === 100]] as const) {
      preset.box.backgroundColor = selected ? colors.cover : colors.panel;
      preset.box.borderColor = selected ? colors.accentDim : colors.borderSoft;
      preset.text.fg = selected ? colors.accentBright : colors.muted;
      preset.text.attributes = selected ? TextAttributes.BOLD : TextAttributes.NONE;
    }
    const summary = level === undefined ? "Refresh to load your station settings." : level < 25 ? "More familiar related artists and favorite tracks." : level > 75 ? "More unplayed songs from related artists." : "A mix of related favorites and fresh directions.";
    caption.content = `${summary}\nRelated songs only · Explore stays cache-bound.`;
    toggle.text.content = active ? "Stop radio  S" : currentTrack() ? "Start radio  S" : "Choose a song";
    reseed.box.visible = active && !!currentTrack();
    nextItems.forEach((entry, index) => {
      const track = upcoming[index]; entry.root.visible = !!track;
      if (track) {
        const origin = origins[index];
        entry.origin.content = originMark(origin);
        entry.origin.fg = origin === "explicit" ? colors.accentBright : origin === "radio" ? colors.muted : colors.dim;
        entry.title.content = new StyledText([
          { __isChunk: true, text: track.title, fg: RGBA.fromHex(colors.text), bg: RGBA.fromHex(colors.panel) },
          { __isChunk: true, text: track.artists.length ? ` · ${track.artists.join(" · ")}` : "", fg: RGBA.fromHex(colors.muted), bg: RGBA.fromHex(colors.panel) },
        ]); entry.duration.content = formatTime(track.duration);
      } else {
        entry.origin.content = "";
        entry.title.content = "";
        entry.duration.content = "";
      }
    });
    empty.visible = !upcoming.length;
    empty.content = active
      ? waiting
        ? "Related cache exhausted\n\nWaiting for related metadata."
        : stationKnown
          ? "No related tracks queued\n\nNo related songs are cached yet."
          : "Queue is empty\n\nRadio status details are unavailable."
      : "Your next songs appear here.\n\nStart radio to keep listening.";
    const count = `${Math.min(15, upcoming.length)}${upcoming.length > 15 ? "+" : ""} upcoming`;
    const played = typeof status.played_count === "number" ? `${status.played_count} session exclusions` : "Session repeats blocked";
    const catalog = nonNegativeCount(status.catalog_tracks);
    const liked = nonNegativeCount(status.cache_tracks);
    const coverage = catalog !== undefined ? `${formatCount(catalog)} catalog · no auto repeats` : liked !== undefined ? `${formatCount(liked)} liked · no auto repeats` : "Catalog count unavailable";
    const parked = nonNegativeCount(status.parked_count);
    const radioPending = nonNegativeCount(status.radio_pending_count);
    const explicitPending = nonNegativeCount(status.explicit_pending_count);
    const pending = [
      radioPending !== undefined && radioPending > 0 ? `${formatCount(radioPending)} radio` : "",
      explicitPending !== undefined && explicitPending > 0 ? `${formatCount(explicitPending)} queued (+)` : "",
    ].filter(Boolean).join(" · ");
    const queueLine = active
      ? stationKnown
        ? upcoming.length ? pending || "STATION QUEUED" : waiting ? "RELATED CACHE EXHAUSTED" : "NO RELATED TRACKS QUEUED"
        : upcoming.length ? `QUEUE UPCOMING · ${count}` : "QUEUE EMPTY"
      : count;
    const parkedLine = parked !== undefined ? `${formatCount(parked)} parked · stop resumes` : active ? "Context status unavailable" : played;
    nextTitle.content = stationKnown ? "UP NEXT · STATION" : active ? "UP NEXT · QUEUE" : "UP NEXT";
    nextFoot.content = `${queueLine}\n${parkedLine}\n${coverage}`;
    void loadArtwork(seed);
    if (diagnostics && !details) { debug.setLines(lines); debug.setRows(candidates, selectedId, showDetail); selectedId = undefined; }
  }
  function paint() {
    for (const item of texts) item.node.fg = colors[item.tone];
    for (const panel of [card, nextCard]) { panel.backgroundColor = colors.panel; panel.borderColor = colors.border; }
    for (const item of buttons) { item.box.backgroundColor = item.primary ? colors.cover : colors.panel; item.box.borderColor = item.primary ? colors.coverBorder : colors.borderSoft; }
    paintCover();
  }
  controller = new RadioController(ctx.api, {
    lines(value) { lines = value; }, rows(value) { candidates = value; render(); },
    message(value) { ctx.notify(value); debug.setMessage(value); }, changed() { render(); },
  }, listener => ctx.onStatus(listener));
  const unsubscribe = ctx.onStatus(value => { live = value; render(); });
  const onResize = () => { paint(); render(); }; ctx.renderer.on("resize", onResize);
  paint(); render(); void controller.refresh();
  return {
    root, title: "Radio Studio", editing: () => diagnostics && debug.editing(),
    handleKey(key) {
      if (disposed) return false;
      if (diagnostics && debug.editing()) return debug.handleKey(key);
      if (key.ctrl || key.meta) return false;
      const name = (key.name ?? key.sequence ?? "").toLowerCase();
      if (name === "escape" && diagnostics) {
        if (details) { details = false; debug.setLines(lines); debug.setRows(candidates, selectedId, showDetail); }
        else closeDebug(); return true;
      }
      if (name === "enter" || name === "return") { if (diagnostics) { const selected = debug.selected(); if (selected) showDetail(selected); } return diagnostics; }
      if (name === "s") { toggleRadio(); return true; }
      if (name === "left" || name === "[") { void controller?.adjust(-5); return true; }
      if (name === "right" || name === "]") { void controller?.adjust(5); return true; }
      if (name === "f" || name === "m" || name === "e") { void controller?.setDiscovery(name === "f" ? 0 : name === "m" ? 50 : 100); return true; }
      if (name === "d") { openDiagnostics(); return true; }
      if (name === "r") { void controller?.refresh(); return true; }
      return diagnostics ? debug.handleKey(key) : false;
    },
    refresh: () => controller!.refresh(),
    setTheme(theme) { colors = paletteForTheme(theme); debug.setTheme(theme); paint(); render(); },
    dispose() { if (disposed) return; disposed = true; ++artworkGeneration; unsubscribe(); controller?.dispose(); ctx.renderer.off("resize", onResize); debug.dispose(); root.destroyRecursively(); },
  };
};
