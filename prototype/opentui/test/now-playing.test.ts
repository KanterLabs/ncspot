import { expect, test } from "bun:test";
import { createTestRenderer } from "@opentui/core/testing";
import { createNowPlayingScreen, ambientProgress, artworkLines, audioSpectrum, type Artwork } from "../src/screens/now-playing/index.js";
import { mountWorkspace } from "../src/workspace/app.js";
import { demoStatus, type ParsedStatus } from "../src/status.js";
import type { ScreenContext, Params } from "../src/workspace/contracts.js";

type ArtworkResolver = (params: Params) => Artwork | Promise<Artwork>;
type RpcResponder = (method: string, params?: Params) => unknown | Promise<unknown>;

interface FixtureOptions {
  width?: number;
  height?: number;
  status?: ParsedStatus;
  reducedMotion?: boolean;
  artwork?: ArtworkResolver;
  respond?: RpcResponder;
}

interface Deferred<T> {
  promise: Promise<T>;
  resolve(value: T): void;
}

function deferred<T>(): Deferred<T> {
  let resolve!: (value: T) => void;
  return { promise: new Promise<T>(done => { resolve = done; }), resolve };
}

function trackStatus(index = 0, positionMs = 82_000): ParsedStatus {
  const status = demoStatus(index, positionMs);
  const playable = status.playable;
  if (playable) status.playable = { ...playable, uri: `spotify:track:demo${index + 1}` };
  return status;
}

function unavailableArtwork(params: Params = {}): Artwork {
  return {
    available: false,
    uri: typeof params.uri === "string" ? params.uri : "",
    width: typeof params.width === "number" ? params.width : 20,
    height: typeof params.height === "number" ? params.height : 10,
  };
}

function coloredArtwork(uri: string, width: number, height: number): Artwork {
  const pixels = Array.from({ length: width * height * 2 }, (_, index) => {
    if (index < width) return "#112233";
    if (index < width * 2) return "#445566";
    return index % 2 === 0 ? "#778899" : "#aabbcc";
  });
  return { available: true, uri, width, height, pixels };
}

async function settle(f: { renderOnce(): Promise<void> }) {
  await new Promise(resolve => setTimeout(resolve, 0));
  await f.renderOnce();
}

function nativeRgb(renderer: { currentRenderBuffer: { width: number; buffers: { fg: Uint16Array; bg: Uint16Array } } }, x: number, y: number, channel: "fg" | "bg") {
  const buffer = renderer.currentRenderBuffer;
  const values = buffer.buffers[channel];
  const offset = (y * buffer.width + x) * 4;
  return [values[offset]! & 0xff, values[offset + 1]! & 0xff, values[offset + 2]! & 0xff];
}

function textValue(value: unknown): string {
  if (typeof value === "string") return value;
  if (value && typeof value === "object" && "chunks" in value && Array.isArray((value as { chunks?: unknown }).chunks)) {
    return ((value as { chunks: Array<{ text?: unknown }> }).chunks).map(chunk => String(chunk.text ?? "")).join("");
  }
  return "";
}

