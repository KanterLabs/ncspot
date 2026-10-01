import { describe, expect, test } from "bun:test";
import { LibraryModel, type LibraryState } from "../src/screens/library/model.js";
import type { Page, RpcApi } from "../src/workspace/contracts.js";
import type { ScreenContext } from "../src/workspace/contracts.js";
import { createTestRenderer } from "@opentui/core/testing";
import { createLibraryScreen } from "../src/screens/library/index.js";

const page = (id: string, offset = 0, has_more = false): Page => ({
  items: [{ id, kind: "track", title: id, subtitle: "artist", uri: `spotify:track:${id}` }],
  offset, limit: 50, total: has_more ? 100 : 1, has_more, source: "cache",
});
function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason: Error) => void;
  const promise = new Promise<T>((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
}
const tick = () => new Promise(resolve => setTimeout(resolve, 0));

describe("library screen data and lifecycle", () => {
  test.each(["album", "albums", "artist", "artists"])("initial %s route opens the requested detail without listing tracks", async kind => {
    const setup = await createTestRenderer({ width: 80, height: 24 });
    const detailKind = kind.startsWith("album") ? "album" : "artist";
    const requests: Array<{ method: string; params: unknown }> = [];
    const context: ScreenContext = {
      renderer: setup.renderer,
      api: { call: async <T>(method: string, params: unknown) => {
        requests.push({ method, params });
        return page("Selected result track") as T;
      } },
      theme: () => "light", setTheme() {}, reducedMotion: () => true, setReducedMotion() {},
      status: () => null, onStatus: () => () => {}, navigate() {}, notify() {},
    };
    const screen = createLibraryScreen(context, { kind, id: "search-result", uri: `spotify:${detailKind}:search-result`, title: "Selected search result" });
    setup.renderer.root.add(screen.root);
    try {
      await tick(); await setup.renderOnce();
      expect(requests).toEqual([{ method: "library.detail", params: { kind: detailKind, id: "search-result", uri: `spotify:${detailKind}:search-result`, offset: 0, limit: 50 } }]);
      expect(setup.captureCharFrame()).toContain("Library › Selected search result");
      expect(setup.captureCharFrame()).toContain("Selected result track");
      expect(screen.handleKey({ name: "escape" })).toBe(true);
      await tick();
      expect(requests[1]).toEqual({ method: "library.list", params: { kind: `${detailKind}s`, offset: 0, limit: 50, filter: "", sort: "title" } });
    } finally { screen.dispose(); setup.renderer.destroy(); }
  });

  test("native 80x24 screen navigates album details and recolors both themes", async () => {
    const setup = await createTestRenderer({ width: 80, height: 24 });
    const requests: string[] = [];
    const context: ScreenContext = {
      renderer: setup.renderer,
      api: { call: async <T>(method: string) => {
        requests.push(method);
        return (method === "library.detail" ? page("Detail track") : {
          ...page("album"), items: [{ id: "album", kind: "album", title: "Saved album", subtitle: "Album artist", uri: "spotify:album:album" }],
        }) as T;
      } },
      theme: () => "light", setTheme() {}, reducedMotion: () => true, setReducedMotion() {},
      status: () => null, onStatus: () => () => {}, navigate() {}, notify() {},
    };
    const screen = createLibraryScreen(context, { kind: "albums" });
    setup.renderer.root.add(screen.root);
    try {
      await tick(); await setup.renderOnce();
      expect(setup.captureCharFrame()).toContain("Saved album");
      expect(setup.captureCharFrame()).toContain("[albums]");
      screen.handleKey({ name: "return" });
      await tick(); await setup.renderOnce();
      expect(setup.captureCharFrame()).toContain("Detail track");
      expect(requests).toContain("library.detail");
      screen.setTheme("dark");
      await setup.renderOnce();
      expect(setup.captureCharFrame()).toContain("Detail track");
      expect(screen.handleKey({ name: "escape" })).toBe(true);
      await tick(); await setup.renderOnce();
      expect(setup.captureCharFrame()).toContain("Saved album");
      screen.handleKey({ name: "/", sequence: "/" });
      expect(screen.editing?.()).toBe(true);
      screen.dispose(); screen.dispose();
      expect(screen.root.isDestroyed).toBe(true);
    } finally { screen.dispose(); setup.renderer.destroy(); }
  });

  test("maps categories, filtering, sorting and server pagination", async () => {
    const requests: Array<{ method: string; params: unknown }> = [];
    const api = { call: async (method: string, params: unknown) => {
      requests.push({ method, params }); return page("one", 0, true);
    } } as RpcApi;
    const model = new LibraryModel(api, () => {}, () => {});
    await model.load();
    model.category("albums"); await tick();
    model.filter("Björk"); await tick();
    model.sort(); await tick();
    expect(model.page(1)).toBe(true); await tick();
    expect(requests[0]).toEqual({ method: "library.list", params: { kind: "tracks", offset: 0, limit: 50, filter: "", sort: "title" } });
    expect(requests.at(-1)).toEqual({ method: "library.list", params: { kind: "albums", offset: 50, limit: 50, filter: "Björk", sort: "artist" } });
    model.dispose();
  });

  test("late filter responses cannot replace current results", async () => {
    const first = deferred<Page>(); const second = deferred<Page>();
    let calls = 0;
    const api = { call: () => (++calls === 1 ? first.promise : second.promise) } as RpcApi;
    const model = new LibraryModel(api, () => {}, () => {});
    const initial = model.load();
    model.filter("new");
    second.resolve(page("new")); await tick();
    first.resolve(page("old")); await initial;
    expect(model.state.page?.items[0].id).toBe("new");
    expect(model.state.view.filter).toBe("new");
    model.dispose();
  });

  test("detail requests and back preserve selection and cached rows", async () => {
    const requests: Array<{ method: string; params: unknown }> = [];
    const snapshots: LibraryState[] = [];
    const api = { call: async (method: string, params: unknown) => {
      requests.push({ method, params }); return page(method);
    } } as RpcApi;
    const model = new LibraryModel(api, state => snapshots.push(state), () => {}, "albums");
    await model.load(); model.select("selected-album");
    model.open({ id: "album-id", kind: "album", title: "Album", subtitle: "Artist", uri: "spotify:album:album-id" });
    await tick();
    expect(requests[1]).toEqual({ method: "library.detail", params: { kind: "album", id: "album-id", uri: "spotify:album:album-id", offset: 0, limit: 50 } });
    expect(model.back()).toBe(true);
    expect(snapshots.at(-1)?.loading).toBe(true);
    expect(snapshots.at(-1)?.page?.items[0].id).toBe("library.list");
    await tick();
    expect(model.state.view.selectedId).toBe("selected-album");
    expect(model.back()).toBe(false);
    model.dispose();
  });

  test("disposed requests and actions cannot update screen or notifications", async () => {
    const pending = deferred<Page>(); let updates = 0; let notifications = 0;
    const api = { call: () => pending.promise } as RpcApi;
    const model = new LibraryModel(api, () => { updates++; }, () => { notifications++; });
    const loading = model.load();
    const action = model.action("library.action", { action: "save", id: "one" }, "Saved", true);
    model.dispose(); pending.resolve(page("late"));
    await Promise.all([loading, action]);
    expect(updates).toBe(1); expect(notifications).toBe(0);
  });

  test("loading error keeps cached results visible", async () => {
    let fail = false;
    const api = { call: async () => { if (fail) throw new Error("offline"); return page("cached"); } } as RpcApi;
    const model = new LibraryModel(api, () => {}, () => {});
    await model.load(); fail = true; await model.load();
    expect(model.state.page?.items[0].id).toBe("cached");
    expect(model.state.error).toBe("offline");
    model.dispose();
  });
});
