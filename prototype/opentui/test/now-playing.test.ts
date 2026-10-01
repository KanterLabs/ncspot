import { expect, test } from "bun:test";
import { createTestRenderer } from "@opentui/core/testing";
import { createNowPlayingScreen, ambientProgress, audioSpectrum } from "../src/screens/now-playing/index.js";
import { mountWorkspace } from "../src/workspace/app.js";
import { demoStatus, type ParsedStatus } from "../src/status.js";
import type { ScreenContext, Params } from "../src/workspace/contracts.js";

async function fixtureCtx(width = 96, height = 34) {
  const setup = await createTestRenderer({ width, height, useMouse: true });
  let status: ParsedStatus = demoStatus(0, 82_000);
  status.playable = { ...status.playable!, uri: "spotify:track:demo1" };
  let listener: ((s: ParsedStatus) => void) | undefined;
  const calls: { method: string; params?: Params }[] = [];
  let fail = false;
  let reduced = true;
  let unsubscribed = false;
  const ctx: ScreenContext = {
    renderer: setup.renderer, api: { async call<T>(method: string, params?: Params) { calls.push({ method, params }); if (fail) throw new Error("engine offline"); return (method === "player.status" ? { repeat: "off", shuffle: false, saved: false, current: { uri: "spotify:track:demo1" } } : method === "share" ? { url: "https://open.spotify.com/track/demo1" } : { applied: true }) as T; } },
    theme: () => "light", setTheme() {}, reducedMotion: () => reduced, setReducedMotion(value) { reduced = value; },
    status: () => status, onStatus(value) { listener = value; return () => { listener = undefined; unsubscribed = true; }; }, navigate() {}, notify() {},
  };
  const screen = createNowPlayingScreen(ctx); setup.renderer.root.add(screen.root);
  return { ...setup, screen, calls, motion(value: boolean) { reduced = value; screen.refresh(); }, update(value: ParsedStatus) { status = value; listener?.(value); }, fail() { fail = true; }, unsubscribed: () => unsubscribed, close() { screen.dispose(); setup.renderer.destroy(); } };
}

test("Now Playing renders live metadata, truthful ambient label and cover fallback", async () => {
  const f = await fixtureCtx();
  try {
    await f.renderOnce();
    let frame = f.captureCharFrame();
    expect(frame).toContain("The Colour of Air"); expect(frame).toContain("Cover initials"); expect(frame).toContain("Ambient · playback progress"); expect(frame).toContain("1:22"); expect(frame).toContain("Reduced motion");
    f.update(demoStatus(1, 14_000)); await f.renderOnce(); frame = f.captureCharFrame(); expect(frame).toContain("Soft Geometry"); expect(frame).toContain("0:14");
    f.screen.setTheme("dark"); await f.renderOnce(); expect(f.captureCharFrame()).toContain("Soft Geometry");
  } finally { f.close(); }
  expect(f.unsubscribed()).toBe(true);
});

test("transport shortcuts send exact RPC vocabulary and expose failed mutations", async () => {
  const f = await fixtureCtx();
  try {
    expect(f.screen.editing?.()).toBe(false);
    f.screen.handleKey({ name: "space" }); await Promise.resolve();
    f.screen.handleKey({ name: "]" }); await Promise.resolve();
    f.screen.handleKey({ name: "+" }); await Promise.resolve();
    f.screen.handleKey({ name: "r", shift: true }); await Promise.resolve();
    f.screen.handleKey({ name: "f" }); await Promise.resolve();
    expect(f.calls).toContainEqual({ method: "player.action", params: { action: "play_pause" } });
    expect(f.calls.find(c => c.params?.action === "seek")?.params?.value).toBeGreaterThanOrEqual(87_000);
    expect(f.calls).toContainEqual({ method: "player.action", params: { action: "volume", value: 73 } });
    expect(f.calls).toContainEqual({ method: "radio.action", params: { action: "start", uri: "spotify:track:demo1" } });
    expect(f.calls).toContainEqual({ method: "library.action", params: { action: "save", kind: "track", uri: "spotify:track:demo1" } });
    f.fail(); f.screen.handleKey({ name: "space" }); await Promise.resolve(); await f.renderOnce(); expect(f.captureCharFrame()).toContain("engine offline");
    expect(f.screen.handleKey({ name: "x" })).toBe(false);
  } finally { f.close(); }
});

test("80x24 retains transport, seek and volume controls", async () => {
  const f = await fixtureCtx(80, 24);
  try { await f.renderOnce(); const frame = f.captureCharFrame(); expect(frame).toContain("The Colour of Air"); expect(frame).toContain("Pause"); expect(frame).toContain("Quieter"); expect(frame).toContain("+5s"); }
  finally { f.close(); }
  expect(ambientProgress(5, 10, 10)).toBe("━━━━━─────");
});