async function fixtureCtx(options: FixtureOptions = {}) {
  const setup = await createTestRenderer({ width: options.width ?? 96, height: options.height ?? 34, useMouse: true });
  let status: ParsedStatus = options.status ?? trackStatus();
  let listener: ((s: ParsedStatus) => void) | undefined;
  const calls: { method: string; params?: Params }[] = [];
  const notices: string[] = [];
  let fail = false;
  let reduced = options.reducedMotion ?? true;
  let unsubscribed = false;
  const ctx: ScreenContext = {
    renderer: setup.renderer,
    api: {
      async call<T>(method: string, params?: Params) {
        calls.push({ method, params });
        if (fail && method !== "player.artwork") throw new Error("engine offline");
        if (method === "player.artwork") return await (options.artwork?.(params ?? {}) ?? unavailableArtwork(params)) as T;
        if (options.respond) return await options.respond(method, params) as T;
        if (method === "player.status") return { repeat: "off", shuffle: false, saved: false, current: { uri: "spotify:track:demo1" } } as T;
        if (method === "share") return { url: "https://open.spotify.com/track/demo1" } as T;
        return { applied: true } as T;
      },
    },
    theme: () => "light",
    setTheme() {},
    reducedMotion: () => reduced,
    setReducedMotion(value) { reduced = value; },
    status: () => status,
    onStatus(value) { listener = value; return () => { listener = undefined; unsubscribed = true; }; },
    navigate() {},
    notify(message) { notices.push(message); },
  };
  const screen = createNowPlayingScreen(ctx);
  setup.renderer.root.add(screen.root);
  return {
    ...setup,
    screen,
    calls,
    notices,
    motion(value: boolean) { reduced = value; screen.refresh(); },
    update(value: ParsedStatus) { status = value; listener?.(value); },
    fail() { fail = true; },
    unsubscribed: () => unsubscribed,
    close() { screen.dispose(); setup.renderer.destroy(); },
  };
}

test("Now Playing renders live metadata, artwork fallback, and truthful ambient label", async () => {
  const f = await fixtureCtx();
  try {
    await f.renderOnce();
    let frame = f.captureCharFrame();
    expect(frame).toContain("The Colour of Air");
    expect(frame).toContain("TC");
    expect(frame).toContain("1:22");
    expect(frame).toContain("Reduced motion");
    expect(frame).not.toContain("Cover initials");

    f.update(trackStatus(1, 14_000));
    await f.renderOnce();
    frame = f.captureCharFrame();
    expect(frame).toContain("Soft Geometry");
    expect(frame).toContain("0:14");
    f.screen.setTheme("dark");
    await f.renderOnce();
    expect(f.captureCharFrame()).toContain("Soft Geometry");
  } finally { f.close(); }
  expect(f.unsubscribed()).toBe(true);
});

test("artworkLines validates the RGB contract and pairs two pixels per terminal cell", () => {
  const value = coloredArtwork("spotify:track:demo1", 2, 1);
  const lines = artworkLines(value);
  expect(lines).toHaveLength(1);
  expect(lines?.[0]?.chunks).toHaveLength(2);
  expect(lines?.[0]?.chunks.map(chunk => chunk.text)).toEqual(["▀", "▀"]);
  expect(lines?.[0]?.chunks[0]?.fg?.toInts()).toEqual([17, 34, 51, 255]);
  expect(lines?.[0]?.chunks[0]?.bg?.toInts()).toEqual([68, 85, 102, 255]);
  expect(artworkLines({ ...value, available: false })).toBeNull();
  expect(artworkLines({ ...value, pixels: ["#112233"] })).toBeNull();
  expect(artworkLines({ ...value, pixels: ["#112233", "not-a-pixel", "#445566", "#778899"] })).toBeNull();
});

test("valid artwork renders colored half-block cells and removes the literal cover fallback", async () => {
  const f = await fixtureCtx({ artwork: params => coloredArtwork(String(params.uri), Number(params.width), Number(params.height)) });
  try {
    await f.renderOnce();
    await settle(f);
    const frame = f.captureCharFrame();
    const cover = f.screen.root.findDescendantById("np-cover");
    const art = f.screen.root.findDescendantById("np-art-0");
    expect(cover?.visible).toBe(false);
    expect(art).toBeDefined();
    expect(frame).toContain("▀");
    expect(frame).not.toContain("TC");
    expect(nativeRgb(f.renderer, art!.x, art!.y, "fg")).toEqual([17, 34, 51]);
    expect(nativeRgb(f.renderer, art!.x, art!.y, "bg")).toEqual([68, 85, 102]);
  } finally { f.close(); }
});

