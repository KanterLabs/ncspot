import type { Page, Params, Row, RpcApi } from "../../workspace/contracts.js";

export interface QueueView {
  selected(): Row | undefined;
  rows(rows: Row[], selectedId?: string): void;
  message(message: string): void;
  notify(message: string): void;
}

/** Queue IDs identify occurrences, including multiple copies of the same URI. */
export class QueueController {
  rows: Row[] = [];
  revision: string | undefined;
  private pending: Promise<void> | undefined;
  private busy = false;
  private disposed = false;

  constructor(private api: RpcApi, private view: QueueView) {}

  refresh(): Promise<void> {
    if (this.disposed) return Promise.resolve();
    if (this.pending) return this.pending;
    this.pending = this.load().finally(() => { this.pending = undefined; });
    return this.pending;
  }

  private async load(): Promise<void> {
    const selected = this.view.selected();
    const oldIndex = Math.max(0, this.rows.findIndex(row => row.id === selected?.id));
    try {
      // Fetch every page so the native list can scroll through the entire queue.
      // A queue changed between pages is retried from its first page.
      for (let attempt = 0; attempt < 3; attempt++) {
        const rows: Row[] = [];
        let offset = 0;
        let revision: string | undefined;
        let inconsistent = false;
        while (true) {
          const page = await this.api.call<Page>("queue.list", { offset, limit: 100 });
          if (this.disposed) return;
          if (offset === 0) revision = page.revision;
          else if (page.revision !== revision) { inconsistent = true; break; }
          rows.push(...page.items);
          if (!page.has_more) break;
          if (!page.items.length) throw new Error("Queue returned an empty page before its end");
          offset = page.offset + page.items.length;
        }
        if (inconsistent) continue;
        this.rows = rows;
        this.revision = revision;
        const currentSelection = this.view.selected()?.id ?? selected?.id;
        const selectedId = rows.some(row => row.id === currentSelection)
          ? currentSelection : rows[Math.min(oldIndex, rows.length - 1)]?.id;
        this.view.rows(rows, selectedId);
        this.view.message(rows.length ? `${rows.length} queue entries · ▶ current` : "Your queue is empty · a to append a Spotify URI");
        return;
      }
      throw new Error("Queue kept changing during refresh; refresh again");
    } catch (error) {
      if (!this.disposed) this.view.message(`Queue refresh failed: ${errorMessage(error)}`);
    }
  }

  async action(action: string, params: Params = {}): Promise<void> {
    if (this.disposed || this.busy) return;
    this.busy = true;
    try {
      if (this.pending) await this.pending;
      if (!this.revision) {
        await this.refresh();
        if (!this.revision) throw new Error("Queue revision unavailable; refresh before editing");
      }
      if (this.disposed) return;
      await this.api.call("queue.action", { action, ...params, revision: this.revision });
      if (this.disposed) return;
      if (this.pending) await this.pending;
      await this.refresh();
      if (!this.disposed) this.view.notify(`Queue ${action === "save" ? "saved" : "updated"}`);
    } catch (error) {
      if (this.disposed) return;
      // Never repeat a failed mutation: its old identity/revision is no longer safe.
      if (this.pending) await this.pending;
      await this.refresh();
      if (!this.disposed) {
        const message = `Queue action failed: ${errorMessage(error)} · refreshed; try again`;
        this.view.message(message);
        this.view.notify(message);
      }
    } finally { this.busy = false; }
  }

  selectedAction(action: "play" | "remove"): void {
    const row = this.view.selected();
    if (row) void this.action(action, { entry_id: row.id });
  }

  moveSelected(delta: number): void {
    const row = this.view.selected();
    if (!row) return;
    const index = typeof row.meta?.index === "number" ? row.meta.index : this.rows.findIndex(item => item.id === row.id);
    const to = index + delta;
    if (to < 0 || to >= this.rows.length) return;
    void this.action("move", { entry_id: row.id, to });
  }

  dispose(): void { this.disposed = true; }
}

function errorMessage(error: unknown): string {
  if (error instanceof Error) return error.message;
  if (error && typeof error === "object" && "message" in error) return String(error.message);
  return String(error);
}
