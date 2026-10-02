// Runs the actual Rust RPC server and every native OpenTUI screen without Spotify credentials.
// Usage: bun run test/engine-contract.ts [path to the Rust test executable]
import assert from "node:assert/strict";
import { existsSync, mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { createTestRenderer } from "@opentui/core/testing";
import { WorkspaceClient, RpcError } from "../src/workspace/client.js";
import { mountWorkspace } from "../src/workspace/app.js";
import type { ParsedStatus } from "../src/status.js";
import { ROUTES, type Page } from "../src/workspace/contracts.js";

const delay = (ms: number) => new Promise(resolve => setTimeout(resolve, ms));
let executable = Bun.argv[2];
if (!executable) {
  const build = Bun.spawn(["cargo", "test", "--locked", "--bin", "resonance", "--no-run", "--message-format=json"], { cwd: resolve(import.meta.dir, "../../.."), stdout: "pipe", stderr: "inherit" });
  const records = await new Response(build.stdout).text();
  assert.equal(await build.exited, 0, "Rust fixture compilation failed");
  for (const line of records.split("\n")) {
    try { const value = JSON.parse(line); if (value.reason === "compiler-artifact" && value.target?.name === "resonance" && value.profile?.test && value.executable) executable = value.executable; } catch {}
  }
}
assert.ok(executable, "Rust test executable was not found");
const temporary = mkdtempSync(join(tmpdir(), "resonance-contract-"));
const socket = join(temporary, "engine.sock");
const fixture = Bun.spawn([executable, "--ignored", "--exact", "ipc::tests::opentui_frontend_fixture", "--nocapture"], { env: { ...process.env, RESONANCE_TEST_SOCKET: socket }, stdout: "pipe", stderr: "pipe" });
const stdout = new Response(fixture.stdout).text();
const stderr = new Response(fixture.stderr).text();
let broadcasts = 0;
let lastStatus: ParsedStatus | undefined;
let app: ReturnType<typeof mountWorkspace> | undefined;
let renderer: Awaited<ReturnType<typeof createTestRenderer>>["renderer"] | undefined;
const client = new WorkspaceClient(socket, { onStatus(status) { lastStatus = status; broadcasts++; app?.setStatus(status); } }, 5000);
try {
  const deadline = Date.now() + 15000;
  while (!existsSync(socket) && Date.now() < deadline && fixture.exitCode === null) await delay(20);
  assert.ok(existsSync(socket), "Fixture socket did not become ready");
  await client.connect();
  const session = await client.call<{ capabilities: string[] }>("session.info");
  assert.ok(session.capabilities.includes("queue.action"));
  const [tracks, albums, artists, playlists, shows] = await Promise.all(["tracks", "albums", "artists", "playlists", "shows"].map(kind => client.call<Page>("library.list", { kind })));
  assert.equal(tracks!.items.length, 2); assert.equal(albums!.items[0]?.saved, true);
  assert.equal(artists!.items[0]?.saved, true); assert.equal(shows!.items[0]?.saved, true);
  assert.equal(playlists!.items[0]?.title, "Fixture Duplicates");
  const detail = await client.call<Page>("library.detail", { kind: "playlist", id: playlists!.items[0]!.id });
  assert.equal(detail.revision, "fixture-snapshot-1");
  assert.equal(detail.items.length, 3); assert.equal(detail.items[0]!.uri, detail.items[1]!.uri);
  const episodes = await client.call<Page>("library.detail", { kind: "show", id: shows!.items[0]!.id });
  assert.equal(episodes.items[0]?.kind, "episode");
  const search = await client.call<Page>("search", { query: "Fixture", kind: "tracks" });
  assert.equal(search.source, "cache"); assert.equal(search.refresh_available, true);
  await assert.rejects(client.call("search", { query: "Fixture", kind: "tracks", refresh: true }), error => error instanceof RpcError && error.code === "upstream_error");
  let queue = await client.call<Page>("queue.list");
  assert.equal(queue.items.length, 3); assert.notEqual(queue.items[0]!.id, queue.items[1]!.id);
  await client.call("queue.action", { action: "remove", entry_id: queue.items[1]!.id, revision: queue.revision });
  await assert.rejects(client.call("queue.action", { action: "remove", entry_id: queue.items[0]!.id, revision: queue.revision }), error => error instanceof RpcError && error.code === "stale_queue");
  queue = await client.call<Page>("queue.list"); assert.equal(queue.items.length, 2);
  await client.call("player.action", { action: "play", uri: playlists!.items[0]!.uri });
  queue = await client.call<Page>("queue.list"); assert.equal(queue.items.length, 5);
  assert.equal(queue.items[2]?.meta?.current, true);
  await client.call("radio.action", { action: "discovery", value: 73 });
  const radio = await client.call<{ discovery: number }>("radio.status"); assert.equal(radio.discovery, 73);
  await client.call("radio.action", { action: "start" });
  const stationDeadline = Date.now() + 5000;
  let station = await client.call<{ radio_pending_count: number; parked_count: number; queue_mode: string }>("radio.status");
  while (!station.radio_pending_count && Date.now() < stationDeadline) {
    await delay(20);
    station = await client.call<typeof station>("radio.status");
  }
  assert.equal(station.queue_mode, "station");
  assert.ok(station.parked_count >= 2, "previous playlist context was not retained");
  assert.equal(station.radio_pending_count, 1, "cached related track was blocked by parked context");
  await client.call("queue.action", { action: "append", uri: "spotify:track:FixtureTrackAlpha00001" });
  const statusDeadline = Date.now() + 5000;
  while (!lastStatus?.prototype?.up_next_origins?.includes("explicit") && Date.now() < statusDeadline) await delay(20);
  assert.deepEqual(lastStatus?.prototype?.up_next_origins, ["explicit", "radio"]);
  assert.equal(lastStatus?.prototype?.up_next[0]?.title, "Fixture Alpha");
  assert.equal(lastStatus?.prototype?.up_next[1]?.title, "Fixture Beta");
  await client.call("radio.action", { action: "stop" });
  await assert.rejects(client.call("player.action", { action: "volume", value: 101 }), error => error instanceof RpcError && error.code === "invalid_params");
  await assert.rejects(client.call("unknown.method"), error => error instanceof RpcError && error.code === "unknown_method");

  const native = await createTestRenderer({ width: 80, height: 24 }); renderer = native.renderer;
  app = mountWorkspace(renderer, { api: client, theme: "light", reducedMotion: true });
  for (const theme of ["light", "dark"] as const) {
    app.context.setTheme(theme);
    for (const route of ROUTES) {
      app.navigate(route); await app.refresh(); await delay(20); await native.renderOnce();
      const frame = native.captureCharFrame();
      assert.ok(frame.includes("RESONANCE"), `${route} missing workspace chrome`);
      assert.equal(app.route, route);
      // Browse/Cast deliberately use the offline API and render honest unavailable/empty states.
      if (route === "queue" || route === "library") assert.ok(frame.includes("Fixture Alpha"), `${route} failed to display Rust data`);
    }
  }
  // A failed automatic search refresh must not prevent the native Enter key
  // from playing the selected cached result through the actual Rust engine.
  app.navigate("search", { query: "Fixture" });
  await app.refresh(); await delay(20); await native.renderOnce();
  assert.ok(native.captureCharFrame().includes("Showing cached results"));
  native.mockInput.pressArrow("down");
  native.mockInput.pressEnter();
  const playedDeadline = Date.now() + 5000;
  let played = await client.call<{ current?: { uri?: string } }>("player.status");
  while (played.current?.uri !== "spotify:track:FixtureTrackBeta000002" && Date.now() < playedDeadline) {
    await delay(20);
    played = await client.call<typeof played>("player.status");
  }
  assert.equal(played.current?.uri, "spotify:track:FixtureTrackBeta000002", "native Enter did not play the selected cached search result");

  // Quick search must reach the same real queue and player paths while the
  // Now Playing screen remains mounted. Explicit queue choices may repeat.
  app.navigate("now-playing");
  async function quickSearch() {
    await native.mockInput.typeText("/");
    await native.mockInput.typeText("Fixture");
    const deadline = Date.now() + 5000;
    do {
      await delay(20); await native.renderOnce();
    } while (!renderer!.root.findDescendantById("np-quick-search-row-1")?.visible && Date.now() < deadline);
    assert.ok(renderer!.root.findDescendantById("np-quick-search-row-1")?.visible, "quick search did not render cached results");
    assert.equal(app!.route, "now-playing");
  }
  async function waitQueueSize(size: number) {
    const deadline = Date.now() + 5000;
    let page = await client.call<Page>("queue.list");
    while (page.items.length !== size && Date.now() < deadline) {
      await delay(20); page = await client.call<Page>("queue.list");
    }
    assert.equal(page.items.length, size);
    await delay(20);
    return page;
  }
  const beforeQuick = await client.call<Page>("queue.list");
  await quickSearch();
  native.mockInput.pressKey("n", { ctrl: true });
  const withNext = await waitQueueSize(beforeQuick.items.length + 1);
  const currentIndex = withNext.items.findIndex(row => row.meta?.current);
  assert.equal(withNext.items[currentIndex + 1]?.uri, "spotify:track:FixtureTrackAlpha00001", "quick play next did not insert immediately after current");
  assert.equal((await client.call<typeof played>("player.status")).current?.uri, played.current?.uri, "play next interrupted current playback");

  await quickSearch();
  native.mockInput.pressArrow("down");
  native.mockInput.pressKey("e", { ctrl: true });
  const withAppend = await waitQueueSize(withNext.items.length + 1);
  assert.equal(withAppend.items.at(-1)?.uri, "spotify:track:FixtureTrackBeta000002", "quick queue did not append selected track");
  assert.equal((await client.call<typeof played>("player.status")).current?.uri, played.current?.uri, "queue append interrupted current playback");

  await quickSearch();
  native.mockInput.pressEnter();
  const quickPlayDeadline = Date.now() + 5000;
  let quickPlayed = await client.call<typeof played>("player.status");
  while (quickPlayed.current?.uri !== "spotify:track:FixtureTrackAlpha00001" && Date.now() < quickPlayDeadline) {
    await delay(20); quickPlayed = await client.call<typeof played>("player.status");
  }
  assert.equal(quickPlayed.current?.uri, "spotify:track:FixtureTrackAlpha00001", "quick Enter did not play selected track now");
  assert.equal(app.route, "now-playing");
  assert.ok(broadcasts > 1, "Status broadcasting stalled during RPC requests");
  app.dispose(); renderer.destroy(); renderer = undefined;
  await client.call("settings.action", { action: "command", command: "quit" });
  assert.equal(await fixture.exited, 0, await stderr);
  assert.ok(!existsSync(socket), "Server socket leaked after shutdown");
  console.log("Engine contract passed: real Rust RPC, cached search Enter, quick-search play now/next/queue, isolated radio, explicit priority, context playback, stale edits, status broadcasts, all OpenTUI routes in both themes.");
} catch (error) { console.error(await Promise.race([stderr, delay(10).then(() => "")])); throw error; }
finally {
  app?.dispose(); renderer?.destroy(); client.close();
  if (fixture.exitCode === null) fixture.kill();
  await fixture.exited; await stdout; await stderr; rmSync(temporary, { recursive: true, force: true });
}
