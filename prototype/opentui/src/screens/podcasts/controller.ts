import type { Page, Row, RpcApi } from "../../workspace/contracts.js";

export interface PodcastsView {
  rows(rows: Row[], selectedId?: string): void;
  message(message: string): void;
  notify(message: string): void;
}

export class PodcastsController {
  rows: Row[] = [];
  show: Row | undefined;
  filter = "";
  offset = 0;
  page: Page | undefined;
  private generation = 0;
  private disposed = false;
  private busy = false;
  private cache = new Map<string, Page>();

  constructor(private api: RpcApi, private view: PodcastsView) {}

  async refresh(): Promise<void> {
    if (this.disposed) return;
    const generation = ++this.generation;
    const show = this.show;
    const key = JSON.stringify([show?.id, this.filter, this.offset]);
    const cached = this.cache.get(key);
    this.page = cached;
    this.rows = cached?.items ?? [];
    this.view.rows(this.rows);
    this.view.message(`${cached ? "Refreshing cached" : "Loading"} ${show ? "episodes" : "saved shows"}…`);
    try {
      const page = await this.api.call<Page>(show ? "library.detail" : "library.list", {
        kind: show ? "show" : "shows", ...(show ? { id: show.id, uri: show.uri } : { filter: this.filter }),
        offset: this.offset, limit: 50,
      });
      if (this.disposed || generation !== this.generation) return;
      const normalized = { ...page, items: page.items.map(row => show ? row : { ...row, saved: row.saved ?? true }) };
      this.cache.set(key, normalized);
      this.page = normalized;
      this.rows = normalized.items;
      this.view.rows(this.rows);
      this.view.message(this.rows.length
        ? `${show?.title ?? "Saved shows"} · ${page.offset + 1}–${page.offset + page.items.length} of ${page.total}${page.source ? ` · ${page.source}` : ""}`
        : show ? "No episodes available" : this.filter ? "No saved shows match your filter" : "No saved podcasts yet");
    } catch (error) {
      if (!this.disposed && generation === this.generation) {
        this.view.message(`Could not load ${show ? "episodes" : "shows"}: ${message(error)}${cached ? " · showing cached results" : ""}`);
      }
    }
  }

  open(row: Row): Promise<void> { this.show = row; this.offset = 0; return this.refresh(); }
  back(): Promise<void> { this.show = undefined; this.offset = 0; return this.refresh(); }
  search(filter: string): Promise<void> { this.filter = filter.trim(); this.offset = 0; return this.refresh(); }
  next(): Promise<void> {
    if (!this.page?.has_more) return Promise.resolve();
    this.offset = this.page.offset + this.page.items.length;
    return this.refresh();
  }
  previous(): Promise<void> { this.offset = Math.max(0, this.offset - 50); return this.refresh(); }

  async episodeAction(row: Row, action: "play" | "append" | "play_next"): Promise<void> {
    if (this.disposed || this.busy || row.kind !== "episode") return;
    const uri = row.uri || `spotify:episode:${row.id}`;
    await this.mutate(async () => {
      await this.api.call(action === "play" ? "player.action" : "queue.action", { action, uri });
      if (!this.disposed) this.view.notify(action === "play" ? `Playing ${row.title}` : `${row.title} ${action === "append" ? "added to queue" : "will play next"}`);
    });
  }

  async toggleSaved(row: Row): Promise<void> {
    if (this.disposed || this.busy || row.kind !== "show") return;
    await this.mutate(async () => {
      const saved = !(row.saved ?? true);
      await this.api.call("library.action", { action: saved ? "save" : "unsave", kind: "show", id: row.id, uri: row.uri });
      if (this.disposed) return;
      row.saved = saved;
      this.cache.clear();
      this.view.notify(saved ? "Show saved" : "Show removed from saved podcasts");
      await this.refresh();
    });
  }

  private async mutate(run: () => Promise<void>): Promise<void> {
    this.busy = true;
    try { await run(); }
    catch (error) { if (!this.disposed) { const text = `Podcast action failed: ${message(error)}`; this.view.message(text); this.view.notify(text); } }
    finally { this.busy = false; }
  }
  dispose(): void { this.disposed = true; ++this.generation; this.cache.clear(); }
}

function message(error: unknown): string { return error instanceof Error ? error.message : String(error); }
