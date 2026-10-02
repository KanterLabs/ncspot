import { expect, test } from "bun:test";
import { createTestRenderer } from "@opentui/core/testing";
import { RadioController } from "../src/screens/radio/controller.js";
import { diagnosticRows, radioLines } from "../src/screens/radio/model.js";
import { createRadioScreen } from "../src/screens/radio/index.js";
import type { Params, RpcApi, ScreenContext } from "../src/workspace/contracts.js";
import { demoStatus, type ParsedStatus } from "../src/status.js";

test("radio diagnostics show returned evidence and explicit unavailable fields", () => {
  const report = {
    source_count: 80,
    report: {
      catalog_count: 60,
      rng_seed: 42,
      fallback: false,
      reasons: ["seed relation available"],
      candidates: [{ track_uri: "spotify:track:rejected", excluded: true }],
      selected: [{ track: { title: "Next song", uri: "spotify:track:next" }, score: 1.25, reasons: ["artist affinity"], components: { artist_affinity: .5, recent_penalty: -.2 } }],
      discovery: { reasons: ["discovery target shortfall"] },
    },
  };
  const lines = radioLines({ active: true, waiting: false, discovery: 75, played_count: 4, cache_tracks: 80, catalog_tracks: 1136 }, report).join("\n");
  expect(lines).toContain("Unique catalog: 80");
  expect(lines).toContain("Catalog tracks: 1136   Liked tracks: 80");
  expect(lines).toContain("Quality: related candidates");
  expect(lines).toContain("Rejected: 1/1 cached candidates");
  expect(lines).toContain("seed relation available · discovery target shortfall");
  expect(radioLines({}).join("\n")).toContain("Discovery: unavailable");
  expect(radioLines({}).join("\n")).toContain("explicitly queued tracks may repeat");
  const rows = diagnosticRows(report);
  expect(rows[0]?.title).toBe("Next: Next song");
  expect(rows[0]?.detail).toContain("recent penalty: -0.200");
  expect(diagnosticRows({ report: { candidates: [{ track_uri: "spotify:track:x", excluded: true }] } })[0]?.subtitle).toBe("Score: unavailable · excluded");
});

test("radio loads status single-flight and debug only on request; disposal blocks updates", async () => {
  const calls: string[] = [];
  let listener: ((status: ParsedStatus) => void) | undefined;
  let unsubscribed = 0;
  let resolveStatus: ((value: unknown) => void) | undefined;
  let draws = 0;
  const messages: string[] = [];
  const api: RpcApi = { call<T>(method: string): Promise<T> {
    calls.push(method);
    if (method === "radio.status") return new Promise(resolve => { resolveStatus = value => resolve(value as T); });
    return Promise.reject(new Error("unsupported"));
  } };
  const controller = new RadioController(api, { lines() { draws++; }, rows() {}, message(value) { messages.push(value); } }, callback => { listener = callback; return () => { unsubscribed++; }; });
  const pending = controller.refresh();
  listener?.(demoStatus()); listener?.(demoStatus());
  expect(calls).toEqual(["radio.status"]);
  resolveStatus?.({ active: false, discovery: 100 });
  await pending;
  listener?.(demoStatus());
  expect(calls).toEqual(["radio.status"]);
  await controller.debug("bad");
  expect(messages.at(-1)).toContain("safe integer");
  await controller.debug("42");
  expect(calls).toEqual(["radio.status", "radio.debug"]);
  expect(messages.at(-1)).toContain("diagnostics unavailable: unsupported");
  const late = controller.refresh();
  const before = draws;
  controller.dispose(); controller.dispose();
  resolveStatus?.({ active: true });
  await late;
  expect(draws).toBe(before);
  expect(unsubscribed).toBe(1);
});

test("radio controls render at 80x24 in light and dark and clamp discovery", async () => {
  const setup = await createTestRenderer({ width: 80, height: 24 });
  const calls: { method: string; params?: Params }[] = [];
  let discovery = 100;
  const api: RpcApi = { async call<T>(method: string, params?: Params): Promise<T> {
    calls.push({ method, params });
    if (method === "radio.action" && params?.action === "discovery") discovery = Number(params.value);
    if (method === "radio.debug") return { source_count: 80, report: { rng_seed: 42, selected: [{ track: { title: "Diagnostic song", uri: "spotify:track:next" }, score: .5, components: { artist_affinity: .25 } }] } } as T;
    return { active: false, waiting: true, discovery, played_count: 4, cache_tracks: 80 } as T;
  } };
  const context: ScreenContext = { renderer: setup.renderer, api, theme: () => "light", setTheme() {}, reducedMotion: () => true, setReducedMotion() {}, status: () => null, onStatus: () => () => {}, navigate() {}, notify() {} };
  const screen = createRadioScreen(context);
  setup.renderer.root.add(screen.root);
  try {
    await screen.refresh();
    await setup.renderOnce();
    expect(setup.captureCharFrame()).toContain("RADIO STUDIO");
    expect(setup.captureCharFrame()).toContain("Choose a song");
    expect(setup.captureCharFrame()).not.toContain("Session exclusions:");
    screen.handleKey({ name: "right" });
    await new Promise(resolve => setTimeout(resolve, 0));
    expect(calls.find(call => call.method === "radio.action")?.params).toEqual({ action: "discovery", value: 100 });
    for (const [name, value] of [["f", 0], ["m", 50], ["e", 100]] as const) {
      expect(screen.handleKey({ name })).toBe(true);
      await new Promise(resolve => setTimeout(resolve, 0));
      expect(calls.filter(call => call.method === "radio.action").at(-1)?.params).toEqual({ action: "discovery", value });
    }
    expect(screen.handleKey({ name: "1" })).toBe(false);
    screen.setTheme("dark");
    await setup.renderOnce();
    expect(setup.captureCharFrame()).toContain("Explore  100%");
    screen.handleKey({ name: "d" });
    expect(screen.editing?.()).toBe(true);
    screen.handleKey({ name: "escape" });
    expect(screen.editing?.()).toBe(false);
    screen.handleKey({ name: "d" });
    setup.mockInput.pressEnter();
    await new Promise(resolve => setTimeout(resolve, 0));
    expect(calls.find(call => call.method === "radio.debug")?.params).toEqual({ limit: 20, rng_seed: 42 });
    await setup.renderOnce();
    expect(setup.captureCharFrame()).toContain("Diagnostic song");
    screen.handleKey({ name: "enter" });
    await setup.renderOnce();
    expect(setup.captureCharFrame()).toContain("artist affinity: 0.250");
    screen.handleKey({ name: "escape" });
    await setup.renderOnce();
    expect(setup.captureCharFrame()).toContain("Session exclusions: 4");
    screen.handleKey({ name: "escape" });
    await setup.renderOnce();
    expect(setup.captureCharFrame()).toContain("RADIO STUDIO");
  } finally { screen.dispose(); setup.renderer.destroy(); }
});
