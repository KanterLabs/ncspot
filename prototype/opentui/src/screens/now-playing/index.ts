import { BoxRenderable, ImageRenderable, TextRenderable, MouseButton, RGBA, StyledText, TextAttributes, type TextChunk } from "@opentui/core";
import type { ScreenFactory, Params } from "../../workspace/contracts.js";
import { formatTime, initials, playableTitle, playableArtists, playableAlbum, positionAt } from "../../status.js";
import { paletteForTheme, type ThemePalette } from "../../theme.js";
import { artworkTint, blendHex, blendPixels } from "./motion-colors.js";
import { MOTION, PlayerMotion, ease, playingBars, queueKeys } from "./motion.js";
import { createQuickSearch } from "./quick-search.js";
import { glassArtwork, letterboxNativeArtwork, supportsImageArtwork, type HighResolutionArtwork } from "./glass-artwork.js";

export function ambientProgress(position: number, duration: number, width = 36): string {
  const fraction = duration > 0 ? Math.min(1, Math.max(0, position / duration)) : 0;
  return Array.from({ length: width }, (_, i) => i < Math.floor(width * fraction) ? "━" : "─").join("");
}

/** Only measured audio animates the spectrum; silence stays flat. */
export function audioSpectrum(bands: readonly number[], width = 36): string {
  const chars = "▁▂▃▄▅▆▇█";
  if (!bands.length) return "─".repeat(width);
  return Array.from({ length: width }, (_, index) => {
    const value = Math.min(1, Math.max(0, bands[Math.floor(index * bands.length / width)] ?? 0));
    return value <= 0 ? "─" : chars[Math.min(7, Math.floor(value * 8))];
  }).join("");
}

export interface Artwork { available: boolean; uri: string; width: number; height: number; pixels?: string[]; image?: HighResolutionArtwork }

