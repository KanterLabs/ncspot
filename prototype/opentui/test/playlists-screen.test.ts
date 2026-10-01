import { expect, test } from "bun:test";
import { InputRenderable, type Renderable } from "@opentui/core";
import { createTestRenderer } from "@opentui/core/testing";
import { createPlaylistsScreen } from "../src/screens/playlists/index.js";
import type { Page, Params, Row, ScreenContext } from "../src/workspace/contracts.js";

const playlist: Row = { id: "playlist-id", kind: "playlist", title: "Night drive", subtitle: "Shane", uri: "spotify:playlist:playlist-id", meta: { owned: true } };
const track: Row = { id: "track-entry", kind: "track", title: "Midnight", subtitle: "Artist", uri: "spotify:track:track-id", meta: { index: 7 } };
const page = (items: Row[]): Page => ({ items, offset: 0, limit: 30, total: items.length, has_more: false });
const tick = () => new Promise(resolve => setTimeout(resolve, 0));
function input(root: Renderable): InputRenderable | undefined {
  if (root instanceof InputRenderable) return root;
  for (const child of root.getChildren()) { const found = input(child); if (found) return found; }
}
async function fixture(params: Params = {}) {
  const setup = await createTestRenderer({ width: 80, height: 24 });
  const calls: { method: string; params: Params }[] = [];
  let fail = false;
  const context: ScreenContext = {
    renderer: setup.renderer,
    api: { async call<T>(method: string, params: Params = {}) {
      calls.push({ method, params });
      if (fail && method === "playlist.action") throw new Error("Permission denied by service");
      return (method === "library.detail" ? { ...page([track]), revision: "snapshot-current" } : method === "library.list" ? page([playlist]) : {}) as T;
    } },
    theme: () => "dark", setTheme() {}, reducedMotion: () => true, setReducedMotion() {}, status: () => null,
    onStatus: () => () => {}, navigate() {}, notify() {},
  };
  const screen = createPlaylistsScreen(context, params);
  setup.renderer.root.add(screen.root);
  await tick();
  return { setup, screen, context, calls, fail: () => { fail = true; }, submit(value: string) { const node = input(screen.root); expect(node).toBeDefined(); node!.emit("enter", value); }, cleanup() { screen.dispose(); setup.renderer.destroy(); } };
}

test("playlists open details and play actual URI with contextual compact rendering", async () => {
  const f = await fixture();
  try {
    f.screen.handleKey({ name: "return" }); await tick();
    expect(f.calls.at(-1)).toEqual({ method: "library.detail", params: { kind: "playlist", id: playlist.id, uri: playlist.uri, offset: 0, limit: 30 } });
    expect(f.screen.title).toBe("Playlists / Night drive");
    await f.setup.renderOnce();
    expect(f.setup.captureCharFrame()).toContain("Midnight");
    f.screen.handleKey({ name: "return" }); await tick();
    expect(f.calls).toContainEqual({ method: "player.action", params: { action: "play", uri: track.uri } });
    expect(f.screen.handleKey({ name: "q" })).toBe(false);
    f.screen.handleKey({ name: "u" }); await tick();
    expect(f.calls).toContainEqual({ method: "queue.action", params: { action: "append", uri: track.uri } });
    f.screen.handleKey({ name: "escape" }); await tick();
    expect(f.screen.title).toBe("Playlists");
    f.screen.setTheme("light");
  } finally { f.cleanup(); }
});

test("create, add and remove use prompts and canonical track position", async () => {
  const f = await fixture();
  try {
    f.screen.handleKey({ name: "c" }); expect(f.screen.editing?.()).toBe(true);
    f.submit("Fresh playlist"); await tick();
    expect(f.calls).toContainEqual({ method: "playlist.action", params: { action: "create", name: "Fresh playlist" } });
    f.screen.handleKey({ name: "return" }); await tick();
    f.screen.handleKey({ name: "a" }); f.submit(track.uri!); await tick();
    expect(f.calls).toContainEqual({ method: "playlist.action", params: { action: "add", id: playlist.id, uri: track.uri } });
    f.screen.handleKey({ name: "d" });
    expect(f.calls.filter(c => c.params.action === "remove")).toHaveLength(0);
    f.screen.handleKey({ name: "y" }); await tick();
    expect(f.calls).toContainEqual({ method: "playlist.action", params: { action: "remove", id: playlist.id, position: 7, revision: "snapshot-current", uri: track.uri } });
  } finally { f.cleanup(); }
});

test("mutation errors stay visible and playlist deletion requires confirmation", async () => {
  const f = await fixture();
  try {
    f.fail();
    f.screen.handleKey({ name: "r" }); f.submit("Other name"); await tick();
    await f.setup.renderOnce();
    expect(f.setup.captureCharFrame()).toContain("Permission denied by service");
    f.screen.handleKey({ name: "d" }); f.screen.handleKey({ name: "n" }); await tick();
    expect(f.calls.some(c => c.params.action === "delete")).toBe(false);
    f.screen.handleKey({ name: "d" }); f.screen.handleKey({ name: "y" }); await tick();
    expect(f.calls).toContainEqual({ method: "playlist.action", params: { action: "delete", id: playlist.id } });
  } finally { f.cleanup(); }
});

test("direct detail accepts an id without inventing a URI and disposal ignores pending requests", async () => {
  const f = await fixture({ id: "only-id" });
  try {
    expect(f.calls[0]?.params).toEqual({ kind: "playlist", id: "only-id", offset: 0, limit: 30 });
    f.screen.handleKey({ name: "p" });
    expect(f.calls.some(c => c.method === "player.action")).toBe(false);
    f.screen.dispose(); f.screen.dispose();
    await f.screen.refresh();
  } finally { f.cleanup(); }
});


test("late detail responses cannot replace a newer browser view or revive a disposed screen", async () => {
  const f = await fixture();
  try {
    const pending: ((value: Page) => void)[] = [];
    const original = f.context.api.call.bind(f.context.api);
    f.context.api.call = <T>(method: string, params?: Params): Promise<T> => method === "library.detail"
      ? new Promise<T>(resolve => { pending.push(value => resolve(value as T)); })
      : original<T>(method, params);
    f.screen.handleKey({ name: "return" });
    f.screen.handleKey({ name: "escape" }); await tick();
    pending.shift()!(page([track])); await tick();
    await f.setup.renderOnce();
    expect(f.setup.captureCharFrame()).toContain("Night drive");
    expect(f.setup.captureCharFrame()).not.toContain("Midnight");
    f.screen.handleKey({ name: "return" });
    f.screen.dispose();
    pending.shift()!(page([track])); await tick();
    const count = f.calls.length;
    await f.screen.refresh();
    expect(f.calls.length).toBe(count);
  } finally { f.cleanup(); }
});
