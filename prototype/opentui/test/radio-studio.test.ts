import { expect, test } from "bun:test";
import { createTestRenderer } from "@opentui/core/testing";
import { createRadioScreen } from "../src/screens/radio/index.js";
import type { Artwork } from "../src/screens/now-playing/index.js";
import type { ParsedStatus, Track, UpNextOrigin } from "../src/status.js";
import type { Params, RpcApi, Screen, ScreenContext } from "../src/workspace/contracts.js";

interface RadioStatus {
  active: boolean;
  waiting: boolean;
  discovery: number;
  played_count: number;
  cache_tracks: number;
  queue_mode?: "station" | "context";
  parked_count?: number;
  catalog_tracks?: number;
  radio_pending_count?: number;
  explicit_pending_count?: number;
  seed_track?: Track;
  seed?: string;
}

interface Call {
  method: string;
  params?: Params;
}

interface FixtureOptions {
  width: number;
  height: number;
  status?: ParsedStatus;
  radioStatus?: Partial<RadioStatus>;
  artwork?: (params: Params) => Artwork | Promise<Artwork>;
}

interface RadioFixture {
  renderer: Awaited<ReturnType<typeof createTestRenderer>>["renderer"];
  renderOnce(): Promise<void>;
  mockMouse: Awaited<ReturnType<typeof createTestRenderer>>["mockMouse"];
  mockInput: Awaited<ReturnType<typeof createTestRenderer>>["mockInput"];
  screen: Screen;
  calls: Call[];
  update(status: ParsedStatus): void;
  radioStatus: RadioStatus;
  close(): void;
}

const tick = () => new Promise<void>(resolve => setTimeout(resolve, 0));

async function settle(fixture: Pick<RadioFixture, "renderOnce">): Promise<void> {
  // Controller writes are followed by a confirming status read. Let both
  // promise turns run before capturing the native buffer.
  for (let index = 0; index < 3; index++) {
    await Promise.resolve();
    await tick();
  }
  await fixture.renderOnce();
}

function track(index: number, title = `Next song ${String(index).padStart(2, "0")}`): Track {
  return {
    type: "Track",
    id: `track-${index}`,
    uri: `spotify:track:track-${index}`,
    title,
    artists: [`Artist ${String(index).padStart(2, "0")}`],
    album: "Radio Studies",
    duration: 60_000 + index * 1_000,
  };
}

function liveStatus(current: Track, upcoming: Track[] = [], radioActive = false, origins?: UpNextOrigin[], waiting = false): ParsedStatus {
  return {
    mode: { kind: "paused", positionMs: 12_000 },
    playable: current,
    prototype: {
      position_ms: 12_000,
      discovery: 50,
      volume_percent: 68,
      radio_active: radioActive,
      radio_waiting: waiting,
      up_next: upcoming,
      ...(origins ? { up_next_origins: origins } : {}),
    },
  };
}

function unavailableArtwork(params: Params): Artwork {
  return {
    available: false,
    uri: typeof params.uri === "string" ? params.uri : "",
    width: typeof params.width === "number" ? params.width : 12,
    height: typeof params.height === "number" ? params.height : 6,
  };
}

function coloredArtwork(uri: string, width: number, height: number): Artwork {
  return {
    available: true,
    uri,
    width,
    height,
    pixels: Array.from({ length: width * height * 2 }, (_, index) => index % 2 ? "#445566" : "#112233"),
  };
}

async function fixture(options: FixtureOptions): Promise<RadioFixture> {
  const setup = await createTestRenderer({ width: options.width, height: options.height, useMouse: true });
  let live = options.status ?? liveStatus(track(0));
  const listeners = new Set<(status: ParsedStatus) => void>();
  const calls: Call[] = [];
  const radioStatus: RadioStatus = {
    active: false,
    waiting: false,
    discovery: 50,
    played_count: 12,
    cache_tracks: 64,
    ...options.radioStatus,
  };

  const api: RpcApi = {
    async call<T>(method: string, params?: Params): Promise<T> {
      calls.push({ method, params });
      if (method === "player.artwork") return await (options.artwork?.(params ?? {}) ?? unavailableArtwork(params ?? {})) as T;
      if (method === "radio.action") {
        if (params?.action === "discovery") radioStatus.discovery = Number(params.value);
        if (params?.action === "start") radioStatus.active = true;
        if (params?.action === "stop") radioStatus.active = false;
        return { applied: true } as T;
      }
      if (method === "radio.status") return { ...radioStatus } as T;
      if (method === "radio.debug") return {
        source_count: 64,
        report: { rng_seed: 42, selected: [{ track: { title: "Diagnostic candidate", uri: "spotify:track:diagnostic" }, score: 0.75 }] },
      } as T;
      throw new Error(`unexpected RPC ${method}`);
    },
  };
  const context: ScreenContext = {
    renderer: setup.renderer,
    api,
    theme: () => "light",
    setTheme() {},
    reducedMotion: () => true,
    setReducedMotion() {},
    status: () => live,
    onStatus(listener) { listeners.add(listener); return () => listeners.delete(listener); },
    navigate() {},
    notify() {},
  };
  const screen = createRadioScreen(context);
  setup.renderer.root.add(screen.root);
  return {
    renderer: setup.renderer,
    renderOnce: setup.renderOnce,
    mockMouse: setup.mockMouse,
    mockInput: setup.mockInput,
    screen,
    calls,
    update(status) { live = status; for (const listener of listeners) listener(status); },
    radioStatus,
    close() { screen.dispose(); setup.renderer.destroy(); },
  };
}

