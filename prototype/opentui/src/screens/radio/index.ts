import type { Row, ScreenFactory } from "../../workspace/contracts.js";
import { createSurface } from "../../workspace/surface.js";
import { RadioController } from "./controller.js";

export const createRadioScreen: ScreenFactory = ctx => {
  const surface = createSurface(ctx, "Radio & Discovery", "s start/stop · ←/→ ±5 · f/m/e familiar/balanced/explore · d debug · Enter detail");
  let lines: string[] = [];
  let candidates: Row[] = [];
  let details = false;
  let selectedId: string | undefined;
  const render = () => {
    if (details) return;
    surface.setLines(lines);
    surface.setRows(candidates, selectedId);
    selectedId = undefined;
  };
  const controller = new RadioController(ctx.api, {
    lines(value) { lines = value; },
    rows(value) { candidates = value; render(); },
    message(value) { surface.setMessage(value); },
  }, listener => ctx.onStatus(listener));
  void controller.refresh();
  return {
    root: surface.root, title: "Radio & Discovery",
    editing: () => surface.editing(),
    handleKey(key) {
      if (surface.editing()) return surface.handleKey(key);
      if (key.ctrl || key.meta) return false;
      const name = key.name?.toLowerCase();
      if (name === "escape" && details) { details = false; render(); return true; }
      if (name === "return" || name === "enter") {
        const row = surface.selected();
        if (row?.kind === "radio-candidate") {
          details = true;
          selectedId = row.id;
          surface.setRows([]);
          surface.setLines([row.title, row.uri ?? "URI unavailable", ...row.subtitle.split(" · "), "Score components", ...(row.detail ?? "unavailable").split(" · "), "Esc to return"]);
        }
        return true;
      }
      switch (name) {
        case "s": void controller.action(controller.status.active === true ? "stop" : "start"); return true;
        case "left": case "[": void controller.adjust(-5); return true;
        case "right": case "]": void controller.adjust(5); return true;
        case "f": void controller.action("discovery", 0); return true;
        case "m": void controller.action("discovery", 50); return true;
        case "e": void controller.action("discovery", 100); return true;
        case "d": surface.prompt("Deterministic RNG seed", "42", seed => { details = false; void controller.debug(seed); }); return true;
        case "r": void controller.refresh(); return true;
        default: return surface.handleKey(key);
      }
    },
    refresh: () => controller.refresh(),
    setTheme: theme => surface.setTheme(theme),
    dispose() { controller.dispose(); surface.dispose(); },
  };
};