test("malformed artwork pixels keep the initials fallback without throwing", async () => {
  const f = await fixtureCtx({ artwork: params => ({
    available: true,
    uri: String(params.uri),
    width: Number(params.width),
    height: Number(params.height),
    pixels: ["#bad"],
  }) });
  try {
    await f.renderOnce();
    await settle(f);
    expect(f.screen.root.findDescendantById("np-cover")?.visible).toBe(true);
    expect(f.screen.root.findDescendantById("np-art-0")).toBeUndefined();
    expect(f.captureCharFrame()).toContain("TC");
  } finally { f.close(); }
});

test("artwork requests use true and compact dimensions", async () => {
  const normal = await fixtureCtx({ width: 96, height: 34, artwork: params => coloredArtwork(String(params.uri), Number(params.width), Number(params.height)) });
  try {
    await normal.renderOnce();
    await settle(normal);
    expect(normal.calls.find(call => call.method === "player.artwork")?.params).toEqual({ uri: "spotify:track:demo1", width: 20, height: 10 });
  } finally { normal.close(); }

  const compact = await fixtureCtx({ width: 80, height: 24, artwork: params => coloredArtwork(String(params.uri), Number(params.width), Number(params.height)) });
  try {
    await compact.renderOnce();
    await settle(compact);
    expect(compact.calls.find(call => call.method === "player.artwork")?.params).toEqual({ uri: "spotify:track:demo1", width: 12, height: 6 });
  } finally { compact.close(); }
});

test("stale artwork responses are ignored after a track change", async () => {
  const oldArtwork = deferred<Artwork>();
  const newArtwork = deferred<Artwork>();
  let artworkCalls = 0;
  const f = await fixtureCtx({ artwork: () => {
    artworkCalls++;
    return artworkCalls === 1 ? oldArtwork.promise : newArtwork.promise;
  } });
  try {
    await f.renderOnce();
    expect(artworkCalls).toBe(1);
    f.update(trackStatus(1));
    expect(artworkCalls).toBe(2);

    oldArtwork.resolve(coloredArtwork("spotify:track:demo1", 20, 10));
    await settle(f);
    expect(f.screen.root.findDescendantById("np-cover")?.visible).toBe(true);
    expect(f.screen.root.findDescendantById("np-art-0")).toBeUndefined();

    newArtwork.resolve(coloredArtwork("spotify:track:demo2", 20, 10));
    await settle(f);
    expect(f.screen.root.findDescendantById("np-cover")?.visible).toBe(false);
    expect(f.screen.root.findDescendantById("np-art-0")).toBeDefined();
  } finally { f.close(); }
});

test("stale artwork responses are ignored after disposal", async () => {
  const pendingArtwork = deferred<Artwork>();
  const f = await fixtureCtx({ artwork: () => pendingArtwork.promise });
  const cover = f.screen.root.findDescendantById("np-cover")!;
  try {
    await f.renderOnce();
    f.screen.dispose();
    pendingArtwork.resolve(coloredArtwork("spotify:track:demo1", 20, 10));
    await Promise.resolve();
    await Promise.resolve();
    expect(cover.visible).toBe(true);
    expect(f.notices).toEqual([]);
  } finally { f.renderer.destroy(); }
});

test("status updates for the same identity do not issue another artwork RPC", async () => {
  const f = await fixtureCtx();
  try {
    await f.renderOnce();
    const artworkCalls = () => f.calls.filter(call => call.method === "player.artwork");
    expect(artworkCalls()).toHaveLength(1);
    f.update(trackStatus(0, 83_000));
    await f.renderOnce();
    expect(artworkCalls()).toHaveLength(1);
    f.update(trackStatus(1, 12_000));
    await f.renderOnce();
    expect(artworkCalls()).toHaveLength(2);
  } finally { f.close(); }
});

