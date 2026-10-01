import type { ParsedStatus } from "../../status.js";
import type { ScreenFactory } from "../../workspace/contracts.js";
import { createSurface } from "../../workspace/surface.js";
import { QueueController } from "./controller.js";

export const createQueueScreen: ScreenFactory = (ctx) => {
  const surface = createSurface(ctx, "Queue", "↑↓ select · Enter play · Delete remove · Shift ↑↓ reorder · c clear · s save · a append · n play next");
  const controller = new QueueController(ctx.api, {
    selected: () => surface.selected(),
    rows: (rows, selectedId) => surface.setRows(rows.map(row => ({
      ...row,
      title: `${row.meta?.current ? "▶ " : ""}${row.title}`,
      detail: `${typeof row.meta?.index === "number" ? row.meta.index + 1 : ""}${row.detail ? ` · ${row.detail}` : ""}`,
    })), selectedId, () => controller.selectedAction("play")),
    message: (message) => surface.setMessage(message),
    notify: (message) => ctx.notify(message),
  });
  let disposed = false;
  let timer: ReturnType<typeof setTimeout> | undefined;
  let lastRefresh = 0;
  let statusKey = fingerprint(ctx.status());
  const refresh = async () => {
    if (disposed) return;
    lastRefresh = Date.now();
    await controller.refresh();
  };
  const unsubscribe = ctx.onStatus(status => {
    const next = fingerprint(status);
    if (next === statusKey) return;
    statusKey = next;
    if (timer) return;
    timer = setTimeout(() => {
      timer = undefined;
      void refresh();
    }, Math.max(0, 500 - (Date.now() - lastRefresh)));
  });
  void refresh();
  return {
    root: surface.root,
    title: "Queue",
    editing: () => surface.editing(),
    refresh,
    setTheme: theme => surface.setTheme(theme),
    handleKey(key) {
      if (surface.editing()) return surface.handleKey(key);
      if (key.ctrl || key.meta) return false;
      if (key.shift && (key.name === "up" || key.name === "down")) {
        controller.moveSelected(key.name === "up" ? -1 : 1);
        return true;
      }
      if (key.name === "return" || key.name === "enter") { controller.selectedAction("play"); return true; }
      if (key.name === "delete" || key.name === "backspace") { controller.selectedAction("remove"); return true; }
      switch (key.name ?? key.sequence) {
        case "c": surface.confirm("Clear the entire queue?", () => { void controller.action("clear"); }); return true;
        case "s": surface.prompt("Save queue as playlist", "", name => { if (name.trim()) void controller.action("save", { name: name.trim() }); }); return true;
        case "a":
        case "n": {
          const action = (key.name ?? key.sequence) === "n" ? "play_next" : "append";
          surface.prompt(action === "append" ? "Append Spotify URI" : "Play Spotify URI next", "", uri => {
            if (uri.trim()) void controller.action(action, { uri: uri.trim() });
          });
          return true;
        }
        case "r": void refresh(); return true;
      }
      return surface.handleKey(key);
    },
    dispose() {
      if (disposed) return;
      disposed = true;
      if (timer) clearTimeout(timer);
      unsubscribe();
      controller.dispose();
      surface.dispose();
    },
  };
};

function fingerprint(status: ParsedStatus | null): string {
  return JSON.stringify([
    status?.playable?.uri ?? status?.playable?.id ?? null,
    status?.prototype?.up_next.map(track => track.uri ?? track.id ?? track.title),
  ]);
}
