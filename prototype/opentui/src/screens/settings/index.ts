import type { Row, ScreenFactory } from "../../workspace/contracts.js";
import { createSurface } from "../../workspace/surface.js";
import { actionNotice, appearanceRows, errorText, settingsAction, settingRows, toggleAppearance, type SettingsData } from "./model.js";

export const createSettingsScreen: ScreenFactory = ctx => {
  const surface = createSurface(ctx, "Settings", "↑/↓ select · Enter change · double-click activate · r refresh");
  let disposed = false;
  let busy = false;
  let values: SettingsData["values"] = {};
  let pending: Promise<void> | undefined;
  let refreshFailed = false;

  const draw = () => surface.setRows([...appearanceRows(ctx), ...settingRows(values)], undefined, activate);
  const refresh = (): Promise<void> => {
    if (disposed) return Promise.resolve();
    if (pending) return pending;
    pending = (async () => {
      try {
        const data = await ctx.api.call<SettingsData>("settings.get");
        if (disposed) return;
        values = data.values ?? {};
        refreshFailed = false;
        draw();
        surface.setMessage("Read-only config · run `resonance info` to find it, then reload");
      } catch (error) {
        refreshFailed = true;
        if (!disposed) surface.setMessage(`Settings unavailable: ${errorText(error)}`, true);
      }
    })().finally(() => { pending = undefined; });
    return pending;
  };
  async function run(action: "reload" | "reconnect" | "logout") {
    if (busy || disposed) return;
    busy = true;
    surface.setMessage(`${action}…`);
    try {
      const result = await settingsAction(ctx.api, action);
      const notice = actionNotice(action, result);
      if (disposed) return;
      if (action === "reload" && notice === "reload completed") {
        if (pending) await pending;
        await refresh();
        if (refreshFailed) return;
      }
      if (!disposed) { surface.setMessage(notice); ctx.notify(notice); }
    } catch (error) {
      if (!disposed) surface.setMessage(`${action} failed: ${errorText(error)}`, true);
    } finally { busy = false; }
  }
  function activate(row: Row) {
    if (busy || disposed) return;
    if (toggleAppearance(ctx, row.id)) { draw(); return; }
    if (row.id === "logout") surface.confirm("Log out of Spotify?", () => { void run("logout"); });
    else if (row.id === "reload" || row.id === "reconnect") void run(row.id);
  }
  draw();
  return {
    root: surface.root, title: "Settings", refresh,
    handleKey(key) {
      if (surface.handleKey(key)) return true;
      if (surface.editing() || key.ctrl || key.meta) return false;
      const name = key.name?.toLowerCase();
      if (name === "enter" || name === "return") { const row = surface.selected(); if (row) activate(row); return true; }
      if (name === "r") { void refresh(); return true; }
      return false;
    },
    editing: surface.editing,
    setTheme(theme) { surface.setTheme(theme); draw(); },
    dispose() { disposed = true; surface.dispose(); },
  };
};
