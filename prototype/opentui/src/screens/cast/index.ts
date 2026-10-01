import type { ScreenFactory } from "../../workspace/contracts.js";
import { createSurface } from "../../workspace/surface.js";
import { CastController } from "./controller.js";

export const createCastScreen: ScreenFactory = (ctx) => {
  const surface = createSurface(ctx, "Casting", "↑↓ select · Enter connect · d disconnect · r refresh");
  surface.setLines([
    "Spotify Connect devices use the signed-in Spotify account.",
    "Roku opens its Spotify app, then waits for Spotify Connect.",
  ]);
  const controller = new CastController(ctx.api, {
    selected: () => surface.selected(),
    rows: (rows, id) => surface.setRows(rows, id, row => { void controller.connect(row); }),
    message: (message, error) => surface.setMessage(message, error),
    notify: message => ctx.notify(message),
  });
  void controller.refresh();
  return {
    root: surface.root,
    title: "Casting",
    handleKey(key) {
      if (surface.handleKey(key)) return true;
      if (key.ctrl || key.meta || surface.editing()) return false;
      switch (key.name?.toLowerCase()) {
        case "enter": case "return": void controller.connect(); return true;
        case "d": void controller.disconnect(); return true;
        case "r": void controller.refresh(); return true;
        default: return false;
      }
    },
    refresh: () => controller.refresh(),
    editing: () => surface.editing(),
    setTheme: theme => surface.setTheme(theme),
    dispose() { controller.dispose(); surface.dispose(); },
  };
};
