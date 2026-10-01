import type { Page, ScreenFactory } from "../../workspace/contracts.js";
import { createSurface } from "../../workspace/surface.js";
import { errorMessage, openResult, PageLoader, resultAction } from "../search/model.js";

export const createBrowseScreen: ScreenFactory = (ctx, params = {}) => {
  const category = typeof params.id === "string" ? params.id : undefined;
  const title = typeof params.title === "string" ? params.title : "Browse";
  const surface = createSurface(ctx, title, "Enter open • a queue • n next • s save • y share • [ / ] page • r retry • Backspace categories");
  let offset = 0;
  let page: Page | undefined;
  let disposed = false;
  const loader = new PageLoader(ctx.api, (value) => { page = value; surface.setRows(value.items, undefined, () => void action("open")); }, (message) => surface.setMessage(message, message.includes("failed:")));
  const refresh = () => loader.load(category ? "library.detail" : "library.list", category
    ? { kind: "category", id: category, offset, limit: 20 }
    : { kind: "browse", offset, limit: 20 });
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
  return {
    root: surface.root, title, refresh,
    editing: () => surface.editing(),
    setTheme: (theme) => surface.setTheme(theme),
    dispose() { disposed = true; loader.dispose(); surface.dispose(); },
    handleKey(key) {
      if (surface.editing()) return surface.handleKey(key);
      if (key.ctrl || key.meta) return surface.handleKey(key);
      const name = key.sequence?.length === 1 ? key.sequence : key.name;
      if (name === "backspace" && category) { ctx.navigate("browse"); return true; }
      if (name === "r") { void refresh(); return true; }
      if (name === "]" || name === "[") {
        if (name === "[" && offset > 0) offset = Math.max(0, offset - 20);
        else if (name === "]" && page?.has_more) offset += 20;
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
