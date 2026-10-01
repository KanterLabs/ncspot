import type { Row, RpcApi, ScreenContext } from "../../workspace/contracts.js";

export interface SettingsData {
  values: Record<string, unknown>;
  bindings: Record<string, string>;
}

const privateKey = /password|credential|secret|token|username|email|cookie|authorization/i;

/** Display scalar configuration only; never stringify opaque credential objects. */
export function settingRows(values: Record<string, unknown>): Row[] {
  return Object.entries(values).filter(([key, value]) => !privateKey.test(key)
    && (value === null || ["string", "number", "boolean"].includes(typeof value)))
    .sort(([a], [b]) => a.localeCompare(b))
    .map(([key, value]) => ({ id: `value:${key}`, kind: "setting", title: key.replaceAll("_", " "), subtitle: String(value ?? "unset") }));
}

export function appearanceRows(context: Pick<ScreenContext, "theme" | "reducedMotion">): Row[] {
  return [
    { id: "theme", kind: "action", title: "Appearance", subtitle: `${context.theme()} · Enter to switch theme` },
    { id: "motion", kind: "action", title: "Reduced motion", subtitle: `${context.reducedMotion() ? "on" : "off"} · Enter to toggle` },
    { id: "reload", kind: "action", title: "Reload configuration", subtitle: "Read updated settings from your ncspot config" },
    { id: "reconnect", kind: "action", title: "Reconnect", subtitle: "Reconnect the Spotify session" },
    { id: "logout", kind: "action", title: "Log out", subtitle: "Confirmation required" },
  ];
}

export function toggleAppearance(context: Pick<ScreenContext, "theme" | "setTheme" | "reducedMotion" | "setReducedMotion">, id: string): boolean {
  if (id === "theme") { context.setTheme(context.theme() === "light" ? "dark" : "light"); return true; }
  if (id === "motion") { context.setReducedMotion(!context.reducedMotion()); return true; }
  return false;
}

export async function settingsAction(api: RpcApi, action: "reload" | "reconnect" | "logout" | "command", command?: string): Promise<unknown> {
  if (action === "command") {
    const input = command?.trim();
    if (!input) throw new Error("Enter an ncspot command");
    if (/[\r\n\x00]/.test(input)) throw new Error("Enter a single command");
    return api.call("settings.action", { action, command: input });
  } else {
    return api.call("settings.action", { action });
  }
}

export function actionNotice(label: string, result: unknown): string {
  if (result && typeof result === "object") {
    const acknowledgment = result as { completed?: unknown; accepted?: unknown };
    if (acknowledgment.completed === true) return `${label} completed`;
    if (acknowledgment.accepted === true) return `${label} accepted`;
    if (acknowledgment.completed === false) return `${label} started`;
  }
  return `${label} completed`;
}

export function errorText(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}
