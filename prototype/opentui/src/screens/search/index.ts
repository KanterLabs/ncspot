import type { Page, ScreenFactory } from "../../workspace/contracts.js";
import { createSurface } from "../../workspace/surface.js";
import { cycleSearchKind, errorMessage, openResult, PageLoader, resultAction, SEARCH_PAGE_SIZE, searchKind } from "./model.js";

export const createSearchScreen: ScreenFactory = (ctx, params = {}) => {
  const surface = createSurface(ctx, "Search", "/ search • Tab category • Enter open/play • a queue • n next • s save • y share • [ / ] page • r retry");
  let query = typeof params.query === "string" ? params.query.trim() : "";
  let kind = searchKind(params.kind);
  let offset = 0;
  let page: Page | undefined;
  let disposed = false;
  const loader = new PageLoader(ctx.api, (value) => { page = value; surface.setRows(value.items, undefined, () => void action("open")); }, (message) => surface.setMessage(`${kind} • “${query}” • ${message}`, message.includes("failed:")));
  const refresh = (force = false) => {
    if (!query) { loader.invalidate(); page = undefined; surface.setRows([]); surface.setMessage("Search tracks, albums, artists, playlists, shows and episodes • / enter a query"); return; }
    return loader.load("search", { query, kind, offset, limit: SEARCH_PAGE_SIZE, ...(force ? { refresh: true } : {}) });
  };
  const prompt = () => surface.prompt("Search", query, (value: string) => { query = value.trim(); offset = 0; void refresh(); });
  const action = async (name: "open" | "append" | "play_next" | "save" | "share") => {
    const row = surface.selected();
    if (!row) return;
    try {
      if (name === "open") await openResult(ctx, row);
      else await resultAction(ctx, row, name);
      if (name === "save" && !disposed) surface.setRows(page?.items ?? [], row.id, () => void action("open"));
    } catch (error) { if (!disposed) surface.setMessage(`Action failed: ${errorMessage(error)}`, true); }
  };
  void refresh();
  if (!query) queueMicrotask(() => { if (!disposed) prompt(); });
  return {
    root: surface.root, title: "Search", refresh,
    editing: () => surface.editing(),
    setTheme: (theme) => surface.setTheme(theme),
    dispose() { disposed = true; loader.dispose(); surface.dispose(); },
    handleKey(key) {
      if (surface.editing()) return surface.handleKey(key);
      if (key.ctrl || key.meta) return surface.handleKey(key);
      // OpenTUI keeps printable input in `sequence`, but named keys such as
      // Enter also carry a one-character terminal sequence ("\r"). Prefer
      // the semantic name so a real Enter reaches the playback branch.
      const name = key.name?.toLowerCase() || key.sequence;
      if (name === "/") { prompt(); return true; }
      if (name === "tab") {
        kind = cycleSearchKind(kind, key.shift);
        offset = 0; void refresh(); return true;
      }
      if (name === "r") { void refresh(true); return true; }
      if (name === "]" || name === "[") {
        if (name === "[" && offset > 0) offset = Math.max(0, offset - SEARCH_PAGE_SIZE);
        else if (name === "]" && page?.has_more) offset += SEARCH_PAGE_SIZE;
        else return true;
        void refresh(); return true;
      }
      if (name === "return" || name === "enter") { void action("open"); return true; }
      const actions = { a: "append", n: "play_next", s: "save", y: "share" } as const;
      if (name && name in actions) { void action(actions[name as keyof typeof actions]); return true; }
      return surface.handleKey(key);
    },
  };
};
