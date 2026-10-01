import { expect, test } from "bun:test";
import type { Page, Params, Row, RpcApi } from "../src/workspace/contracts.js";
import { QueueController } from "../src/screens/queue/controller.js";
import { createQueueScreen } from "../src/screens/queue/index.js";
import { createTestRenderer } from "@opentui/core/testing";
import { demoStatus, type ParsedStatus } from "../src/status.js";
import type { ScreenContext } from "../src/workspace/contracts.js";

const entry = (id: string, index: number): Row => ({ id, kind: "track", title: "Duplicate song", subtitle: "Artist", uri: "spotify:track:same", meta: { index, current: index === 0 } });
const page = (rows: Row[], revision = "revision-1", offset = 0, has_more = false): Page => ({ items: rows, revision, offset, limit: 100, total: rows.length, has_more });

function harness(respond: (method: string, params: Params) => unknown | Promise<unknown>) {
  let rows: Row[] = [];
  let selectedId: string | undefined;
  const requests: { method: string; params: Params }[] = [];
  const messages: string[] = [];
  const api: RpcApi = { async call<T>(method: string, params: Params = {}): Promise<T> {
    requests.push({ method, params });
    return await respond(method, params) as T;
  } };
  const controller = new QueueController(api, {
    selected: () => rows.find(row => row.id === selectedId),
    rows: (next, id) => { rows = next; selectedId = id; },
    message: message => messages.push(message),
    notify: message => messages.push(message),
  });
  return { controller, requests, messages, select: (id: string) => { selectedId = id; }, selected: () => selectedId, rows: () => rows };
}

test("queue loads all pages and plays the selected duplicate occurrence with its revision", async () => {
  const h = harness((method, params) => method === "queue.list"
    ? params.offset === 0 ? page([entry("first", 0)], "revision-1", 0, true) : page([entry("second", 1)], "revision-1", 1)
    : {});
  await h.controller.refresh();
  h.select("second");
  await h.controller.action("play", { entry_id: h.selected() });
  expect(h.rows()).toHaveLength(2);
  expect(h.requests.find(request => request.method === "queue.action")?.params).toEqual({ action: "play", entry_id: "second", revision: "revision-1" });
  expect(h.selected()).toBe("second");
});

test("stale remove refetches without repeating the mutation and retains surviving duplicate selection", async () => {
  let changed = false;
  const h = harness(method => {
    if (method === "queue.action") { changed = true; throw { code: "stale_revision", message: "Queue revision is stale" }; }
    return page(changed ? [entry("second", 0)] : [entry("first", 0), entry("second", 1)], changed ? "revision-2" : "revision-1");
  });
  await h.controller.refresh();
  h.select("second");
  await h.controller.action("remove", { entry_id: "second" });
  expect(h.requests.filter(request => request.method === "queue.action")).toHaveLength(1);
  expect(h.selected()).toBe("second");
  expect(h.controller.revision).toBe("revision-2");
  expect(h.messages.at(-1)).toContain("stale");
});

test("reorder sends occurrence identity and absolute destination, then preserves selection", async () => {
  let changed = false;
  const h = harness(method => {
    if (method === "queue.action") { changed = true; return {}; }
    return page(changed ? [entry("second", 0), entry("first", 1)] : [entry("first", 0), entry("second", 1)], changed ? "revision-2" : "revision-1");
  });
  await h.controller.refresh();
  h.select("second");
  h.controller.moveSelected(-1);
  await new Promise(resolve => setTimeout(resolve, 0));
  expect(h.requests.find(request => request.method === "queue.action")?.params).toEqual({ action: "move", entry_id: "second", to: 0, revision: "revision-1" });
  expect(h.selected()).toBe("second");
});