function node(fixture: RadioFixture, id: string) {
  const value = fixture.screen.root.findDescendantById(id);
  expect(value).toBeDefined();
  return value!;
}

function content(value: unknown): string {
  const raw = (value as { content?: unknown })?.content;
  if (typeof raw === "string") return raw;
  if (raw && typeof raw === "object" && "chunks" in raw && Array.isArray((raw as { chunks?: unknown }).chunks)) {
    return (raw as { chunks: Array<{ text?: unknown }> }).chunks.map(chunk => String(chunk.text ?? "")).join("");
  }
  return String(raw ?? "");
}

function contained(inner: { x: number; y: number; width: number; height: number }, outer: { x: number; y: number; width: number; height: number }) {
  expect(inner.x).toBeGreaterThanOrEqual(outer.x);
  expect(inner.y).toBeGreaterThanOrEqual(outer.y);
  expect(inner.x + inner.width).toBeLessThanOrEqual(outer.x + outer.width);
  expect(inner.y + inner.height).toBeLessThanOrEqual(outer.y + outer.height);
}

test("Radio Studio natively fits 15 fixed-duration songs in wide side-by-side cards in both themes", async () => {
  const upcoming = Array.from({ length: 15 }, (_, index) => track(index + 1));
  for (const width of [189, 112] as const) {
    const f = await fixture({
      width,
      height: 34,
      status: liveStatus(track(0), upcoming, true, Array.from({ length: 15 }, () => "radio" as const)),
      radioStatus: {
        active: true,
        discovery: 50,
        queue_mode: "station",
        parked_count: 6,
        catalog_tracks: 1136,
        radio_pending_count: 15,
        explicit_pending_count: 2,
      },
    });
    try {
      await settle(f);
      const card = node(f, "radio-station-card");
      const nextCard = node(f, "radio-next-card");
      const body = node(f, "radio-next-body");
      const footer = node(f, "radio-next-footer");
      expect(nextCard.visible).toBe(true);
      expect(card.width).toBe(width === 189 ? 85 : 68);
      expect(nextCard.width).toBe(width === 189 ? 44 : 37);
      expect(card.x + card.width).toBeLessThanOrEqual(width);
      expect(nextCard.x + nextCard.width).toBeLessThanOrEqual(width);
      expect(footer.y + footer.height).toBeLessThanOrEqual(nextCard.y + nextCard.height);
      expect(content(footer)).toContain("15 radio · 2 queued (+)");
      expect(content(footer)).toContain("6 parked · stop resumes");
      expect(content(footer)).toContain("1,136 catalog · no auto repeats");
      expect(content(node(f, "radio-next-title"))).toContain("UP NEXT · STATION");

      for (const [index, song] of upcoming.entries()) {
        const entry = node(f, `radio-next-${index}`);
        const title = node(f, `radio-next-name-${index}`);
        const duration = node(f, `radio-next-duration-${index}`);
        expect(entry.visible).toBe(true);
        contained(entry, body);
        expect(entry.y + entry.height).toBeLessThanOrEqual(footer.y);
        contained(title, entry);
        contained(duration, entry);
        expect(content(title)).toContain(song.title);
        expect(content(duration)).toBe(`${1 + Math.floor(index / 60)}:${String(index + 1).padStart(2, "0")}`);
      }

      for (const theme of ["light", "dark"] as const) {
        f.screen.setTheme(theme);
        await f.renderOnce();
        const frame = f.renderer.currentRenderBuffer.getRealCharBytes(true);
        const text = new TextDecoder().decode(frame);
        expect(text).toContain("RADIO STUDIO");
        expect(text).toContain("UP NEXT");
        expect(text).toContain("Next song 01");
        expect(text).toContain("Next song 15");
        expect(text).toContain("1:01");
        expect(text).toContain("1:15");
        expect(text).toContain("1,136 catalog");
        expect(text).toContain("stop resumes");
        expect(text).toContain("queued (+)");
      }
    } finally {
      f.close();
    }
  }
});