test("actions publish one notification through ctx.notify and preserve exact RPC vocabulary", async () => {
  const f = await fixtureCtx();
  try {
    await f.renderOnce();
    expect(f.screen.editing?.()).toBe(false);
    f.screen.handleKey({ name: "space" }); await settle(f);
    f.screen.handleKey({ name: "]" }); await settle(f);
    f.screen.handleKey({ name: "+" }); await settle(f);
    f.screen.handleKey({ name: "r", shift: true }); await settle(f);
    f.screen.handleKey({ name: "f" }); await settle(f);
    expect(f.calls).toContainEqual({ method: "player.action", params: { action: "play_pause" } });
    expect(f.calls.find(call => call.params?.action === "seek")?.params?.value).toBeGreaterThanOrEqual(87_000);
    expect(f.calls).toContainEqual({ method: "player.action", params: { action: "volume", value: 73 } });
    expect(f.calls).toContainEqual({ method: "radio.action", params: { action: "start", uri: "spotify:track:demo1" } });
    expect(f.calls).toContainEqual({ method: "library.action", params: { action: "save", kind: "track", uri: "spotify:track:demo1" } });
    expect(f.notices).toContain("Playback toggled");
    expect(f.notices.filter(notice => notice === "Playback toggled")).toHaveLength(1);

    f.fail();
    f.screen.handleKey({ name: "space" });
    await settle(f);
    expect(f.notices.at(-1)).toBe("Unable to apply: engine offline");
    expect(f.notices.filter(notice => notice === "Unable to apply: engine offline")).toHaveLength(1);
    expect(f.screen.handleKey({ name: "x" })).toBe(false);
  } finally { f.close(); }
});

test("compact controls keep their IDs, show symbol-only volume buttons, and remain inside the card", async () => {
  const f = await fixtureCtx({ width: 80, height: 24 });
  try {
    await f.renderOnce();
    const frame = f.captureCharFrame();
    const card = f.screen.root.findDescendantById("np-player-card")!;
    const volume = f.screen.root.findDescendantById("np-volume-controls")!;
    const detail = f.screen.root.findDescendantById("np-detail")!;
    const quieter = f.screen.root.findDescendantById("np-quieter")!;
    const louder = f.screen.root.findDescendantById("np-louder")!;
    expect(frame).toContain("The Colour of Air");
    expect(frame).toContain("Pause");
    expect(frame).toContain("+5s");
    expect(textValue((quieter as { content?: unknown }).content)).toBe("−");
    expect(textValue((louder as { content?: unknown }).content)).toBe("+");
    expect(frame).not.toContain("Quieter");
    expect(frame).not.toContain("Louder");
    expect(detail.parent?.id).toBe("np-volume-controls");
    expect(volume.x).toBeGreaterThanOrEqual(card.x);
    expect(volume.x + volume.width).toBeLessThanOrEqual(card.x + card.width);
    expect(detail.x + detail.width).toBeLessThanOrEqual(volume.x + volume.width);
    expect(card.x).toBeGreaterThanOrEqual(0);
    expect(card.y).toBeGreaterThanOrEqual(0);
    expect(card.x + card.width).toBeLessThanOrEqual(80);
    expect(card.y + card.height).toBeLessThanOrEqual(24);
    expect(f.screen.root.findDescendantById("np-play")!.y).toBe(f.screen.root.findDescendantById("np-previous")!.y);
  } finally { f.close(); }
  expect(ambientProgress(5, 10, 10)).toBe("━━━━━─────");
});

test("mouse controls use acknowledged actions and failed repeat preserves its state", async () => {
  const f = await fixtureCtx();
  try {
    await f.renderOnce();
    await settle(f);
    const previous = f.screen.root.findDescendantById("np-previous")!;
    await f.mockMouse.click(previous.x + 2, previous.y);
    await settle(f);
    expect(f.calls).toContainEqual({ method: "player.action", params: { action: "previous" } });
    f.screen.handleKey({ name: "s" }); await settle(f);
    expect(f.captureCharFrame()).toContain("Shuffle: on");
    f.fail();
    f.screen.handleKey({ name: "r" });
    await settle(f);
    expect(f.captureCharFrame()).toContain("Repeat: off");
    expect(f.notices.at(-1)).toBe("Unable to apply: engine offline");
  } finally { f.close(); }
});

