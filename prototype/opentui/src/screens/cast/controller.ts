import type { Page, Row, RpcApi } from "../../workspace/contracts.js";

export interface CastView {
  selected(): Row | undefined;
  rows(rows: Row[], selectedId?: string): void;
  message(message: string, error?: boolean): void;
  notify(message: string): void;
}

const errorMessage = (error: unknown): string => error instanceof Error ? error.message
  : error && typeof error === "object" && "message" in error ? String(error.message) : String(error);

/** Device IDs are opaque: Connect IDs and Roku addresses both go to cast.action. */
export class CastController {
  rows: Row[] = [];
  private pending?: Promise<void>;
  private busy = false;
  private disposed = false;

  constructor(private api: RpcApi, private view: CastView) {}

  refresh(): Promise<void> {
    if (this.disposed) return Promise.resolve();
    if (this.pending) return this.pending;
    if (!this.busy) this.view.message("Discovering Spotify Connect devices and Rokus…");
    this.pending = this.load().finally(() => { this.pending = undefined; });
    return this.pending;
  }

  private async load(): Promise<void> {
    const selectedId = this.view.selected()?.id;
    try {
      const rows: Row[] = [];
      let offset = 0;
      while (true) {
        const page = await this.api.call<Page>("cast.list", { offset, limit: 200 });
        if (this.disposed) return;
        rows.push(...page.items);
        if (!page.has_more) break;
        const nextOffset = page.offset + page.items.length;
        if (nextOffset <= offset) throw new Error("Device discovery returned an incomplete page");
        offset = nextOffset;
      }
      this.rows = rows;
      this.view.rows(rows.map(decorateDevice), this.view.selected()?.id ?? selectedId);
      const active = rows.find(row => row.meta?.active === true);
      this.view.message(active ? `Casting to ${active.title} · d to return playback locally`
        : !rows.length ? "No devices found · Open Spotify on a device, then press r"
        : rows.some(row => typeof row.meta?.active === "boolean") ? `${rows.length} devices · Playback is local`
        : `${rows.length} devices · Select a device to connect`);
    } catch (error) {
      if (!this.disposed) this.view.message(`Device discovery failed: ${errorMessage(error)}`, true);
    }
  }

  connect(row = this.view.selected()): Promise<void> {
    if (!row || this.disposed || this.busy) return Promise.resolve();
    if (row.meta?.supported === false || row.meta?.connectable === false || row.meta?.restricted === true || row.meta?.spotify === "missing") {
      const message = typeof row.meta?.reason === "string" ? row.meta.reason : `${row.title} cannot accept playback; check its Spotify app and device permissions`;
      this.view.message(message, true);
      this.view.notify(message);
      return Promise.resolve();
    }
    return this.action("connect", row);
  }

  disconnect(): Promise<void> { return this.action("disconnect"); }

  private async action(action: "connect" | "disconnect", row?: Row): Promise<void> {
    if (this.disposed || this.busy) return;
    this.busy = true;
    this.view.message(action === "disconnect" ? "Returning playback locally…"
      : row?.kind === "roku" ? `Opening Spotify on ${row.title} · waiting for Spotify Connect…` : `Connecting to ${row?.title}…`);
    try {
      if (this.pending) await this.pending;
      if (this.disposed) return;
      await this.api.call("cast.action", action === "connect" ? { action, id: row!.id } : { action });
      if (this.disposed) return;
      await this.refresh();
      if (!this.disposed) this.view.notify(action === "disconnect" ? "Playback returned locally" : `Connection requested for ${row!.title}`);
    } catch (error) {
      if (this.disposed) return;
      await this.refresh();
      if (this.disposed) return;
      const message = `Cast ${action} failed: ${errorMessage(error)}`;
      this.view.message(message, true);
      this.view.notify(message);
    } finally { this.busy = false; }
  }

  dispose(): void { this.disposed = true; }
}

function decorateDevice(row: Row): Row {
  const type = typeof row.meta?.type === "string" ? row.meta.type : row.subtitle || (row.kind === "roku" ? "Roku" : "Spotify Connect");
  const unavailable = row.meta?.supported === false || row.meta?.connectable === false || row.meta?.restricted === true || row.meta?.spotify === "missing";
  return { ...row, subtitle: type, detail: row.meta?.active === true ? "Connected" : unavailable ? "Unavailable" : row.detail ?? (row.kind === "roku" ? "Opens Spotify app" : "") };
}
