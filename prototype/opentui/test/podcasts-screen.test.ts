import { expect, test } from "bun:test";
import { createTestRenderer } from "@opentui/core/testing";
import { PodcastsController } from "../src/screens/podcasts/controller.js";
import { createPodcastsScreen } from "../src/screens/podcasts/index.js";
import type { Page, Row, RpcApi, ScreenContext } from "../src/workspace/contracts.js";

const show: Row = { id: "show1", kind: "show", title: "A podcast", subtitle: "Publisher", uri: "spotify:show:show1", saved: true };
const episode: Row = { id: "episode1", kind: "episode", title: "First episode", subtitle: "2026-10-01", uri: "spotify:episode:actual-uri", duration_ms: 120000, meta: { resume_position_ms: 30000 } };
const page = (items: Row[], offset = 0, has_more = false): Page => ({ items, offset, limit: 50, total: 55, has_more });
function harness(call: RpcApi["call"]) {
  let rows: Row[] = [], message = "", notifications: string[] = [];
  const controller = new PodcastsController({ call }, { rows(value) { rows = value; }, message(value) { message = value; }, notify(value) { notifications.push(value); } });
  return { controller, rows: () => rows, message: () => message, notifications };
}

test("podcasts loads show detail and uses the episode URI for playback and queue actions", async () => {
  const calls: unknown[] = [];
  const state = harness(async (method, params) => { calls.push([method, params]); return (method === "library.detail" ? page([episode]) : page([show])) as any; });
  await state.controller.refresh();
  await state.controller.open(show);
  expect(calls[1]).toEqual(["library.detail", { kind: "show", id: "show1", uri: show.uri, offset: 0, limit: 50 }]);
  for (const action of ["play", "append", "play_next"] as const) await state.controller.episodeAction(episode, action);
  expect(calls.slice(2)).toEqual([["player.action", { action: "play", uri: episode.uri }], ["queue.action", { action: "append", uri: episode.uri }], ["queue.action", { action: "play_next", uri: episode.uri }]]);
  await state.controller.back();
  expect(state.controller.show).toBeUndefined();
});

test("podcasts ignores stale loads and callbacks after disposal", async () => {
  const resolvers: ((value: any) => void)[] = [];
  const state = harness(() => new Promise(resolve => resolvers.push(resolve)));
  const first = state.controller.refresh();
  const detail = state.controller.open(show);
  resolvers[1]!(page([episode])); await detail;
  resolvers[0]!(page([show])); await first;
  expect(state.rows()).toEqual([episode]);
  const pending = state.controller.back();
  const message = state.message();
  state.controller.dispose(); resolvers[2]!(page([show])); await pending;
  expect(state.message()).toBe(message);
});

test("podcasts preserves cached results on refresh error and resets pagination for filters", async () => {
  const calls: any[] = [];
  let fail = false;
  const state = harness(async (method, params) => { calls.push(params); if (fail) throw new Error("offline"); return page([show], Number(params?.offset), params?.offset === 0) as any; });
  await state.controller.refresh(); fail = true; await state.controller.refresh();
  expect(state.rows()[0]?.id).toBe(show.id);
  expect(state.message()).toContain("offline · showing cached results");
  fail = false; await state.controller.next();
  expect(calls.at(-1).offset).toBe(1);
  await state.controller.search(" space ");
  expect(calls.at(-1)).toMatchObject({ offset: 0, filter: "space" });
});

test("podcast save toggle calls the show action and invalidates saved-show cache", async () => {
  const calls: any[] = [];
  const state = harness(async (method, params) => { calls.push([method, params]); return page([]) as any; });
  const selected = { ...show };
  await state.controller.toggleSaved(selected);
  expect(calls[0]).toEqual(["library.action", { action: "unsave", kind: "show", id: show.id, uri: show.uri }]);
  expect(selected.saved).toBe(false);
});

test("podcast screen renders episode resume metadata, supports Enter and cleans up status", async () => {
  const setup = await createTestRenderer({ width: 80, height: 24 });
  const calls: any[] = [];
  let removed = 0;
  const context: ScreenContext = {
    renderer: setup.renderer, api: { async call(method, params) { calls.push([method, params]); return page([episode]) as any; } },
    theme: () => "dark", setTheme() {}, reducedMotion: () => true, setReducedMotion() {}, status: () => null,
    onStatus: () => () => { removed++; }, navigate() {}, notify() {},
  };
  const screen = createPodcastsScreen(context, { id: show.id, title: show.title, uri: show.uri });
  setup.renderer.root.add(screen.root);
  try {
    await screen.refresh(); await setup.renderOnce();
    const frame = setup.captureCharFrame();
    expect(frame).toContain("A podcast"); expect(frame).toContain("First episode"); expect(frame).toContain("Resume 0:30");
    screen.handleKey({ name: "return" }); await Promise.resolve();
    expect(calls.at(-1)).toEqual(["player.action", { action: "play", uri: episode.uri }]);
    screen.setTheme("light"); await setup.renderOnce();
  } finally { screen.dispose(); setup.renderer.destroy(); }
  expect(removed).toBe(1);
});