test("timeline clicks seek to bounded start and end positions", async () => {
  const f = await fixtureCtx();
  try {
    await f.renderOnce();
    const timeline = f.screen.root.findDescendantById("np-timeline")!;
    await f.mockMouse.click(timeline.x, timeline.y);
    await settle(f);
    const seeks = () => f.calls.filter(call => call.method === "player.action" && call.params?.action === "seek");
    expect(seeks().at(-1)?.params?.value).toBe(0);

    await f.mockMouse.click(timeline.x + timeline.width - 1, timeline.y);
    await settle(f);
    expect(seeks().at(-1)?.params?.value).toBe(232_000);
  } finally { f.close(); }
});

test("real sampled bands replace playback progress and silence remains flat", async () => {
  const f = await fixtureCtx();
  const sampled = trackStatus();
  sampled.prototype!.audio = { bands: [0, 1], level: 0.6, pulse: 0.5, tempo: null };
  try {
    f.update(sampled); await f.renderOnce();
    let frame = f.captureCharFrame();
    expect(frame).toContain("Audio spectrum");
    expect(frame).not.toContain("Playback progress");
    expect(frame).toContain("████████");
    f.motion(false);
    f.update({ ...sampled, prototype: { ...sampled.prototype!, audio: { bands: [1, 0], level: 0.6, pulse: 0, tempo: null } } });
    await new Promise(resolve => setTimeout(resolve, 110)); await f.renderOnce();
    frame = f.captureCharFrame(); expect(frame).toMatch(/[▂▃▄▅▆▇]{3}/);
    f.motion(true);
    f.update({ ...sampled, prototype: { ...sampled.prototype!, audio: { bands: [1, 1], level: 0, pulse: 0, tempo: null } } });
    await f.renderOnce(); frame = f.captureCharFrame();
    expect(frame).toContain("Audio spectrum · silence");
    expect(frame).not.toContain("████");
    f.update({ ...sampled, mode: { kind: "paused", positionMs: 82_000 } });
    await f.renderOnce(); expect(f.captureCharFrame()).toContain("Audio spectrum · silence");
    expect(audioSpectrum([0, 0.5, 1], 3)).toBe("─▅█");
  } finally { f.close(); }
});

test("189x34 centers bounded player and side queue cards", async () => {
  const f = await fixtureCtx({ width: 189, height: 34 });
  try {
    await f.renderOnce();
    const columns = f.screen.root.findDescendantById("np-columns")!;
    const card = f.screen.root.findDescendantById("np-player-card")!;
    const queue = f.screen.root.findDescendantById("np-up-next-card")!;
    expect(card.visible).toBe(true);
    expect(queue.visible).toBe(true);
    expect(columns.width).toBe(132);
    expect(Math.abs(columns.x - (189 - columns.width) / 2)).toBeLessThanOrEqual(1);
    expect(card.x).toBe(columns.x);
    expect(queue.x).toBe(card.x + card.width + 3);
    for (const panel of [card, queue]) {
      expect(panel.x).toBeGreaterThanOrEqual(0);
      expect(panel.y).toBeGreaterThanOrEqual(0);
      expect(panel.x + panel.width).toBeLessThanOrEqual(189);
      expect(panel.y + panel.height).toBeLessThanOrEqual(34);
    }
  } finally { f.close(); }
});

