import { expect, test } from "bun:test";
import { BoxRenderable, InputRenderable, type Renderable } from "@opentui/core";
import { createTestRenderer } from "@opentui/core/testing";
import { paletteForTheme } from "../src/theme.js";
import { createQuickSearch } from "../src/screens/now-playing/quick-search.js";
import type { Page, Params, Row, ScreenContext } from "../src/workspace/contracts.js";

const wait = (ms = 0) => new Promise(resolve => setTimeout(resolve, ms));
const track = (id: string, title = id): Row => ({ id, kind: "track", title, subtitle: `${title} artist`, uri: `spotify:track:${id}`, duration_ms: 90_000 });
const page = (items: Row[], source = "search_cache", refresh_available = false): Page => ({ items, offset: 0, limit: 20, total: items.length, has_more: false, source, refresh_available });
function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason: unknown) => void;
  const promise = new Promise<T>((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
}

function findInput(root: Renderable): InputRenderable | undefined {
  if (root instanceof InputRenderable) return root;
  for (const child of root.getChildren()) {
    const found = findInput(child);
    if (found) return found;
  }
}

interface FixtureOptions {
  respond?: (method: string, params?: Params) => unknown | Promise<unknown>;
  width?: number;
  height?: number;
}

async function fixture(options: FixtureOptions = {}) {
  const setup = await createTestRenderer({ width: options.width ?? 80, height: options.height ?? 24, useMouse: true });
  let theme: "light" | "dark" = "light";
  const calls: Array<{ method: string; params?: Params }> = [];
  const notices: string[] = [];
  const parent = new BoxRenderable(setup.renderer, { id: "quick-search-test-parent", width: "100%", height: "100%", minWidth: 0, minHeight: 0 });
  setup.renderer.root.add(parent);
  const ctx: ScreenContext = {
    renderer: setup.renderer,
    api: { async call<T>(method: string, params?: Params) { calls.push({ method, params }); return await (options.respond?.(method, params) ?? {}) as T; } },
    theme: () => theme,
    setTheme(value) { theme = value; },
    reducedMotion: () => true,
    setReducedMotion() {},
    status: () => null,
    onStatus: () => () => {},
    navigate() {},
    notify(message) { notices.push(message); },
  };
  const controller = createQuickSearch(ctx, parent);
  const listener = (key: { name?: string; sequence?: string; ctrl?: boolean; meta?: boolean; shift?: boolean; preventDefault?(): void }) => {
    if (controller.handleKey(key)) key.preventDefault?.();
  };
  setup.renderer.keyInput.on("keypress", listener);
  return {
    ...setup,
    parent,
    controller,
    calls,
    notices,
    setTheme(value: "light" | "dark") { theme = value; controller.setTheme(value); },
    input() { return findInput(parent); },
    cleanup() { setup.renderer.keyInput.off("keypress", listener); controller.dispose(); setup.renderer.destroy(); },
  };
}

test("native input stays focused while printable text and arrows are routed correctly", async () => {
  const f = await fixture({ respond: async method => method === "search" ? page([track("alpha", "Alpha"), track("beta", "Beta")]) : {} });
  try {
    f.controller.open();
    await f.renderOnce();
    const input = f.input();
    expect(input).toBeDefined();
    expect(f.renderer.currentFocusedRenderable).toBe(input!);
    await f.mockInput.typeText("Q1lr");
    expect(f.controller.handleKey({ name: "a", ctrl: true })).toBe(false);
    expect(f.controller.handleKey({ name: "home" })).toBe(false);
    expect(f.calls.filter(call => call.method === "search")).toHaveLength(0);
    f.mockInput.pressEnter();
    await f.renderOnce();
    expect(f.captureCharFrame()).toContain("Searching… select a result");
    await wait(180);
    await f.renderOnce();
    expect(f.captureCharFrame()).toContain("Q1lr");
    expect(f.captureCharFrame()).toContain("Alpha");
    expect(f.calls.filter(call => call.method === "search").every(call => call.params?.limit === 10)).toBe(true);
    f.mockInput.pressArrow("down");
    expect(f.renderer.currentFocusedRenderable).toBe(input!);
    f.mockInput.pressEnter();
    await wait();
    expect(f.calls.at(-1)).toEqual({ method: "player.action", params: { action: "play", uri: "spotify:track:beta" } });
    expect(f.controller.isOpen()).toBe(false);
    expect(f.notices.at(-1)).toContain("Beta");
  } finally { f.cleanup(); }
});

test("empty queries do not call search, stale pages cannot replace the newest query, and cache refresh errors keep rows", async () => {
  let oldResolve!: (value: Page) => void;
  let newResolve!: (value: Page) => void;
  let refreshFails = false;
  const f = await fixture({ respond: (method, params) => {
    if (method !== "search") return {};
    if (params?.refresh) { if (refreshFails) throw new Error("offline"); return page([track("fresh", "Fresh")], "remote"); }
    if (params?.query === "old") return new Promise<Page>(resolve => { oldResolve = resolve; });
    if (params?.query === "new") return new Promise<Page>(resolve => { newResolve = resolve; });
    return page([track("cached", "Cached")], "local-search-cache", true);
  } });
  try {
    f.controller.open();
    await f.mockInput.typeText("old"); await wait(180);
    f.controller.handleKey({ name: "escape" });
    f.controller.open();
    await f.mockInput.typeText("new"); await wait(180);
    oldResolve(page([track("old", "Old result")]));
    await wait(); await f.renderOnce();
    expect(f.captureCharFrame()).not.toContain("Old result");
    newResolve(page([track("new", "New result")]));
    await wait(); await f.renderOnce();
    expect(f.captureCharFrame()).toContain("New result");
    f.controller.handleKey({ name: "escape" });
    f.controller.open();
    await f.mockInput.typeText("cached"); await wait(180); await f.renderOnce();
    expect(f.captureCharFrame()).toContain("Fresh");
    refreshFails = true;
    f.controller.handleKey({ name: "r", ctrl: true });
    await wait(); await f.renderOnce();
    expect(f.captureCharFrame()).toContain("Ctrl+R retry");
    expect(f.calls.some(call => call.method === "search" && call.params?.refresh === true)).toBe(true);
  } finally { f.cleanup(); }
});

test("a reordered refresh retains the selected URI before Enter", async () => {
  const refresh = deferred<Page>();
  const f = await fixture({ respond: async (method, params) => {
    if (method !== "search") return {};
    if (params?.refresh) return refresh.promise;
    return page([track("alpha", "Alpha"), track("beta", "Beta")], "local-search-cache", true);
  } });
  try {
    f.controller.open(); await f.mockInput.typeText("tracks"); await wait(180); await f.renderOnce();
    f.mockInput.pressArrow("down");
    refresh.resolve(page([track("beta", "Beta"), track("alpha", "Alpha")], "remote"));
    await wait(); await f.renderOnce();
    f.mockInput.pressEnter(); await wait();
    expect(f.calls.at(-1)).toEqual({ method: "player.action", params: { action: "play", uri: "spotify:track:beta" } });
  } finally { f.cleanup(); }
});

test("control actions use the selected URI, block duplicates, leave failures open, and cancel late requests on disposal", async () => {
  let resolveAction!: () => void;
  let failAction = false;
  const f = await fixture({ respond: async (method, params) => {
    if (method === "search") return page([track("alpha", "Alpha"), track("beta", "Beta")]);
    if (method === "queue.action") {
      if (failAction) throw new Error("queue offline");
      await new Promise<void>(resolve => { resolveAction = resolve; });
    }
    return params;
  } });
  try {
    f.controller.open(); await f.mockInput.typeText("song"); await wait(180); await f.renderOnce();
    f.mockInput.pressArrow("down");
    f.mockInput.pressKey("e", { ctrl: true });
    f.mockInput.pressKey("e", { ctrl: true });
    expect(f.calls.filter(call => call.method === "queue.action")).toHaveLength(1);
    resolveAction!(); await wait();
    expect(f.calls.at(-1)).toEqual({ method: "queue.action", params: { action: "append", uri: "spotify:track:beta" } });
    expect(f.controller.isOpen()).toBe(false);

    f.controller.open(); await f.mockInput.typeText("song"); await wait(180); await f.renderOnce();
    failAction = true; f.mockInput.pressKey("e", { ctrl: true }); await wait(); await f.renderOnce();
    expect(f.controller.isOpen()).toBe(true);
    expect(f.captureCharFrame()).toContain("queue offline");
    f.controller.dispose();
    expect(f.controller.isOpen()).toBe(false);
    await wait();
  } finally { f.cleanup(); }
});

test("late completion from a closed action cannot unlock a newer pending action", async () => {
  const actions: Array<ReturnType<typeof deferred<void>>> = [];
  const f = await fixture({ respond: async (method) => {
    if (method === "search") return page([track("alpha", "Alpha"), track("beta", "Beta")]);
    if (method === "queue.action") {
      const pending = deferred<void>(); actions.push(pending); await pending.promise;
    }
    return {};
  } });
  try {
    f.controller.open(); await f.mockInput.typeText("song"); await wait(180); await f.renderOnce();
    f.mockInput.pressKey("e", { ctrl: true });
    f.controller.handleKey({ name: "escape" });
    f.controller.open(); await f.mockInput.typeText("song"); await wait(180); await f.renderOnce();
    f.mockInput.pressKey("e", { ctrl: true });
    expect(actions).toHaveLength(2);
    actions[0]!.resolve(); await wait();
    f.mockInput.pressKey("e", { ctrl: true });
    expect(actions).toHaveLength(2);
    actions[1]!.resolve(); await wait();
    expect(f.controller.isOpen()).toBe(false);
  } finally { f.cleanup(); }
});

test("selection scrolls through long results, themes repaint every layer, and compact bounds stay inside the renderer", async () => {
  const items = Array.from({ length: 20 }, (_, index) => track(`track-${index}`, `Track ${index}`));
  const f = await fixture({ width: 80, height: 14, respond: async method => method === "search" ? page(items, "remote") : {} });
  try {
    f.controller.open(); await f.mockInput.typeText("tracks"); await wait(180); await f.renderOnce();
    for (let index = 0; index < 15; index++) f.mockInput.pressArrow("down");
    await f.renderOnce();
    expect(f.captureCharFrame()).toContain("Track 15");
    f.setTheme("dark"); await f.renderOnce();
    const popup = f.parent.findDescendantById("np-quick-search") as BoxRenderable;
    const hex = paletteForTheme("dark").panelRaised;
    expect(popup.backgroundColor.toInts()).toEqual([Number.parseInt(hex.slice(1, 3), 16), Number.parseInt(hex.slice(3, 5), 16), Number.parseInt(hex.slice(5, 7), 16), 255]);
    expect(popup.x).toBeGreaterThanOrEqual(0);
    expect(popup.y).toBeGreaterThanOrEqual(0);
    expect(popup.x + popup.width).toBeLessThanOrEqual(f.renderer.width);
    expect(popup.y + popup.height).toBeLessThanOrEqual(f.renderer.height);
    f.renderer.resize(120, 30); await f.renderOnce();
    expect(popup.x).toBeGreaterThan(0);
    expect(popup.x + popup.width).toBeLessThanOrEqual(120);
    expect(popup.y + popup.height).toBeLessThanOrEqual(30);
    f.renderer.resize(80, 14); await f.renderOnce();
    expect(popup.x + popup.width).toBeLessThanOrEqual(80);
    expect(popup.y + popup.height).toBeLessThanOrEqual(14);
    expect(f.captureCharFrame()).toContain("Track 15");
    f.controller.setTheme("light"); await f.renderOnce();
    expect(f.captureCharFrame()).toContain("Ctrl+E Queue");
    const queueButton = f.parent.findDescendantById("np-quick-search-queue")!;
    await f.mockMouse.click(queueButton.x + 2, queueButton.y); await wait();
    expect(f.calls.at(-1)).toEqual({ method: "queue.action", params: { action: "append", uri: "spotify:track:track-15" } });
  } finally { f.cleanup(); }
});