test("compact Radio Studio keeps every main control inside the station card and hides UP NEXT", async () => {
  const f = await fixture({ width: 80, height: 24, status: liveStatus(track(0)), radioStatus: { active: true } });
  try {
    await settle(f);
    const card = node(f, "radio-station-card");
    expect(node(f, "radio-next-card").visible).toBe(false);
    for (const id of [
      "radio-heading", "radio-hero", "radio-seed", "radio-discovery-heading", "radio-slider", "radio-scale", "radio-presets", "radio-caption", "radio-actions",
      "radio-familiar", "radio-balanced", "radio-explore", "radio-toggle", "radio-reseed", "radio-diagnostics",
    ]) {
      const control = node(f, id);
      expect(control.visible).toBe(true);
      contained(control, card);
    }
    const frame = f.renderer.currentRenderBuffer.getRealCharBytes(true);
    const text = new TextDecoder().decode(frame);
    expect(text).toContain("RADIO STUDIO");
    expect(text).not.toContain("UP NEXT");
    expect(content(node(f, "radio-caption"))).toContain("Related songs only · Explore stays cache-bound.");
  } finally {
    f.close();
  }
});

test("Radio Studio attributes explicit and related queue entries and preserves parked context", async () => {
  const upcoming = [track(1, "Explicit request"), track(2, "Related pick")];
  const f = await fixture({
    width: 112,
    height: 34,
    status: liveStatus(track(0), upcoming, true, ["explicit", "radio"]),
    radioStatus: {
      active: true,
      queue_mode: "station",
      parked_count: 4,
      catalog_tracks: 1136,
      cache_tracks: 45,
      radio_pending_count: 1,
      explicit_pending_count: 1,
    },
  });
  try {
    await settle(f);
    expect(content(node(f, "radio-next-origin-0"))).toBe("+");
    expect(content(node(f, "radio-next-origin-1"))).toBe("◇");
    expect(content(node(f, "radio-next-footer"))).toContain("1 radio · 1 queued (+)");
    expect(content(node(f, "radio-next-footer"))).toContain("4 parked · stop resumes");
    expect(content(node(f, "radio-next-footer"))).toContain("1,136 catalog · no auto repeats");
  } finally {
    f.close();
  }
});

test("legacy radio status labels upcoming as queue and does not claim radio provenance", async () => {
  const f = await fixture({
    width: 112,
    height: 34,
    status: liveStatus(track(0), [track(1)], true),
    radioStatus: { active: true },
  });
  try {
    await settle(f);
    expect(content(node(f, "radio-next-origin-0"))).toBe("");
    expect(content(node(f, "radio-next-title"))).toBe("UP NEXT · QUEUE");
    expect(content(node(f, "radio-next-footer"))).toContain("QUEUE UPCOMING · 1 upcoming");
    expect(content(node(f, "radio-next-footer"))).not.toContain("STATION QUEUED");
  } finally {
    f.close();
  }
});

test("waiting station reports related-cache exhaustion and finite metadata coverage", async () => {
  const f = await fixture({
    width: 112,
    height: 34,
    status: liveStatus(track(0), [], true, [], true),
    radioStatus: { active: true, waiting: true, queue_mode: "station", parked_count: 3, catalog_tracks: 1136 },
  });
  try {
    await settle(f);
    expect(content(node(f, "radio-state"))).toContain("RELATED CACHE EXHAUSTED");
    expect(content(node(f, "radio-next-empty"))).toContain("Waiting for related metadata");
    expect(content(node(f, "radio-next-footer"))).toContain("RELATED CACHE EXHAUSTED");
    expect(content(node(f, "radio-next-footer"))).toContain("3 parked · stop resumes");
    expect(content(node(f, "radio-next-footer"))).toContain("1,136 catalog · no auto repeats");
    expect(content(node(f, "radio-next-footer"))).not.toContain("Continuous");
  } finally {
    f.close();
  }
});

test("seed_track from radio.status stays displayed and artwork is requested once for the seed dimensions", async () => {
  const seed = track(41, "Cached station seed");
  const first = track(0, "Playing before radio");
  const changed = track(42, "Playing after radio");
  const f = await fixture({
    width: 112,
    height: 34,
    status: liveStatus(first, [], true),
    radioStatus: { active: true, seed_track: seed, seed: seed.uri },
  });
  try {
    await settle(f);
    const artworkCalls = () => f.calls.filter(call => call.method === "player.artwork");
    expect(artworkCalls()).toEqual([
      { method: "player.artwork", params: { uri: seed.uri, width: 12, height: 6 } },
    ]);
    expect(content(node(f, "radio-seed-title"))).toBe(seed.title);
    expect(content(node(f, "radio-seed-artist"))).toBe(seed.artists[0]);

    f.update(liveStatus(changed, [], true));
    await settle(f);
    expect(content(node(f, "radio-seed-title"))).toBe(seed.title);
    expect(content(node(f, "radio-seed-artist"))).toBe(seed.artists[0]);
    expect(artworkCalls()).toHaveLength(1);
    expect(artworkCalls()[0]?.params).toEqual({ uri: seed.uri, width: 12, height: 6 });
  } finally {
    f.close();
  }
});

