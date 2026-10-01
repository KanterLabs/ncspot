import type { Page, Params, Row, RpcApi, ScreenContext } from "../../workspace/contracts.js";

export const SEARCH_KINDS = ["tracks", "albums", "artists", "playlists", "shows", "episodes"] as const;
export type SearchKind = typeof SEARCH_KINDS[number];
export function searchKind(value: unknown): SearchKind {
  return SEARCH_KINDS.includes(value as SearchKind) ? value as SearchKind : "tracks";
}
export function cycleSearchKind(kind: SearchKind, backwards = false): SearchKind {
  return SEARCH_KINDS[(SEARCH_KINDS.indexOf(kind) + (backwards ? SEARCH_KINDS.length - 1 : 1)) % SEARCH_KINDS.length]!;
}

export function errorMessage(error: unknown): string {
  if (error instanceof Error) return error.message;
  if (typeof error === "object" && error && "message" in error) return String(error.message);
  return String(error);
}

/** One cache per mounted controller, with responses guarded against disposal and navigation. */
export class PageLoader {
  private generation = 0;
  private disposed = false;
  private cache = new Map<string, Page>();
  constructor(private api: RpcApi, private rows: (page: Page) => void, private message: (message: string) => void) {}
  async load(method: string, params: Params): Promise<void> {
    if (this.disposed) return;
    const generation = ++this.generation;
    const { refresh: _refresh, ...cacheParams } = params;
    const key = JSON.stringify([method, cacheParams]);
    let cached = this.cache.get(key);
    if (cached) this.rows(cached);
    else this.rows({ items: [], offset: Number(params.offset ?? 0), limit: Number(params.limit ?? 20), total: 0, has_more: false });
    this.message(cached ? `Cached results • ${cached.source ?? "local cache"} • refreshing…` : "Loading…");
    try {
      let page = await this.api.call<Page>(method, params);
      if (this.disposed || generation !== this.generation) return;
      this.cache.set(key, page);
      this.rows(page);
      // The backend can return its local cache immediately while allowing one
      // remote refresh. Keep those rows mounted while that request is pending.
      if (method === "search" && page.refresh_available && params.refresh !== true) {
        cached = page;
        this.message(`Cached results • ${page.source ?? "backend cache"} • refreshing…`);
        page = await this.api.call<Page>(method, { ...params, refresh: true });
        if (this.disposed || generation !== this.generation) return;
        this.cache.set(key, page);
        this.rows(page);
      }
      const first = page.items.length ? page.offset + 1 : 0;
      this.message(`${page.source ?? "backend"} • ${first}–${page.offset + page.items.length} of ${page.total}${page.has_more ? " • ] next page" : ""}${page.items.length ? "" : " • No results"}`);
    } catch (error) {
      if (this.disposed || generation !== this.generation) return;
      this.message(`${cached ? `Showing cached results • ${cached.source ?? "local cache"} • ` : ""}Request failed: ${errorMessage(error)} • r retry`);
    }
  }
  invalidate(): void { ++this.generation; }
  dispose(): void { this.disposed = true; ++this.generation; this.cache.clear(); }
}

export function openResult(ctx: ScreenContext, row: Row): Promise<unknown> | void {
  if (row.kind === "track" || row.kind === "episode") {
    if (!row.uri) throw new Error("This result has no playable URI");
    return ctx.api.call("player.action", { action: "play", uri: row.uri });
  }
  const params = { kind: row.kind, id: row.id, uri: row.uri, title: row.title };
  if (row.kind === "playlist") ctx.navigate("playlists", params);
  else if (row.kind === "show") ctx.navigate("podcasts", params);
  else if (row.kind === "category") ctx.navigate("browse", params);
  else ctx.navigate("library", params);
}

export async function resultAction(ctx: ScreenContext, row: Row, action: "append" | "play_next" | "save" | "share"): Promise<void> {
  if (action === "save") {
    await ctx.api.call("library.action", { action: row.saved ? "unsave" : "save", kind: row.kind, id: row.id, uri: row.uri });
    row.saved = !row.saved;
    ctx.notify(row.saved ? "Saved to library" : "Removed from library");
  } else {
    if (!row.uri) throw new Error("This result has no URI");
    if (action === "share") {
      const result = await ctx.api.call<{ url: string }>("share", { uri: row.uri });
      ctx.notify(result.url);
    } else {
      await ctx.api.call("queue.action", { action, uri: row.uri });
      ctx.notify(action === "append" ? "Added to queue" : "Will play next");
    }
  }
}
