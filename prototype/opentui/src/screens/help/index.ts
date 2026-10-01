import type { ScreenFactory } from "../../workspace/contracts.js";
import { createSurface } from "../../workspace/surface.js";
import { actionNotice, errorText, settingsAction, type SettingsData } from "../settings/model.js";
import { helpRows } from "./model.js";

export const createHelpScreen: ScreenFactory = (ctx, params) => {
  const surface = createSurface(ctx, "Help & Commands", "↑/↓ scroll · : command · Enter on command row · r refresh bindings");
  let disposed = false;
  let busy = false;
  let pending: Promise<void> | undefined;
  function prompt() {
    if (busy || disposed) return;
    surface.prompt("ncspot command", "", command => { void run(command); });
  }
  async function run(command: string) {
    if (busy || disposed) return;
    busy = true;
    try {
      const result = await settingsAction(ctx.api, "command", command);
      const notice = actionNotice("Command", result);
      if (!disposed) { surface.setMessage(notice); ctx.notify(notice); }
    } catch (error) {
      if (!disposed) surface.setMessage(`Command failed: ${errorText(error)}`, true);
    } finally { busy = false; }
  }
  surface.setRows(helpRows({}), undefined, row => { if (row.id === "command") prompt(); });
  if (params?.command === true) prompt();
  return {
    root: surface.root, title: "Help & Commands",
    refresh() {
      if (disposed) return Promise.resolve();
      if (pending) return pending;
      pending = (async () => {
        try {
          const data = await ctx.api.call<SettingsData>("settings.get");
          if (!disposed) {
            surface.setRows(helpRows(data.bindings ?? {}), undefined, row => { if (row.id === "command") prompt(); });
            surface.setMessage("Workspace shortcuts above · ncspot bindings below · commands validated by backend");
          }
        } catch (error) {
          if (!disposed) surface.setMessage(`Bindings unavailable: ${errorText(error)}`, true);
        }
      })().finally(() => { pending = undefined; });
      return pending;
    },
    handleKey(key) {
      if (surface.handleKey(key)) return true;
      if (surface.editing() || key.ctrl || key.meta) return false;
      const name = key.name?.toLowerCase();
      if (key.sequence === ":" || name === ":") { prompt(); return true; }
      if ((name === "enter" || name === "return") && surface.selected()?.id === "command") { prompt(); return true; }
      if (name === "r") { void this.refresh(); return true; }
      return false;
    },
    editing: surface.editing,
    setTheme: surface.setTheme,
    dispose() { disposed = true; surface.dispose(); },
  };
};