test("late seed artwork responses are ignored after Radio Studio disposal", async () => {
  let resolveArtwork!: (value: Artwork) => void;
  const pending = new Promise<Artwork>(resolve => { resolveArtwork = resolve; });
  const seed = track(41, "Deferred station seed");
  const f = await fixture({
    width: 112,
    height: 34,
    status: liveStatus(track(0), [], true),
    radioStatus: { active: true, seed_track: seed, seed: seed.uri },
    artwork: () => pending,
  });
  const cover = node(f, "radio-cover");
  try {
    await settle(f);
    expect(f.calls.filter(call => call.method === "player.artwork")).toEqual([
      { method: "player.artwork", params: { uri: seed.uri, width: 12, height: 6 } },
    ]);
    expect(cover.getChildren().filter(child => child.id.startsWith("radio-art-"))).toHaveLength(0);
    f.screen.dispose();
    resolveArtwork(coloredArtwork(seed.uri!, 12, 6));
    await Promise.resolve();
    await Promise.resolve();
    expect(cover.getChildren().filter(child => child.id.startsWith("radio-art-")).length).toBe(0);
    expect(f.screen.root.isDestroyed).toBe(true);
  } finally {
    f.renderer.destroy();
  }
});

test("mouse presets and slider endpoints send exact discovery values and show backend acknowledgement", async () => {
  const f = await fixture({ width: 112, height: 34, status: liveStatus(track(0)), radioStatus: { discovery: 50 } });
  try {
    await settle(f);
    const actionCalls = () => f.calls.filter(call => call.method === "radio.action");
    const familiar = node(f, "radio-familiar");
    const explore = node(f, "radio-explore");
    const slider = node(f, "radio-slider");

    await f.mockMouse.click(familiar.x + 1, familiar.y + 1);
    await settle(f);
    expect(actionCalls().at(-1)).toEqual({ method: "radio.action", params: { action: "discovery", value: 0 } });
    expect(f.radioStatus.discovery).toBe(0);
    expect(content(node(f, "radio-discovery-value"))).toContain("Familiar  0%");

    await f.mockMouse.click(explore.x + 1, explore.y + 1);
    await settle(f);
    expect(actionCalls().at(-1)).toEqual({ method: "radio.action", params: { action: "discovery", value: 100 } });
    expect(f.radioStatus.discovery).toBe(100);
    expect(content(node(f, "radio-discovery-value"))).toContain("Explore  100%");

    await f.mockMouse.click(slider.x, slider.y);
    await settle(f);
    expect(actionCalls().at(-1)).toEqual({ method: "radio.action", params: { action: "discovery", value: 0 } });
    await f.mockMouse.click(slider.x + slider.width - 1, slider.y);
    await settle(f);
    expect(actionCalls().at(-1)).toEqual({ method: "radio.action", params: { action: "discovery", value: 100 } });
    expect(content(node(f, "radio-discovery-value"))).toContain("Explore  100%");
  } finally {
    f.close();
  }
});

test("start sends currentTrack.uri, stop sends radio.action stop, and diagnostics run only on request", async () => {
  const current = track(7, "Current station source");
  const f = await fixture({ width: 112, height: 34, status: liveStatus(current), radioStatus: { active: false } });
  try {
    await settle(f);
    expect(f.calls.filter(call => call.method === "radio.debug")).toHaveLength(0);

    expect(f.screen.handleKey({ name: "s" })).toBe(true);
    await settle(f);
    expect(f.calls).toContainEqual({ method: "radio.action", params: { action: "start", uri: current.uri } });
    expect(f.radioStatus.active).toBe(true);

    expect(f.screen.handleKey({ name: "s" })).toBe(true);
    await settle(f);
    expect(f.calls).toContainEqual({ method: "radio.action", params: { action: "stop" } });
    expect(f.calls.filter(call => call.method === "radio.debug")).toHaveLength(0);

    expect(f.screen.handleKey({ name: "d" })).toBe(true);
    expect(f.screen.editing?.()).toBe(true);
    f.mockInput.pressEnter();
    await settle(f);
    expect(f.calls.filter(call => call.method === "radio.debug")).toEqual([
      { method: "radio.debug", params: { limit: 20, rng_seed: 42 } },
    ]);
  } finally {
    f.close();
  }
});