test("mouse buttons use acknowledged controls and failed repeat preserves its state", async () => {
  const f = await fixtureCtx();
  try {
    await f.renderOnce();
    const previous = f.screen.root.findDescendantById("np-previous")!;
    await f.mockMouse.click(previous.x + 2, previous.y + 1);
    expect(f.calls).toContainEqual({ method: "player.action", params: { action: "previous" } });
    f.screen.handleKey({ name: "s" }); await Promise.resolve(); await f.renderOnce();
    expect(f.captureCharFrame()).toContain("Shuffle: on");
    f.fail(); f.screen.handleKey({ name: "r" }); await Promise.resolve(); await f.renderOnce();
    expect(f.captureCharFrame()).toContain("Repeat: off");
    expect(f.captureCharFrame()).toContain("engine offline");
  } finally { f.close(); }
});

test("real sampled bands replace ambient graphics and silence remains flat", async () => {
  const f = await fixtureCtx();
  const sampled = demoStatus();
  sampled.prototype!.audio = { bands: [0, 1], level: 0.6, pulse: 0.5, tempo: null };
  try {
    f.update(sampled); await f.renderOnce();
    let frame = f.captureCharFrame();
    expect(frame).toContain("Audio spectrum"); expect(frame).not.toContain("Ambient · playback progress");
    expect(frame).toContain("████████");
    f.motion(false);
    f.update({ ...sampled, prototype: { ...sampled.prototype!, audio: { bands: [1, 0], level: 0.6, pulse: 0, tempo: null } } });
    await new Promise(resolve => setTimeout(resolve, 110)); await f.renderOnce();
    frame = f.captureCharFrame(); expect(frame).toMatch(/[▂▃▄▅▆▇]{3}/);
    f.motion(true);
    f.update({ ...sampled, prototype: { ...sampled.prototype!, audio: { bands: [1, 1], level: 0, pulse: 0, tempo: null } } });
    await f.renderOnce(); frame = f.captureCharFrame(); expect(frame).toContain("Audio spectrum · silence"); expect(frame).not.toContain("████");
    f.update({ ...sampled, mode: { kind: "paused", positionMs: 82_000 } }); await f.renderOnce(); expect(f.captureCharFrame()).toContain("Audio spectrum · silence");
    expect(audioSpectrum([0, 0.5, 1], 3)).toBe("─▅█");
  } finally { f.close(); }
});

test("full workspace chrome at 80x24 keeps Now Playing controls inside its border", async () => {
  const setup = await createTestRenderer({ width: 80, height: 24 });
  const app = mountWorkspace(setup.renderer, { theme: "light", reducedMotion: true, api: { async call<T>() { return { repeat: "off", shuffle: false } as T; } } });
  const live = demoStatus(); live.prototype!.audio = { bands: [0.2, 0.8], level: 0.5, pulse: 0, tempo: null };
  app.setStatus(live);
  try {
    await setup.renderOnce(); const frame = setup.captureCharFrame();
    expect(frame).toContain("The Colour of Air"); expect(frame).toContain("Audio spectrum"); expect(frame).toContain("+ Louder"); expect(frame).toContain("Volume 68%");
    const detail = setup.renderer.root.findDescendantById("np-detail")!;
    const surface = detail.parent!.parent!;
    expect(detail.y).toBeLessThan(surface.y + surface.height - 1);
    expect(frame).toContain("Connecting…");
  } finally { app.dispose(); setup.renderer.destroy(); }
});

test("share shortcut displays backend URL and reports missing items and RPC errors", async () => {
  const f = await fixtureCtx();
  try {
    expect(f.screen.handleKey({ name: "y" })).toBe(true); await Promise.resolve(); await f.renderOnce();
    expect(f.calls).toContainEqual({ method: "share", params: { uri: "spotify:track:demo1" } });
    expect(f.captureCharFrame()).toContain("https://open.spotify.com/track/demo1");
    f.fail(); f.screen.handleKey({ name: "y" }); await Promise.resolve(); await f.renderOnce();
    expect(f.captureCharFrame()).toContain("Unable to share: engine offline");
    const count = f.calls.length;
    f.update({ mode: { kind: "stopped" }, playable: null, prototype: null }); await Promise.resolve();
    f.screen.handleKey({ name: "y" }); await f.renderOnce();
    expect(f.captureCharFrame()).toContain("Choose an item before sharing");
    expect(f.calls.filter(call => call.method === "share")).toHaveLength(2);
    expect(f.calls.length).toBeGreaterThanOrEqual(count);
  } finally { f.close(); }
});
