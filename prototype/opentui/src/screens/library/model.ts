import type { Page, Row, RpcApi } from "../../workspace/contracts.js";

export type LibraryKind = "tracks" | "albums" | "artists";
export interface LibraryView {
  kind: LibraryKind | string;
  detail?: Row;
  offset: number;
  filter: string;
  sort: string;
  selectedId?: string;
}
export interface LibraryState {
  view: LibraryView;
  page?: Page;
  loading: boolean;
  error?: string;
}
export const LIBRARY_KINDS: LibraryKind[] = ["tracks", "albums", "artists"];
export const LIBRARY_SORTS = ["title", "artist", "added"];
export const LIBRARY_PAGE_SIZE = 50;

/** Keeps request generations and navigation independent of renderable lifetimes. */
export class LibraryModel {
  state: LibraryState;
  private generation = 0;
  private disposed = false;
  private stack: LibraryView[] = [];
  private cache = new Map<string, Page>();

  constructor(private api: RpcApi, private changed: (state: LibraryState) => void,
    private notify: (message: string) => void, kind: LibraryKind = "tracks") {
    this.state = { view: { kind, offset: 0, filter: "", sort: "title" }, loading: false };
  }

  async load(): Promise<void> {
    if (this.disposed) return;
    const generation = ++this.generation;
    const view = { ...this.state.view };
    const params = view.detail
      ? { kind: view.detail.kind, id: view.detail.id, uri: view.detail.uri, offset: view.offset, limit: LIBRARY_PAGE_SIZE }
      : { kind: view.kind, offset: view.offset, limit: LIBRARY_PAGE_SIZE, filter: view.filter, sort: view.sort };
    const method = view.detail ? "library.detail" : "library.list";
    const key = JSON.stringify([method, params]);
    this.state = { view, page: this.cache.get(key), loading: true };
    this.changed(this.state);
    try {
      const page = await this.api.call<Page>(method, params);
      if (this.disposed || generation !== this.generation) return;
      this.cache.set(key, page);
      this.state = { view: this.state.view, page, loading: false };
    } catch (error) {
      if (this.disposed || generation !== this.generation) return;
      this.state = { ...this.state, loading: false, error: error instanceof Error ? error.message : String(error) };
    }
    this.changed(this.state);
  }

  category(kind: LibraryKind): void {
    this.stack = [];
    this.state.view = { kind, offset: 0, filter: "", sort: "title" };
    void this.load();
  }
  filter(value: string): void {
    if (this.state.view.detail) return;
    this.state.view = { ...this.state.view, filter: value, offset: 0, selectedId: undefined };
    void this.load();
  }
  sort(): void {
    if (this.state.view.detail) return;
    const index = LIBRARY_SORTS.indexOf(this.state.view.sort);
    this.state.view = { ...this.state.view, sort: LIBRARY_SORTS[(index + 1) % LIBRARY_SORTS.length], offset: 0, selectedId: undefined };
    void this.load();
  }
  select(id?: string): void { this.state.view.selectedId = id; }
  open(row: Row): void {
    this.stack.push({ ...this.state.view });
    this.state.view = { ...this.state.view, detail: row, offset: 0, selectedId: undefined };
    void this.load();
  }
  back(): boolean {
    const previous = this.stack.pop();
    if (!previous) return false;
    this.state.view = previous;
    void this.load();
    return true;
  }
  page(direction: number): boolean {
    if (direction > 0 && !this.state.page?.has_more) return false;
    const offset = Math.max(0, this.state.view.offset + direction * LIBRARY_PAGE_SIZE);
    if (offset === this.state.view.offset) return false;
    this.state.view = { ...this.state.view, offset, selectedId: undefined };
    void this.load();
    return true;
  }
  async action(method: string, params: Record<string, unknown>, message: string, reload = false): Promise<void> {
    try {
      const result = await this.api.call<{ url?: string }>(method, params);
      if (this.disposed) return;
      this.notify(result?.url || message);
      if (reload) { this.cache.clear(); await this.load(); }
    } catch (error) {
      if (!this.disposed) this.notify(error instanceof Error ? error.message : String(error));
    }
  }
  dispose(): void { this.disposed = true; ++this.generation; this.cache.clear(); this.stack = []; }
}
