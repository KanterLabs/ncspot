import type { Page, Params, Row, ScreenFactory, ScreenKey } from "../../workspace/contracts.js";
import { createSurface } from "../../workspace/surface.js";

const LIMIT = 30;
const HINT = "Enter open/play · c create · r rename · a add URI · d remove/delete · s save · p play playlist · u queue · n next · Esc back · / filter · [ ] page";

export const createPlaylistsScreen: ScreenFactory = (context, params = {}) => {
  const surface = createSurface(context, "Playlists", HINT);
  let playlist: Row | undefined = typeof params.id === "string"
    ? { id: params.id, kind: "playlist", title: typeof params.title === "string" ? params.title : "Playlist", subtitle: "", uri: typeof params.uri === "string" ? params.uri : undefined }
    : undefined;
  let offset = 0;
  let filter = "";
  let hasMore = false;
  let revision: string | undefined;
  let generation = 0;
  let disposed = false;
  let busy = false;
  let browserSelection: string | undefined;
  let browserOffset = 0;
  const title = () => playlist ? `Playlists / ${playlist.title}` : "Playlists";
  const message = (text: string) => { if (!disposed) surface.setMessage(text); };
  const error = (reason: unknown) => { if (!disposed) surface.setMessage(reason instanceof Error ? reason.message : String(reason), true); };
  const editable = (row: Row) => {
    if (row.meta?.owned === false || row.meta?.can_edit === false || row.meta?.editable === false) {
      message("This playlist cannot be edited by your account.");
      return false;
    }
    return true;
  };
  const refresh = async () => {
    if (disposed) return;
    const request = ++generation;
    const selectedId = surface.selected()?.id;
    surface.heading.content = title();
    message(`Loading ${title()}…`);
    try {
      const page = playlist
        ? await context.api.call<Page>("library.detail", { kind: "playlist", id: playlist.id, ...(playlist.uri ? { uri: playlist.uri } : {}), offset, limit: LIMIT })
        : await context.api.call<Page>("library.list", { kind: "playlists", offset, limit: LIMIT, filter });
      if (disposed || request !== generation) return;
      const rows = page.items;
      revision = playlist ? page.revision : undefined;
      offset = page.offset;
      hasMore = page.has_more;
      surface.setRows(rows, selectedId ?? (!playlist ? browserSelection : undefined), activate);
      message(`${title()} · ${page.total ? `${page.offset + 1}–${page.offset + page.items.length} of ${page.total}` : "No results"}${filter && !playlist ? ` · filter: ${filter}` : ""}`);
      return true;
    } catch (reason) {
      if (!disposed && request === generation) error(reason);
    }
  };
  const mutate = async (method: string, payload: Params, success: string, after?: () => void) => {
    if (busy || disposed) return;
    busy = true;
    const request = generation;
    try {
      await context.api.call(method, payload);
      if (disposed || request !== generation) return;
      after?.();
      const refreshed = await refresh();
      if (refreshed && !disposed && request + 1 === generation) message(`${title()} · ${success}`);
    } catch (reason) { if (!disposed && request === generation) error(reason); }
    finally { busy = false; }
  };
  const play = (row: Row, queue?: "append" | "play_next") => {
    if (!row.uri) { message("No playable URI is available for this item."); return; }
    void mutate(queue ? "queue.action" : "player.action", { action: queue ?? "play", uri: row.uri }, queue ? "Added to queue" : "Playback started");
  };
  function activate(row: Row) {
    if (playlist) { play(row); return; }
    browserSelection = row.id;
    browserOffset = offset;
    playlist = row;
    offset = 0;
    surface.setRows([]);
    void refresh();
  }
  const back = () => {
    if (!playlist) return false;
    playlist = undefined;
    offset = browserOffset;
    surface.setRows([]);
    void refresh();
    return true;
  };
  const target = () => playlist ?? surface.selected();
  const handleKey = (key: ScreenKey) => {
    if (surface.editing()) return surface.handleKey(key);
    const name = (key.name ?? key.sequence ?? "").toLowerCase();
    if (key.ctrl || key.meta) return false;
    if ((name === "escape" || name === "backspace") && back()) return true;
    if (name === "]" || name === "[" || name === "pagedown" || name === "pageup") {
      const forward = name === "]" || name === "pagedown";
      if (forward ? hasMore : offset > 0) { offset = Math.max(0, offset + (forward ? LIMIT : -LIMIT)); void refresh(); }
      return true;
    }
    if (name === "/" && !playlist) {
      surface.prompt("Filter playlists", filter, value => { filter = value.trim(); offset = 0; void refresh(); });
      return true;
    }
    if (name === "c") {
      surface.prompt("Create playlist", "", name => { if (name.trim()) void mutate("playlist.action", { action: "create", name: name.trim() }, "Playlist created", () => { playlist = undefined; offset = 0; }); });
      return true;
    }
    if (name === "return" || name === "enter") {
      const selected = surface.selected();
      if (selected) activate(selected);
      return true;
    }
    const row = target();
    if (name === "r") {
      if (row && editable(row)) surface.prompt("Rename playlist", row.title, name => { if (name.trim()) void mutate("playlist.action", { action: "rename", id: row.id, name: name.trim() }, "Playlist renamed", () => { if (playlist?.id === row.id) playlist = { ...row, title: name.trim() }; }); });
      return true;
    }
    if (name === "a") {
      if (row && editable(row)) surface.prompt("Add track URI", "", uri => { if (uri.trim()) void mutate("playlist.action", { action: "add", id: row.id, uri: uri.trim() }, "Track added"); });
      return true;
    }
    if (name === "d" || name === "delete") {
      if (!row || !editable(row)) return true;
      if (playlist) {
        const track = surface.selected();
        if (!track) return true;
        const position = track.meta?.index ?? track.meta?.position;
        if (typeof position !== "number" || !Number.isInteger(position) || position < 0) {
          message("Track position unavailable; refresh this playlist before removing a track.");
          return true;
        }
        const snapshot = revision;
        surface.confirm(`Remove ${track.title}?`, () => { void mutate("playlist.action", { action: "remove", id: row.id, position, ...(snapshot ? { revision: snapshot } : {}), ...(track.uri ? { uri: track.uri } : {}) }, "Track removed"); });
      } else surface.confirm(`Delete playlist ${row.title}?`, () => { void mutate("playlist.action", { action: "delete", id: row.id }, "Playlist deleted"); });
      return true;
    }
    if (name === "s") {
      if (row) void mutate("library.action", { action: row.saved ? "unsave" : "save", kind: "playlist", id: row.id, ...(row.uri ? { uri: row.uri } : {}) }, row.saved ? "Playlist unsaved" : "Playlist saved", () => { if (playlist?.id === row.id) playlist = { ...row, saved: !row.saved }; });
      return true;
    }
    if (name === "p" || name === "u" || name === "n") {
      const playable = playlist && name !== "p" ? surface.selected() : row;
      if (playable) play(playable, name === "u" ? "append" : name === "n" ? "play_next" : undefined);
      return true;
    }
    return surface.handleKey(key);
  };
  void refresh();
  return {
    root: surface.root, get title() { return title(); }, handleKey,
    editing: () => surface.editing(), refresh: async () => { await refresh(); }, setTheme: theme => surface.setTheme(theme),
    dispose() { if (disposed) return; disposed = true; generation++; surface.dispose(); },
  };
};