test("stale reorder refetches a new revision without replaying the old destination", async () => {
  let changed = false;
  const h = harness(method => {
    if (method === "queue.action") { changed = true; throw new Error("stale_revision"); }
    return page(changed ? [entry("second", 0), entry("first", 1)] : [entry("first", 0), entry("second", 1)], changed ? "revision-2" : "revision-1");
  });
  await h.controller.refresh();
  h.select("second");
  h.controller.moveSelected(-1);
  await new Promise(resolve => setTimeout(resolve, 0));
  expect(h.requests.filter(request => request.method === "queue.action")).toHaveLength(1);
  expect(h.selected()).toBe("second");
  expect(h.controller.revision).toBe("revision-2");
  expect(h.messages.at(-1)).toContain("try again");
});

test("refresh errors preserve the visible queue and selected occurrence", async () => {
  let failed = false;
  const h = harness(() => {
    if (failed) throw new Error("offline");
    return page([entry("first", 0), entry("second", 1)]);
  });
  await h.controller.refresh();
  h.select("second");
  failed = true;
  await h.controller.refresh();
  expect(h.rows()).toHaveLength(2);
  expect(h.selected()).toBe("second");
  expect(h.messages.at(-1)).toContain("offline");
});

test("refresh is singleflight and disposal ignores an in-flight response", async () => {
  let resolve!: (value: Page) => void;
  const response = new Promise<Page>(done => { resolve = done; });
  const h = harness(() => response);
  const first = h.controller.refresh();
  const second = h.controller.refresh();
  expect(h.requests).toHaveLength(1);
  h.controller.dispose();
  resolve(page([entry("first", 0)]));
  await Promise.all([first, second]);
  expect(h.rows()).toEqual([]);
  await h.controller.action("clear");
  expect(h.requests).toHaveLength(1);
});

test("revision changes between pages restart loading rather than merge snapshots", async () => {
  let calls = 0;
  const h = harness(() => {
    calls++;
    return calls === 1 ? page([entry("old", 0)], "old", 0, true)
      : calls === 2 ? page([entry("moved", 1)], "new", 1)
      : page([entry("new", 0)], "new");
  });
  await h.controller.refresh();
  expect(h.rows().map(row => row.id)).toEqual(["new"]);
  expect(h.controller.revision).toBe("new");
});

test("native queue renders current entry, preserves transport space and cleans status subscription", async () => {
  const setup = await createTestRenderer({ width: 110, height: 25 });
  const requests: { method: string; params: Params }[] = [];
  let listener: ((status: ParsedStatus) => void) | undefined;
  let unsubscribed = 0;
  const ctx: ScreenContext = {
    renderer: setup.renderer,
    api: { async call<T>(method: string, params: Params = {}): Promise<T> {
      requests.push({ method, params });
      return (method === "queue.list" ? page([entry("first", 0), entry("second", 1)]) : {}) as T;
    } },
    theme: () => "light", setTheme() {}, reducedMotion: () => true, setReducedMotion() {},
    status: () => demoStatus(), onStatus(fn) { listener = fn; return () => { unsubscribed++; listener = undefined; }; },
    navigate() {}, notify() {},
  };
  const screen = createQueueScreen(ctx);
  setup.renderer.root.add(screen.root);
  try {
    await screen.refresh();
    await setup.renderOnce();
    expect(setup.captureCharFrame()).toContain("▶ Duplicate song");
    expect(screen.handleKey({ name: "space", sequence: " " })).toBe(false);
    screen.handleKey({ name: "down" });
    screen.handleKey({ name: "return" });
    await new Promise(resolve => setTimeout(resolve, 0));
    expect(requests.find(request => request.method === "queue.action")?.params).toEqual({ action: "play", entry_id: "second", revision: "revision-1" });
    const before = requests.filter(request => request.method === "queue.list").length;
    for (let i = 0; i < 50; i++) listener?.(demoStatus(0, i * 100));
    await new Promise(resolve => setTimeout(resolve, 10));
    expect(requests.filter(request => request.method === "queue.list")).toHaveLength(before);
    listener?.(demoStatus(1)); // A scheduled refresh must be canceled on disposal.
    screen.dispose(); screen.dispose();
    await new Promise(resolve => setTimeout(resolve, 550));
    expect(unsubscribed).toBe(1);
    expect(requests.filter(request => request.method === "queue.list")).toHaveLength(before);
  } finally { screen.dispose(); setup.renderer.destroy(); }
});