test("fifteen compact upcoming songs retain durations and never overlap radio status", async () => {
  for (const width of [112, 189]) {
    const live = trackStatus();
    live.prototype!.radio_active = true;
    live.prototype!.up_next = Array.from({ length: 16 }, (_, index) => ({
      ...live.prototype!.up_next[0]!,
      title: index === 14 ? "Fifteenth track" : index === 15 ? "Hidden sixteenth song" : `Song ${index + 1} 雨の音 with a long title`,
      artists: ["An artist with a long name"],
      duration: 181_000 + index * 1000,
    }));
    const f = await fixtureCtx({ width, height: 34, status: live });
    try {
      for (const theme of ["light", "dark"] as const) {
        f.screen.setTheme(theme); await f.renderOnce();
        const frame = f.captureCharFrame();
        expect(frame).toContain("Fifteenth track");
        expect(frame).toContain("15+ upcoming");
        expect(frame).toContain("Continuous radio");
        expect(frame).not.toContain("Hidden sixteenth song");
        const footer = f.screen.root.findDescendantById("np-queue-foot")!;
        const card = f.screen.root.findDescendantById("np-up-next-card")!;
        for (let index = 0; index < 15; index++) {
          const entry = f.screen.root.findDescendantById(`np-up-next-${index}`)!;
          const duration = f.screen.root.findDescendantById(`np-queue-duration-${index}`)!;
          expect(entry.height).toBe(1);
          expect(entry.y + entry.height).toBeLessThanOrEqual(footer.y);
          expect(duration.x + duration.width).toBeLessThan(card.x + card.width);
          expect(frame.split("\n")[duration.y]).toContain(`3:${String(index + 1).padStart(2, "0")}`);
          if (index > 0) expect(entry.y).toBe(f.screen.root.findDescendantById(`np-up-next-${index - 1}`)!.y + 1);
        }
      }
      f.update({ ...live, prototype: { ...live.prototype!, up_next: live.prototype!.up_next.slice(0, 2) } });
      await f.renderOnce();
      expect(f.captureCharFrame()).not.toContain("Fifteenth track");
      expect(f.captureCharFrame()).toContain("2 upcoming");
    } finally { f.close(); }
  }
});

test("full workspace at 80x24 keeps Now Playing controls within the terminal bounds", async () => {
  const setup = await createTestRenderer({ width: 80, height: 24 });
  const app = mountWorkspace(setup.renderer, {
    theme: "light",
    reducedMotion: true,
    api: { async call<T>(method: string, params?: Params) {
      if (method === "player.artwork") return unavailableArtwork(params) as T;
      return (method === "player.status" ? { repeat: "off", shuffle: false } : {}) as T;
    } },
  });
  app.setStatus(trackStatus());
  try {
    await setup.renderOnce();
    const frame = setup.captureCharFrame();
    expect(frame).toContain("The Colour of Air");
    expect(frame).toContain("Volume 68%");
    expect(frame).toContain("−");
    expect(frame).toContain("+");
    const card = setup.renderer.root.findDescendantById("np-player-card")!;
    const controls = setup.renderer.root.findDescendantById("np-volume-controls")!;
    const detail = setup.renderer.root.findDescendantById("np-detail")!;
    expect(detail.parent?.id).toBe("np-volume-controls");
    for (const node of [card, controls, detail]) {
      expect(node.x).toBeGreaterThanOrEqual(0);
      expect(node.y).toBeGreaterThanOrEqual(0);
      expect(node.x + node.width).toBeLessThanOrEqual(80);
      expect(node.y + node.height).toBeLessThanOrEqual(24);
    }
    expect(controls.x).toBeGreaterThanOrEqual(card.x);
    expect(controls.x + controls.width).toBeLessThanOrEqual(card.x + card.width);
  } finally { app.dispose(); setup.renderer.destroy(); }
});

test("share shortcut reports backend URL, missing items, and RPC errors through notifications", async () => {
  const f = await fixtureCtx();
  try {
    expect(f.screen.handleKey({ name: "y" })).toBe(true);
    await settle(f);
    expect(f.calls).toContainEqual({ method: "share", params: { uri: "spotify:track:demo1" } });
    expect(f.notices).toContain("https://open.spotify.com/track/demo1");
    f.fail();
    f.screen.handleKey({ name: "y" });
    await settle(f);
    expect(f.notices.at(-1)).toBe("Unable to share: engine offline");
    f.update({ mode: { kind: "stopped" }, playable: null, prototype: null });
    await f.renderOnce();
    f.screen.handleKey({ name: "y" });
    expect(f.notices.at(-1)).toBe("Choose an item before sharing");
    expect(f.calls.filter(call => call.method === "share")).toHaveLength(2);
  } finally { f.close(); }
});
