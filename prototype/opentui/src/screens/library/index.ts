import type { Row, ScreenFactory } from "../../workspace/contracts.js";
import { createSurface } from "../../workspace/surface.js";
import { LIBRARY_KINDS, LibraryModel, type LibraryKind, type LibraryState } from "./model.js";

export const createLibraryScreen: ScreenFactory = (ctx, params) => {
  const requestedKind = params?.kind;
  const normalizedKind = requestedKind === "album" ? "albums" : requestedKind === "artist" ? "artists" : requestedKind;
  const initialKind = LIBRARY_KINDS.includes(normalizedKind as LibraryKind) ? normalizedKind as LibraryKind : "tracks";
  const surface = createSurface(ctx, "Library", "Tab category · / filter · s sort · Enter open/play · f save · a queue");
  let disposed = false;

  function play(row: Row) {
    if (row.uri) void model.action("player.action", { action: "play", uri: row.uri }, `Playing ${row.title}`);
    else ctx.notify("This item has no playable URI");
  }
  function activate(row: Row) {
    model.select(row.id);
    if (["album", "albums", "artist", "artists"].includes(row.kind)) model.open(row);
    else play(row);
  }
  function paint(state: LibraryState) {
    if (disposed) return;
    const { view, page } = state;
    const tabs = LIBRARY_KINDS.map(kind => kind === view.kind ? `[${kind}]` : kind).join("  ");
    surface.heading.content = view.detail ? `Library › ${view.detail.title}` : `Library  ${tabs}`;
    surface.hint.content = view.detail
      ? "Esc back · Enter open/play · Space play · a queue · p next · f save · y share"
      : `Tab category · / filter · s sort (${view.sort}) · Enter open/play · f save · a queue`;
    surface.setLines([]);
    surface.setRows(page?.items ?? [], view.selectedId, activate);
    if (!page?.items.length) surface.setLines([state.loading ? "Loading library…" : state.error ? "Library could not be loaded. Press r to retry." : view.filter ? `No matches for “${view.filter}”. Press / to change the filter.` : "Your library is empty."]);
    const range = page?.items.length ? `${page.offset + 1}–${page.offset + page.items.length} of ${page.total}` : "";
    const status = [state.error ? `Error: ${state.error}` : state.loading ? "Loading…" : "", page?.source, range, view.filter ? `Filter: ${view.filter}` : "", page?.has_more || view.offset ? "PgUp/PgDn pages" : ""].filter(Boolean).join(" · ");
    surface.setMessage(status, !!state.error);
  }
  const model = new LibraryModel(ctx.api, paint, ctx.notify, initialKind);
  if (typeof params?.id === "string" && params.id && (initialKind === "albums" || initialKind === "artists")) {
    model.open({
      id: params.id,
      kind: initialKind === "albums" ? "album" : "artist",
      title: typeof params.title === "string" ? params.title : params.id,
      subtitle: typeof params.subtitle === "string" ? params.subtitle : "",
      uri: typeof params.uri === "string" ? params.uri : undefined,
    });
  } else void model.load();

  return {
    root: surface.root,
    title: "Library",
    editing: () => surface.editing(),
    handleKey(key) {
      if (disposed) return false;
      if (surface.editing()) return surface.handleKey(key);
      if (key.ctrl || key.meta) return false;
      const name = key.name?.toLowerCase() ?? key.sequence;
      model.select(surface.selected()?.id);
      if (name === "escape") return model.back();
      if (name === "tab") {
        const index = LIBRARY_KINDS.indexOf(model.state.view.kind as LibraryKind);
        model.category(LIBRARY_KINDS[(index + (key.shift ? 2 : 1)) % LIBRARY_KINDS.length]!);
        return true;
      }
      if (name === "/" || key.sequence === "/") {
        if (!model.state.view.detail) surface.prompt("Filter library", model.state.view.filter, value => model.filter(value.trim()));
        else ctx.notify("Press Esc to filter your library");
        return true;
      }
      if (name === "s") { model.sort(); return true; }
      if (name === "pageup" && model.page(-1)) return true;
      if (name === "pagedown" && model.page(1)) return true;
      if (name === "r") {
        void model.action("library.action", { action: "refresh", kind: model.state.view.kind }, "Library refreshed", true);
        return true;
      }
      const row = surface.selected();
      if (row) {
        if (name === "return" || name === "enter") { activate(row); return true; }
        if (name === "space" || key.sequence === " ") { play(row); return true; }
        if (name === "f") {
          const saved = row.saved ?? !model.state.view.detail;
          const action = saved ? "unsave" : "save";
          const run = () => void model.action("library.action", { action, kind: row.kind, id: row.id, uri: row.uri }, action === "save" ? "Saved to library" : "Removed from library", true);
          if (action === "unsave") surface.confirm(`Remove ${row.title} from your library?`, run);
          else run();
          return true;
        }
        if ((name === "a" || name === "p") && row.uri) {
          void model.action("queue.action", { action: name === "a" ? "append" : "play_next", uri: row.uri }, name === "a" ? "Added to queue" : "Queued to play next");
          return true;
        }
        if (name === "y" && row.uri) { void model.action("share", { uri: row.uri }, "Link shared"); return true; }
      }
      const handled = surface.handleKey(key);
      if (handled) model.select(surface.selected()?.id);
      return handled;
    },
    refresh: () => model.load(),
    setTheme: theme => surface.setTheme(theme),
    dispose() { if (disposed) return; disposed = true; model.dispose(); surface.dispose(); },
  };
};