/** Each cell combines two image pixels without depending on terminal image protocols. */
export function artworkLines(value: Artwork): StyledText[] | null {
  const { width, height, pixels } = value;
  if (!value.available || !Number.isInteger(width) || !Number.isInteger(height) || width < 1 || width > 40 || height < 1 || height > 20 || !pixels || pixels.length !== width * height * 2 || pixels.some(pixel => !/^#[a-f\d]{6}$/i.test(pixel))) return null;
  return Array.from({ length: height }, (_, y) => new StyledText(Array.from({ length: width }, (_, x): TextChunk => ({
    __isChunk: true, text: "▀", fg: RGBA.fromHex(pixels[y * 2 * width + x]!), bg: RGBA.fromHex(pixels[(y * 2 + 1) * width + x]!),
  }))));
}

export const createNowPlayingScreen: ScreenFactory = (ctx) => {
  let status = ctx.status();
  let receivedAt = Date.now();
  let positionBase = status ? positionAt(status, receivedAt) : 0;
  let disposed = false;
  let timer: ReturnType<typeof setInterval> | undefined;
  let timerCadence = 0;
  const noBorders: [] = [];
  const motion = new PlayerMotion();
  let repeat = "unknown";
  let shuffle: boolean | undefined;
  const saved = new Set<string>();
  let pending = false;
  let theme = ctx.theme();
  let colors = paletteForTheme(theme);
  let displayedBands: number[] = [];
  let artworkKey = "";
  let artworkGeneration = 0;
  let coverLines: StyledText[] | null = null;
  let coverValue: Artwork | null = null;
  let coverFrom: string[] | null = null;
  let coverAt = -Infinity;
  let paintedPixels: readonly string[] | null = null;
  let tintFrom: string | null = null;
  let tintTo: string | null = null;
  let tintAt = -Infinity;
  let artWidth = 20;
  let artHeight = 10;
  let nativeUri: string | undefined;
  let nativeArtworkKey = "";
  const texts: Array<{ node: TextRenderable; tone: keyof ThemePalette; background: keyof ThemePalette }> = [];
  const root = new BoxRenderable(ctx.renderer, { id: "np-root", width: "100%", height: "100%", minHeight: 0, flexDirection: "column", justifyContent: "center", alignItems: "center" });
  const quickSearch = createQuickSearch(ctx, root);
  const columns = new BoxRenderable(ctx.renderer, { id: "np-columns", width: "100%", maxWidth: 132, flexDirection: "row", gap: 3, justifyContent: "center", minWidth: 0 });
  root.add(columns);
  const card = new BoxRenderable(ctx.renderer, { id: "np-player-card", height: 24, width: 82, flexShrink: 0, border: true, borderStyle: "rounded", paddingX: 2, paddingY: 1, flexDirection: "column" });
  const queueCard = new BoxRenderable(ctx.renderer, { id: "np-up-next-card", height: 24, width: 42, flexGrow: 1, minWidth: 0, border: true, borderStyle: "rounded", paddingX: 2, paddingY: 1, flexDirection: "column" });
  columns.add(card); columns.add(queueCard);
  function text(parent: BoxRenderable, id: string, content = "", options: ConstructorParameters<typeof TextRenderable>[1] = {}, tone: keyof ThemePalette = "text", background: keyof ThemePalette = "panel") {
    const node = new TextRenderable(ctx.renderer, { id: `np-${id}`, content, height: 1, flexShrink: 0, truncate: true, selectable: false, fg: colors[tone], bg: colors[background], ...options });
    texts.push({ node, tone, background }); parent.add(node); return node;
  }
  function row(parent: BoxRenderable, id: string, height = 1, options: ConstructorParameters<typeof BoxRenderable>[1] = {}) {
    const node = new BoxRenderable(ctx.renderer, { id, width: "100%", height, flexShrink: 0, flexDirection: "row", alignItems: "center", ...options }); parent.add(node); return node;
  }
  function button(parent: BoxRenderable, id: string, label: string, width: number, action: () => void, accent = false) {
    return text(parent, id, label, { width, textAlign: "center", height: 1, onMouseDown: event => { if (event.button === MouseButton.LEFT) { action(); event.preventDefault(); } } }, accent ? "accentBright" : "muted");
  }
  const heading = row(card, "np-heading", 2, { justifyContent: "space-between" });
  text(heading, "heading-label", "NOW PLAYING", { width: 14 }, "dim");
  const mode = text(heading, "mode", "", { width: 24, textAlign: "right" }, "teal");
  const hero = row(card, "np-hero", 10, { gap: 3 });
  const coverBox = new BoxRenderable(ctx.renderer, { id: "np-cover-box", width: 22, height: 10, border: ["left", "right"], flexShrink: 0, flexDirection: "column", justifyContent: "center", alignItems: "center" });
  hero.add(coverBox);
  const coverFallback = text(coverBox, "cover", "", { width: "100%", textAlign: "center", attributes: TextAttributes.BOLD }, "accent", "cover");
  const artNodes: TextRenderable[] = [];
  const coverImage = new ImageRenderable(ctx.renderer, { id: "np-cover-image", visible: false, width: artWidth, height: artHeight, flexShrink: 0, fit: "fit", protocol: "auto", onLoad: () => { if (!disposed) paintCover(); }, onError: () => { if (!disposed) { nativeUri = undefined; paintCover(); } } });
  coverBox.add(coverImage);
  const metadata = new BoxRenderable(ctx.renderer, { id: "np-metadata", flexGrow: 1, minWidth: 0, height: "100%", flexDirection: "column", justifyContent: "center", gap: 1 }); hero.add(metadata);
  const title = text(metadata, "title", "", { width: "100%", height: 2, wrapMode: "word", attributes: TextAttributes.BOLD });
  const artist = text(metadata, "artist", "", { width: "100%" }, "muted");
  const album = text(metadata, "album", "", { width: "100%" }, "dim");
  const ambient = text(metadata, "ambient", "", { width: "100%", height: 2 }, "accent");
  const timeline = text(card, "timeline", "", { width: "100%", onMouseDown: event => {
    if (event.button !== MouseButton.LEFT || !status?.playable) return;
    const value = Math.round(Math.max(0, Math.min(1, (event.x - timeline.x) / Math.max(1, timeline.width - 1))) * status.playable.duration);
    void player("seek", `Seeked to ${formatTime(value)}`, value); event.preventDefault();
  } }, "accent");
  const timeRow = row(card, "np-times", 1, { justifyContent: "space-between" });
  const elapsed = text(timeRow, "elapsed", "", { width: 8 }, "muted");
  const remaining = text(timeRow, "remaining", "", { width: 16, textAlign: "right" }, "dim");
  const transport = row(card, "np-transport", 3, { justifyContent: "center", gap: 2 });
  button(transport, "previous", "│◀ Previous", 13, () => void player("previous", "Previous requested"));
  const playChip = new BoxRenderable(ctx.renderer, { id: "np-play-chip", width: 15, height: 3, flexShrink: 0, border: true, borderStyle: "rounded", justifyContent: "center", alignItems: "center", onMouseDown: event => { if (event.button === MouseButton.LEFT) { void player("play_pause", "Playback toggled"); event.preventDefault(); } } });
  transport.add(playChip);
  const playButton = text(playChip, "play", "▶ Play", { width: 13, textAlign: "center" }, "accentBright", "cover");
  playButton.attributes = TextAttributes.BOLD;
  button(transport, "next", "Next ▶│", 11, () => void player("next", "Next requested"));
  button(transport, "stop", "■ Stop", 8, () => void player("stop", "Playback stopped"));
  const options = row(card, "np-options", 1, { justifyContent: "center", gap: 1 });
  const repeatButton = button(options, "repeat", "Repeat", 15, () => void cycleRepeat());
  const shuffleButton = button(options, "shuffle", "Shuffle", 15, () => void toggleShuffle());
  const savedButton = button(options, "save", "♡ Save", 11, () => void toggleSaved());
  const radioButton = button(options, "radio", "✧ Radio  ⇧R", 16, () => void startRadio(), true);
  const utility = row(card, "np-volume", 1, { justifyContent: "space-between" });
  const seekControls = row(utility, "np-seek-controls", 1, { width: 18 });
  button(seekControls, "seek-back", "[ −5s", 8, () => void seek(-5000));
  button(seekControls, "seek-forward", "] +5s", 8, () => void seek(5000));
  button(utility, "search", "/ Search", 10, () => quickSearch.open(), true);
  const volumeControls = row(utility, "np-volume-controls", 1, { width: 31, justifyContent: "center" });
  button(volumeControls, "quieter", "−", 3, () => void changeVolume(-5));
  const detail = text(volumeControls, "detail", "", { width: 21, textAlign: "center" }, "muted");
  button(volumeControls, "louder", "+", 3, () => void changeVolume(5));
  const navigation = row(card, "np-navigation", 1, { justifyContent: "center", gap: 1 });
  button(navigation, "queue", "Queue →", 10, () => ctx.navigate("queue"));
  button(navigation, "radio-page", "Radio studio →", 17, () => ctx.navigate("radio"));
  button(navigation, "share", "Y Share", 10, () => void shareCurrent());
  button(navigation, "motion", "Motion", 10, () => ctx.setReducedMotion(!ctx.reducedMotion()));
  const queueHeading = row(queueCard, "np-queue-heading", 2, { justifyContent: "space-between" });
  text(queueHeading, "queue-label", "UP NEXT", { width: 12 }, "dim");
  button(queueHeading, "open-queue", "Queue →", 9, () => ctx.navigate("queue"));
  const queueBody = new BoxRenderable(ctx.renderer, { id: "np-queue-body", flexGrow: 1, minHeight: 0, flexDirection: "column", gap: 0, overflow: "hidden" }); queueCard.add(queueBody);
  const queueItems = Array.from({ length: 15 }, (_, index) => {
    const item = new BoxRenderable(ctx.renderer, { id: `np-up-next-${index}`, height: 1, width: "100%", flexShrink: 0, flexDirection: "row", gap: 1, onMouseDown: event => { if (event.button === MouseButton.LEFT) ctx.navigate("queue"); } });
    queueBody.add(item);
    const number = text(item, `queue-number-${index}`, String(index + 1).padStart(2, "0"), { width: 2 }, "dim");
    return { root: item, number, title: text(item, `queue-title-${index}`, "", { flexGrow: 1, flexShrink: 1, flexBasis: 0, minWidth: 0 }), duration: text(item, `queue-duration-${index}`, "", { width: 5, textAlign: "right" }, "muted") };
  });
  const leavingRow = new BoxRenderable(ctx.renderer, { id: "np-queue-leaving", position: "absolute", top: 0, left: 0, height: 1, width: "100%", visible: false, flexDirection: "row", gap: 1 }); queueBody.add(leavingRow);
  const leavingNumber = text(leavingRow, "leaving-number", "01", { width: 2 }, "accent");
  const leavingTitle = text(leavingRow, "leaving-title", "", { flexGrow: 1, flexShrink: 1, flexBasis: 0, minWidth: 0 }, "accentBright");
  const leavingDuration = text(leavingRow, "leaving-duration", "", { width: 5, textAlign: "right" }, "muted");
  const queueEmpty = text(queueBody, "queue-empty", "Nothing queued yet", { width: "100%", height: 4 }, "muted");
  const emptyRadio = new BoxRenderable(ctx.renderer, { id: "np-empty-radio", width: "100%", height: 3, border: true, borderStyle: "rounded", alignItems: "center", justifyContent: "center", onMouseDown: event => { if (event.button === MouseButton.LEFT) { void startRadio(); event.preventDefault(); } } });
  queueBody.add(emptyRadio);
  text(emptyRadio, "empty-radio-label", "✧ Start radio · Shift+R", { width: "100%", textAlign: "center" }, "accentBright", "cover");
  const queueFoot = text(queueCard, "queue-foot", "", { width: "100%", height: 2 }, "dim");

  function currentPosition() { return Math.min(status?.playable?.duration ?? 0, positionBase + (status?.mode.kind === "playing" ? Math.max(0, Date.now() - receivedAt) : 0)); }
  function uri() { const p = status?.playable; return p?.uri ?? (p?.id ? `spotify:${p.type === "Episode" ? "episode" : "track"}:${p.id}` : undefined); }
  function notice(message: string) { if (!disposed) ctx.notify(message); }
  async function mutate(method: string, params: Params, message: string, acknowledged?: () => void, feedback?: string) {
    if (pending || disposed) return;
    pending = true;
    if (feedback && !ctx.reducedMotion()) { motion.pulse(feedback, Date.now()); render(); syncTimer(); }
    try { await ctx.api.call(method, params); if (!disposed) { acknowledged?.(); notice(message); render(); } }
    catch (error) { notice(`Unable to apply: ${error instanceof Error ? error.message : String(error)}`); }
    finally { pending = false; }
  }
  function player(action: string, message: string, value?: unknown, acknowledged?: () => void) { return mutate("player.action", { action, ...(value === undefined ? {} : { value }) }, message, acknowledged, action === "play_pause" ? "play" : undefined); }
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
  function cycleRepeat() { const value = repeat === "off" || repeat === "unknown" ? "all" : repeat === "all" ? "track" : "off"; return player("repeat", `Repeat ${value}`, value, () => { repeat = value; }); }
  function toggleShuffle() { const value = !shuffle; return player("shuffle", `Shuffle ${value ? "on" : "off"}`, value, () => { shuffle = value; }); }
  function toggleSaved() {
    const identity = uri(); if (!identity) { notice("Choose an item before saving"); return; }
    const value = !saved.has(identity);
    return mutate("library.action", { action: value ? "save" : "unsave", kind: status?.playable?.type === "Episode" ? "episode" : "track", uri: identity }, value ? "Saved to your library" : "Removed from your library", () => { value ? saved.add(identity) : saved.delete(identity); }, "save");
  }
  function startRadio() {
    const identity = uri(); if (!identity || status?.playable?.type === "Episode") { notice("Choose a track to start radio"); return; }
    return mutate("radio.action", { action: "start", uri: identity }, "Radio started from this track", undefined, "radio");
  }
  async function shareCurrent() {
    const identity = uri(); if (!identity) { notice("Choose an item before sharing"); return; }
    if (disposed || pending) return;
    pending = true;
    try { const result = await ctx.api.call<{ url: string }>("share", { uri: identity }); if (typeof result.url !== "string" || !result.url) throw new Error("Share link unavailable"); if (!disposed) notice(result.url); }
    catch (error) { notice(`Unable to share: ${error instanceof Error ? error.message : String(error)}`); }
    finally { pending = false; }
  }
  function coverPixels(now: number): string[] | null {
    const target = coverValue?.pixels;
    if (target) {
      const progress = ctx.reducedMotion() ? 1 : ease((now - coverAt) / MOTION.cover);
      return progress >= 1 ? target : blendPixels(coverFrom, target, progress, colors.cover);
    }
    // Fade an outgoing cover while the cache responds. No extra artwork RPCs.
    if (!ctx.reducedMotion() && coverFrom?.length === artWidth * artHeight * 2 && now - coverAt < 180) {
      return blendPixels(null, coverFrom, 1 - ease((now - coverAt) / 180), colors.cover);
    }
    return null;
  }
  function currentTint(now: number): string {
    return blendHex(tintFrom ?? colors.accent, tintTo ?? colors.accent, ctx.reducedMotion() ? 1 : ease((now - tintAt) / MOTION.tint));
  }
  function paintCover(now = Date.now(), force = false) {
    const native = supportsImageArtwork(ctx.renderer) && nativeUri === uri() && !!coverImage.image;
    coverImage.visible = native;
    coverImage.width = artWidth; coverImage.height = artHeight;
    coverImage.opacity = ctx.reducedMotion() ? 1 : ease((now - coverAt) / MOTION.cover);
    coverBox.border = native ? noBorders : ["left", "right"];
    const pixels = coverPixels(now);
    if (force || pixels !== paintedPixels) {
      paintedPixels = pixels;
      coverLines = pixels ? artworkLines({ available: true, uri: uri() ?? "", width: artWidth, height: artHeight, pixels }) : null;
      while (artNodes.length > (coverLines?.length ?? 0)) { const node = artNodes.pop()!; coverBox.remove(node); node.destroy(); }
      coverLines?.forEach((line, index) => {
        let node = artNodes[index];
        if (!node) { node = new TextRenderable(ctx.renderer, { id: `np-art-${index}`, content: line, width: artWidth, height: 1, flexShrink: 0, selectable: false }); artNodes.push(node); coverBox.add(node); }
        else { node.width = artWidth; node.content = line; }
      });
    }
    for (const node of artNodes) node.visible = !native;
    coverFallback.visible = !native && !coverLines;
    const tint = currentTint(now);
    coverBox.backgroundColor = blendHex(colors.cover, tint, 0.08);
    coverBox.borderColor = blendHex(colors.coverBorder, tint, 0.48);
    card.borderColor = blendHex(colors.border, tint, 0.18);
  }
  function updateNativeArtwork() {
    const resolution = ctx.renderer.resolution;
    const key = `${coverValue?.uri ?? ""}:${theme}:${artWidth}x${artHeight}:${ctx.renderer.width}x${ctx.renderer.height}:${resolution?.width ?? 0}x${resolution?.height ?? 0}:${supportsImageArtwork(ctx.renderer)}`;
    if (key === nativeArtworkKey) return;
    nativeArtworkKey = key;
    const decoded = coverValue?.image && supportsImageArtwork(ctx.renderer) ? glassArtwork(coverValue.image, theme) : null;
    const background = paletteForTheme(theme).cover;
    const rgb = (hex: string): [number, number, number, number] => [1, 3, 5].map(offset => parseInt(hex.slice(offset, offset + 2), 16)).concat(255) as [number, number, number, number];
    const inset = decoded ? letterboxNativeArtwork(decoded, ctx.renderer, artWidth, artHeight, rgb(background)) : null;
    const source = inset ?? decoded;
    nativeUri = source ? coverValue?.uri : undefined;
    coverImage.source = source ?? undefined;
    inset?.dispose();
    decoded?.dispose(); // ImageRenderable synchronously retains its own reference.
  }
  async function loadArtwork(force = false) {
    const identity = uri(); const highResolution = supportsImageArtwork(ctx.renderer);
    const key = `${identity ?? ""}:${artWidth}:${artHeight}:${highResolution}`;
    if (!force && key === artworkKey) return;
    const now = Date.now();
    coverFrom = coverPixels(now); coverValue = null; coverAt = now;
    nativeUri = undefined; nativeArtworkKey = ""; coverImage.source = undefined;
    tintFrom = currentTint(now); tintTo = null; tintAt = now;
    artworkKey = key; const generation = ++artworkGeneration; paintCover(now); syncTimer();
    if (!identity) return;
    try {
      const value = await ctx.api.call<Artwork>("player.artwork", { uri: identity, width: artWidth, height: artHeight, ...(highResolution ? { format: "png" } : {}) });
      if (disposed || generation !== artworkGeneration || identity !== uri()) return;
      if (value.uri === identity && value.width === artWidth && value.height === artHeight && artworkLines(value)) {
        const loadedAt = Date.now();
        coverFrom = coverPixels(loadedAt); coverValue = value; coverAt = loadedAt;
        tintFrom = currentTint(loadedAt); tintTo = artworkTint(value.pixels!); tintAt = loadedAt;
        updateNativeArtwork();
      }
      paintCover(); syncTimer();
    } catch { /* A missing cover must never interrupt playback or navigation. */ }
  }
  function render() {
    if (disposed) return;
    const now = Date.now(); const reduced = ctx.reducedMotion();
    if (reduced) motion.settle();
    const p = status?.playable ?? null;
    const position = currentPosition(); const duration = p?.duration ?? 0;
    const compact = ctx.renderer.height < 30;
    const sideBySide = ctx.renderer.width >= 112 && !compact;
    const available = Math.min(132, ctx.renderer.width - 4);
    const playerWidth = sideBySide ? Math.max(68, available - 47) : Math.min(82, available);
    card.width = playerWidth;
    if (sideBySide) queueCard.width = available - playerWidth - 3;
    card.height = compact ? Math.max(15, ctx.renderer.height - 7) : 24;
    card.paddingY = compact ? 0 : 1;
    queueCard.visible = sideBySide; queueCard.height = card.height; columns.height = card.height;
    heading.height = compact ? 1 : 2; hero.height = compact ? 6 : 10; metadata.gap = compact ? 0 : 1;
    title.height = compact ? 1 : 2; album.visible = !compact; ambient.height = compact ? 1 : 2;
    transport.height = compact ? 1 : 3; playChip.height = compact ? 1 : 3;
    // An empty side list stays borderless when OpenTUI's color setter runs;
    // `false` would be promoted back to a full border by that setter.
    playChip.border = compact ? noBorders : true; navigation.visible = !compact;
    const wantedHeight = compact ? 6 : 10; const wantedWidth = wantedHeight * 2;
    if (artWidth !== wantedWidth || artHeight !== wantedHeight) { artWidth = wantedWidth; artHeight = wantedHeight; coverBox.width = artWidth + 2; coverBox.height = artHeight; }
    coverFallback.content = initials(p); artist.content = playableArtists(p); album.content = playableAlbum(p);
    artist.fg = blendHex(colors.panel, colors.muted, reduced ? 1 : motion.trackProgress(now, 110));
    album.fg = blendHex(colors.panel, colors.dim, reduced ? 1 : motion.trackProgress(now, 150));
    mode.content = `${status?.mode.kind === "playing" ? "●" : "○"} ${status?.mode.kind.toUpperCase() ?? "READY"}${status?.prototype?.radio_active ? "  ·  RADIO" : ""}`;
    const spectrumWidth = Math.max(12, playerWidth - artWidth - 11); const audio = status?.prototype?.audio;
    if (audio) {
      const active = status?.mode.kind === "playing" && audio.level > 0; const target = active ? audio.bands : audio.bands.map(() => 0);
      if (ctx.reducedMotion() || !active || displayedBands.length !== target.length) displayedBands = [...target];
      ambient.content = compact ? audioSpectrum(displayedBands, spectrumWidth) : `${active ? "Audio spectrum" : "Audio spectrum · silence"}\n${audioSpectrum(displayedBands, spectrumWidth)}`;
    } else ambient.content = compact ? "" : `${ctx.reducedMotion() ? "Reduced motion" : "Playback progress"}\n${ambientProgress(position, duration, spectrumWidth)}`;
    title.content = new StyledText([
      { __isChunk: true, text: `${playingBars(status?.mode.kind === "playing", audio ? displayedBands : undefined, audio?.level, now, reduced)}  `, fg: RGBA.fromHex(colors.teal), bg: RGBA.fromHex(colors.panel) },
      { __isChunk: true, text: playableTitle(p), fg: RGBA.fromHex(blendHex(colors.panel, colors.text, reduced ? 1 : motion.trackProgress(now, 40))), bg: RGBA.fromHex(colors.panel) },
    ]);
    const trackWidth = Math.max(12, playerWidth - 6);
    const filled = duration > 0 ? Math.min(trackWidth, Math.floor(trackWidth * position / duration)) : 0;
    timeline.content = new StyledText([
      { __isChunk: true, text: "━".repeat(filled), fg: RGBA.fromHex(colors.accent), bg: RGBA.fromHex(colors.panel) },
      { __isChunk: true, text: "─".repeat(trackWidth - filled), fg: RGBA.fromHex(colors.progressTrack), bg: RGBA.fromHex(colors.panel) },
    ]);
    elapsed.content = formatTime(position); remaining.content = `−${formatTime(Math.max(0, duration - position))} / ${formatTime(duration)}`;
    playButton.content = status?.mode.kind === "playing" ? "Ⅱ Pause" : "▶ Play";
    repeatButton.content = `Repeat: ${repeat}`; shuffleButton.content = `Shuffle: ${shuffle === undefined ? "—" : shuffle ? "on" : "off"}`;
    savedButton.content = saved.has(uri() ?? "") ? "♥ Saved" : "♡ Save";
    const playPulse = reduced ? 0 : motion.button("play", now);
    const playBackground = blendHex(colors.cover, colors.queueCurrent, playPulse * 0.7);
    playChip.backgroundColor = playButton.bg = playBackground;
    playChip.borderColor = blendHex(colors.coverBorder, colors.accent, playPulse * 0.8);
    for (const [key, node] of [["save", savedButton], ["radio", radioButton]] as const) {
      const pulse = reduced ? 0 : motion.button(key, now);
      node.bg = blendHex(colors.panel, colors.cover, pulse * 0.8);
      node.fg = blendHex(key === "radio" ? colors.accentBright : colors.muted, colors.accentBright, pulse);
    }
    const radioPulse = reduced ? 0 : motion.button("radio", now);
    emptyRadio.borderColor = blendHex(colors.coverBorder, colors.accent, radioPulse);
    detail.content = `Volume ${status?.prototype ? `${status.prototype.volume_percent}%` : "—"}`;
    const upNext = status?.prototype?.up_next ?? [];
    const keys = queueKeys(upNext);
    queueItems.forEach((item, index) => {
      const track = upNext[index]; item.root.visible = !!track;
      // Native terminal positions are integers; round explicitly so the first
      // frames retain the previous row position before moving up one cell.
      item.root.translateY = reduced ? 0 : Math.round(motion.queueOffset(now));
      item.root.opacity = reduced ? 1 : 1 - motion.queueOffset(now) * 0.25;
      if (track) {
        const highlight = reduced ? 0 : motion.refill(keys[index]!, now);
        const background = blendHex(colors.panel, colors.cover, highlight * 0.8);
        item.root.backgroundColor = background;
        item.number.bg = item.duration.bg = background;
        item.number.fg = blendHex(colors.dim, colors.accent, highlight);
        item.title.content = new StyledText([
          { __isChunk: true, text: track.title, fg: RGBA.fromHex(colors.text), bg: RGBA.fromHex(background) },
          { __isChunk: true, text: track.artists.length ? ` · ${track.artists.join(" · ")}` : "", fg: RGBA.fromHex(colors.muted), bg: RGBA.fromHex(background) },
        ]);
        item.duration.content = formatTime(track.duration);
      }
    });
    const offset = reduced ? 0 : motion.queueOffset(now);
    leavingRow.visible = !!motion.departing && offset > 0;
    if (motion.departing && leavingRow.visible) {
      const background = blendHex(colors.panel, colors.cover, offset * 0.75);
      leavingRow.backgroundColor = background; leavingRow.opacity = offset;
      for (const node of [leavingNumber, leavingTitle, leavingDuration]) node.bg = background;
      leavingTitle.content = motion.departing.title; leavingDuration.content = formatTime(motion.departing.duration);
    }
    queueEmpty.visible = !upNext.length;
    queueEmpty.content = status?.prototype?.radio_active ? "Finding fresh tracks…\n\nYour radio station is active." : "Nothing queued yet\n\nStart radio from this song\nto keep the music going.";
    emptyRadio.visible = !upNext.length && !!p && p.type !== "Episode" && !status?.prototype?.radio_active;
    const upcoming = `${Math.min(15, upNext.length)}${upNext.length > 15 ? "+" : ""} upcoming · 2 open queue`;
    queueFoot.content = status?.prototype?.radio_active ? `✧ Continuous radio\n${upNext.length ? upcoming : "Finding fresh tracks…"}` : upNext.length ? upcoming : "✧ Shift+R starts radio";
    void loadArtwork();
    // Geometry can arrive after the image response (notably Sixel), and a
    // resize can change the physical cell ratio without changing art cells.
    updateNativeArtwork();
    paintCover(now);
  }
  function syncTimer() {
    const now = Date.now(); const reduced = ctx.reducedMotion();
    const finite = !reduced && (motion.active(now) || now - coverAt < MOTION.cover || now - tintAt < MOTION.tint);
    const playing = status?.mode.kind === "playing";
    // Short transitions get 25 frames/sec. Once settled, measured audio uses
    // 12.5 frames/sec and the playback-only icon needs just 6.25 frames/sec.
    const cadence = disposed ? 0 : reduced ? playing ? 1000 : 0 : finite ? 40 : playing ? status?.prototype?.audio ? 80 : 160 : 0;
    if (timer && cadence === timerCadence) return;
    if (timer) { clearInterval(timer); timer = undefined; }
    timerCadence = cadence;
    if (cadence) timer = setInterval(() => {
      if (disposed || !root.visible || !root.parent) { if (timer) clearInterval(timer); timer = undefined; return; }
      const audio = status?.prototype?.audio;
      if (audio) displayedBands = audio.bands.map((band, index) => { const value = audio.level > 0 ? band : 0; const previous = displayedBands[index] ?? 0; return value <= 0 ? 0 : previous + (value - previous) * 0.35; });
      render();
      syncTimer();
    }, cadence);
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
    } catch (error) { if (identity) notice(`Playback details unavailable: ${error instanceof Error ? error.message : String(error)}`); }
  }
  function applyTheme() {
    colors = paletteForTheme(theme); for (const item of texts) { item.node.fg = colors[item.tone]; item.node.bg = colors[item.background]; }
    for (const panel of [card, queueCard]) { panel.backgroundColor = colors.panel; panel.borderColor = colors.border; }
    playChip.backgroundColor = colors.cover; playChip.borderColor = colors.coverBorder;
    emptyRadio.backgroundColor = colors.cover; emptyRadio.borderColor = colors.coverBorder;
    coverBox.backgroundColor = colors.cover;
  }
  const unsubscribe = ctx.onStatus(value => { const previous = uri(); const now = Date.now(); motion.observe(status, value, now, ctx.reducedMotion()); status = value; receivedAt = now; positionBase = positionAt(value, receivedAt); render(); syncTimer(); if (uri() !== previous) void loadMetadata(); });
  const onResize = () => render(); ctx.renderer.on("resize", onResize);
  ctx.renderer.on("capabilities", onResize);
  let graphicsReady = supportsImageArtwork(ctx.renderer);
  const onFrame = () => {
    const ready = supportsImageArtwork(ctx.renderer);
    if (ready !== graphicsReady) { graphicsReady = ready; render(); }
    else if (ready) updateNativeArtwork();
  };
  // Pixel geometry can arrive asynchronously without a resize event. The
  // native source key makes this cheap once geometry has settled.
  ctx.renderer.on("frame", onFrame);
  applyTheme(); render(); syncTimer(); void loadMetadata();
  return {
    root, title: "Now Playing", editing: quickSearch.isOpen,
    handleKey(key) {
      if (quickSearch.isOpen()) return quickSearch.handleKey(key);
      const name = key.name ?? key.sequence ?? ""; if (key.ctrl || key.meta) return false;
      if (name === "/" || name === "slash" || key.sequence === "/") { quickSearch.open(); return true; }
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
    async refresh() { render(); syncTimer(); await Promise.all([loadMetadata(), loadArtwork(true)]); },
    setTheme(value) { theme = value; applyTheme(); updateNativeArtwork(); quickSearch.setTheme(value); render(); },
    dispose() { if (disposed) return; disposed = true; ++artworkGeneration; if (timer) clearInterval(timer); unsubscribe(); ctx.renderer.off("resize", onResize); ctx.renderer.off("capabilities", onResize); ctx.renderer.off("frame", onFrame); quickSearch.dispose(); root.destroyRecursively(); },
  };
};
