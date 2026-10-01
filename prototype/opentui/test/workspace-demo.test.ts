import { expect, test } from "bun:test";
import { DemoApi } from "../src/workspace/demo.js";
import { RpcError } from "../src/workspace/client.js";
import type { Page, Row } from "../src/workspace/contracts.js";

test("offline fixtures cover browsing, searching and playback without shared state", async () => {
  let broadcasts = 0;
  const demo = new DemoApi(() => broadcasts++);
  for (const kind of ["tracks", "albums", "artists", "playlists", "shows", "browse"]) {
    const page = await demo.call<Page>("library.list", { kind });
    expect(page.source).toBe("offline preview");
    expect(page.total).toBeGreaterThan(0);
    expect((await demo.call<Page>("library.detail", { kind, id: page.items[0]!.id })).items.length).toBeGreaterThan(0);
  }
  expect((await demo.call<Page>("search", { query: "Soft" })).items[0]!.title).toBe("Soft Geometry");
  expect((await demo.call<Page>("library.list", { kind: "tracks", filter: "", sort: "added" })).total).toBe(4);
  expect(await demo.call("player.status")).toMatchObject({ current: { uri: "spotify:track:demo-1" }, repeat: "off", shuffle: false, saved: true });
  const before = demo.status.prototype!.position_ms;
  demo.tick(); expect(demo.status.prototype!.position_ms).toBe(before + 1_000);
  await demo.call("player.action", { action: "play_pause" });
  demo.tick(); expect(demo.status.prototype!.position_ms).toBe(before + 1_000);
  await demo.call("player.action", { action: "volume", value: 25 });
  expect(demo.status.prototype!.volume_percent).toBe(25);
  await demo.call("player.action", { action: "play", uri: "spotify:episode:demo-1" });
  expect(demo.status.playable?.type).toBe("Episode");
  expect(broadcasts).toBeGreaterThan(3);
  expect(new DemoApi().status.prototype!.volume_percent).toBe(68);
});

test("queue duplicate identities and stale revision checks protect mutations", async () => {
  const demo = new DemoApi();
  const first = await demo.call<Page>("queue.list");
  await demo.call("queue.action", { action: "append", uri: "spotify:track:demo-1" });
  await demo.call("queue.action", { action: "append", uri: "spotify:track:demo-1" });
  const page = await demo.call<Page>("queue.list");
  expect(new Set(page.items.map(row => row.id)).size).toBe(page.total);
  expect(page.items.filter(row => row.uri === "spotify:track:demo-1")).toHaveLength(3);
  await expect(demo.call("queue.action", { action: "remove", entry_id: first.items[0]!.id, revision: first.revision })).rejects.toMatchObject({ code: "stale_revision" });
  const remove = page.items.at(-1)!;
  await demo.call("queue.action", { action: "remove", entry_id: remove.id, revision: page.revision });
  const updated = await demo.call<Page>("queue.list");
  expect(updated.items.some(row => row.id === remove.id)).toBe(false);
  await demo.call("queue.action", { action: "move", entry_id: updated.items.at(-1)!.id, revision: updated.revision, to: 0 });
  const moved = await demo.call<Page>("queue.list");
  expect(moved.items[0]!.id).toBe(updated.items.at(-1)!.id);
  await demo.call("queue.action", { action: "clear", revision: moved.revision });
  expect((await demo.call<Page>("queue.list")).total).toBe(0);
  expect(demo.status.mode.kind).toBe("stopped");
});

test("playlist, saved items, radio, cast and settings actions mutate only demo fixtures", async () => {
  const demo = new DemoApi();
  const playlist = await demo.call<Row>("playlist.action", { action: "create", name: "Test playlist" });
  await demo.call("playlist.action", { action: "rename", id: playlist.id, name: "Renamed" });
  await demo.call("playlist.action", { action: "add", id: playlist.id, uri: "spotify:track:demo-2" });
  expect((await demo.call<Page>("library.detail", { kind: "playlist", id: playlist.id })).items[0]!.title).toBe("Soft Geometry");
  await demo.call("playlist.action", { action: "remove", id: playlist.id, position: 0 });
  expect((await demo.call<Page>("library.detail", { kind: "playlist", id: playlist.id })).total).toBe(0);
  await demo.call("playlist.action", { action: "delete", id: playlist.id });
  expect((await demo.call<Page>("library.list", { kind: "playlists" })).items.some(row => row.id === playlist.id)).toBe(false);
  await demo.call("library.action", { action: "unsave", kind: "track", id: "demo-1" });
  expect((await demo.call<Page>("library.list", { kind: "tracks" })).items[0]!.saved).toBe(false);
  await demo.call("radio.action", { action: "start" });
  await demo.call("radio.action", { action: "discovery", value: 80 });
  expect(await demo.call("radio.status")).toMatchObject({ active: true, discovery: 80, source: "offline preview" });
  expect(await demo.call("radio.debug")).toMatchObject({ demo: true });
  await demo.call("cast.action", { action: "connect", id: "cast-1" });
  expect((await demo.call<Page>("cast.list")).items[0]!.meta!.active).toBe(true);
  await demo.call("cast.action", { action: "disconnect" });
  expect((await demo.call<Page>("cast.list")).items[0]!.meta!.active).toBe(false);
  await demo.call("settings.action", { action: "command", command: "volume 42" });
  expect((await demo.call<{ values: Record<string, unknown> }>("settings.get")).values.volume).toBe(42);
  await demo.call("settings.action", { action: "logout" });
  expect((await demo.call<{ values: Record<string, unknown> }>("settings.get")).values.logged_in).toBe(false);
});

test("unsupported and malformed requests reject RpcError and results are detached", async () => {
  const demo = new DemoApi();
  for (const [method, params] of [["unknown", {}], ["player.action", { action: "destroy" }], ["player.action", { action: "volume", value: -1 }], ["library.list", { kind: "bad" }], ["library.list", { kind: "tracks", offset: -1 }], ["playlist.action", { action: "add", id: "missing", uri: "bad" }], ["settings.action", { action: "command", command: "arbitrary-command" }]] as const) {
    await expect(demo.call(method, params)).rejects.toBeInstanceOf(RpcError);
  }
  const page = await demo.call<Page>("library.list", { kind: "tracks" });
  page.items[0]!.title = "Tampered";
  expect((await demo.call<Page>("library.list", { kind: "tracks" })).items[0]!.title).toBe("The Colour of Air");
});

test("demo radio retains its seed as playback advances and rejects podcast seeds", async () => {
  const demo = new DemoApi();
  const seed = structuredClone(demo.status.playable);
  await demo.call("radio.action", { action: "start" });
  await demo.call("player.action", { action: "next" });
  expect(demo.status.playable?.uri).not.toBe(seed?.uri);
  expect(await demo.call("radio.status")).toMatchObject({ active: true, seed: seed?.uri, seed_track: seed });
  await demo.call("radio.action", { action: "stop" });
  expect(await demo.call("radio.status")).toMatchObject({ active: false, seed: null, seed_track: null });
  await expect(demo.call("radio.action", { action: "start", uri: "spotify:episode:demo-1" })).rejects.toMatchObject({ code: "invalid_params" });
  expect(await demo.call("radio.status")).toMatchObject({ active: false, seed_track: null });
});
