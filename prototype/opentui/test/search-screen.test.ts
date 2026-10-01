import { expect, test } from "bun:test";
import type { Page, Params, Row, ScreenContext } from "../src/workspace/contracts.js";
import { cycleSearchKind, openResult, PageLoader, resultAction, SEARCH_KINDS, searchKind } from "../src/screens/search/model.js";

const row: Row = { id: "one", kind: "track", title: "One", subtitle: "Artist", uri: "spotify:track:one" };
const page = (title: string): Page => ({ items: [{ ...row, title }], offset: 0, limit: 20, total: 1, has_more: false, source: "spotify" });
function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason: unknown) => void;
  const promise = new Promise<T>((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
}
test("latest query wins and a disposed loader ignores late responses", async () => {
  const first = deferred<Page>(); const second = deferred<Page>();
  let count = 0; const shown: Page[] = [];
  const loader = new PageLoader({ call: async <T>() => await (++count === 1 ? first.promise : second.promise) as T }, (p) => shown.push(p), () => {});
  const a = loader.load("search", { query: "old" });
  const b = loader.load("search", { query: "new" });
  second.resolve(page("New")); await b;
  first.resolve(page("Old")); await a;
  expect(shown.at(-1)?.items[0]?.title).toBe("New");
  const pending = deferred<Page>(); const after: Page[] = [];
  const disposed = new PageLoader({ call: async <T>() => await pending.promise as T }, (p) => after.push(p), () => {});
  const request = disposed.load("search", { query: "gone" }); disposed.dispose();
  pending.resolve(page("Gone")); await request;
  expect(after.at(-1)?.items).toEqual([]);
});
test("refresh displays cached rows immediately and API errors expose retry", async () => {
  let fail = false; const shown: Page[] = []; const messages: string[] = [];
  const loader = new PageLoader({ call: async <T>() => {
    if (fail) throw new Error("rate_limited: wait 30 seconds");
    return page("Cached") as T;
  } }, (p) => shown.push(p), (m) => messages.push(m));
  await loader.load("search", { query: "one" }); fail = true;
  const retry = loader.load("search", { query: "one" });
  expect(shown.at(-1)?.items[0]?.title).toBe("Cached");
  expect(messages.at(-1)).toContain("Cached results");
  await retry;
  expect(messages.at(-1)).toContain("rate_limited");
  expect(messages.at(-1)).toContain("r retry");
});
test("clearing a query invalidates both stale success and stale error messages", async () => {
  const pending = deferred<Page>(); const messages: string[] = []; const shown: Page[] = [];
  const loader = new PageLoader({ call: async <T>() => await pending.promise as T }, (p) => shown.push(p), (m) => messages.push(m));
  const request = loader.load("search", { query: "old" });
  loader.invalidate(); pending.reject(new Error("stale failure")); await request;
  expect(shown).toHaveLength(1);
  expect(messages).toEqual(["Loading…"]);
});
test("backend cache appears first and requests one remote refresh when advertised", async () => {
  const remote = deferred<Page>(); const calls: Params[] = []; const shown: Page[] = []; const messages: string[] = [];
  const cached: Page = { ...page("Cached"), source: "local-search-cache", refresh_available: true };
  const loader = new PageLoader({ call: async <T>(_method: string, params?: Params) => {
    calls.push(params!);
    return (params?.refresh ? await remote.promise : cached) as T;
  } }, (p) => shown.push(p), (m) => messages.push(m));
  const request = loader.load("search", { query: "one", kind: "tracks", offset: 0, limit: 20 });
  await Promise.resolve();
  expect(shown.at(-1)?.items[0]?.title).toBe("Cached");
  expect(messages.at(-1)).toContain("local-search-cache");
  expect(calls).toHaveLength(2);
  expect(calls[1]).toEqual({ query: "one", kind: "tracks", offset: 0, limit: 20, refresh: true });
  // Even a refresh response advertising refresh availability cannot recurse.
  remote.resolve({ ...page("Fresh"), refresh_available: true }); await request;
  expect(shown.at(-1)?.items[0]?.title).toBe("Fresh");
  expect(calls).toHaveLength(2);
});
test("failed automatic and explicit remote refresh preserve cached rows", async () => {
  const remote = deferred<Page>(); const shown: Page[] = []; const messages: string[] = []; const calls: Params[] = [];
  const cached: Page = { ...page("Cached"), source: "local-search-cache", refresh_available: true };
  const loader = new PageLoader({ call: async <T>(_method: string, params?: Params) => {
    calls.push(params!);
    if (params?.refresh) return await remote.promise as T;
    return cached as T;
  } }, (p) => shown.push(p), (m) => messages.push(m));
  const request = loader.load("search", { query: "one" }); await Promise.resolve();
  remote.reject(new Error("network unavailable")); await request;
  expect(shown.at(-1)?.items[0]?.title).toBe("Cached");
  expect(messages.at(-1)).toContain("Showing cached results");
  expect(messages.at(-1)).toContain("network unavailable");
  await loader.load("search", { query: "one", refresh: true });
  expect(shown.at(-1)?.items[0]?.title).toBe("Cached");
  expect(calls).toHaveLength(3);
  expect(calls[2]?.refresh).toBe(true);
});
test("late automatic refresh cannot overwrite a newer submitted query", async () => {
  const remote = deferred<Page>(); const shown: Page[] = [];
  const loader = new PageLoader({ call: async <T>(_method: string, params?: Params) => {
    if (params?.query === "new") return page("New") as T;
    return (params?.refresh ? await remote.promise : { ...page("Old cached"), refresh_available: true }) as T;
  } }, (p) => shown.push(p), () => {});
  const oldRequest = loader.load("search", { query: "old" }); await Promise.resolve();
  await loader.load("search", { query: "new" });
  remote.resolve(page("Old remote")); await oldRequest;
  expect(shown.at(-1)?.items[0]?.title).toBe("New");
});
test("result selection maps playback, category, library, playlists and shows", async () => {
  const calls: [string, Params | undefined][] = [];
  const routes: [string, Params | undefined][] = [];
  const ctx = { api: { call: async (method: string, params?: Params) => { calls.push([method, params]); } }, navigate: (route: string, params?: Params) => routes.push([route, params]) } as unknown as ScreenContext;
  await openResult(ctx, row);
  expect(calls[0]).toEqual(["player.action", { action: "play", uri: row.uri }]);
  await openResult(ctx, { ...row, kind: "episode", uri: "spotify:episode:one" });
  expect(calls[1]).toEqual(["player.action", { action: "play", uri: "spotify:episode:one" }]);
  for (const kind of ["category", "album", "artist", "playlist", "show"]) await openResult(ctx, { ...row, kind });
  expect(routes.map(([route]) => route)).toEqual(["browse", "library", "library", "playlists", "podcasts"]);
  expect(routes[0]?.[1]?.id).toBe("one");
  expect(routes[1]?.[1]).toEqual({ kind: "album", id: "one", uri: row.uri, title: row.title });
  expect(routes[2]?.[1]).toEqual({ kind: "artist", id: "one", uri: row.uri, title: row.title });
  expect(() => openResult(ctx, { ...row, uri: undefined })).toThrow("no playable URI");
  expect(searchKind("unknown")).toBe("tracks");
});
test("search category cycling includes episodes and wraps in both directions", () => {
  expect(searchKind("episodes")).toBe("episodes");
  expect(cycleSearchKind("shows")).toBe("episodes");
  expect(cycleSearchKind("episodes")).toBe("tracks");
  expect(cycleSearchKind("tracks", true)).toBe("episodes");
  let kind = searchKind("tracks");
  const cycled = [kind];
  for (let index = 1; index < SEARCH_KINDS.length; index++) {
    kind = cycleSearchKind(kind); cycled.push(kind);
  }
  expect(cycled).toEqual([...SEARCH_KINDS]);
});
test("queue, save toggles and share use backend actions", async () => {
  const calls: [string, Params | undefined][] = []; const notifications: string[] = [];
  const ctx = { api: { call: async (method: string, params?: Params) => { calls.push([method, params]); return { url: "https://open.spotify.com/track/one" }; } }, notify: (message: string) => notifications.push(message) } as unknown as ScreenContext;
  const selected = { ...row };
  for (const action of ["append", "play_next", "save", "save", "share"] as const) await resultAction(ctx, selected, action);
  expect(calls.map(([method, params]) => [method, params?.action])).toEqual([["queue.action", "append"], ["queue.action", "play_next"], ["library.action", "save"], ["library.action", "unsave"], ["share", undefined]]);
  expect(selected.saved).toBe(false);
  expect(notifications.at(-1)).toBe("https://open.spotify.com/track/one");
});
