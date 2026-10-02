import { createCliRenderer } from "@opentui/core";
import { appendFileSync, closeSync, openSync } from "node:fs";
import { mountWorkspace } from "./workspace/app.js";
import { WorkspaceClient, RpcError } from "./workspace/client.js";
import { DemoApi } from "./workspace/demo.js";
import { ROUTES, type Params, type Route, type RpcApi } from "./workspace/contracts.js";
import { readReducedMotion, saveReducedMotion } from "./workspace/preferences.js";
import { commandForKey } from "./commands.js";
import { parseStatus } from "./status.js";
import { DEFAULT_THEME, isThemeName, paletteForTheme, type ThemeName } from "./theme.js";
import { readThemePreference, saveThemePreference } from "./theme-store.js";

const usage = `Resonance OpenTUI

Usage:
  resonance                          launch engine and interface
  resonance --legacy-ui              retained Cursive interface
  resonance --headless               engine only
  resonance-opentui --socket PATH     attach to an existing engine
  resonance-opentui --demo            offline interactive preview
  --theme light|dark                  initial appearance
  --route ROUTE                      initial screen
  --reduced-motion                   disable ambient animation
  --debug FILE                       write redacted request timings
  --smoke                            validate parser and API fixtures

1 Play · 2 Queue · 3 Library · 4 Search · 5 Playlists · 6 Podcasts
7 Radio · 8 Settings · 9 Cast · B Browse · ? Help
Space play/pause · Shift+R radio · L appearance · : commands · q quit
Now Playing: / quick search · Enter play now · Ctrl+N next · Ctrl+E queue
Focused inputs capture these keys; Escape cancels or returns.
`;
export interface CliOptions { socket?: string; demo: boolean; smoke: boolean; theme?: ThemeName; route?: Route; reducedMotion?: boolean; debug?: string }
export function parseArgs(args: string[]): CliOptions | "help" | string {
  const options: CliOptions = { demo: false, smoke: false };
  for (let index = 0; index < args.length; index++) {
    const arg = args[index];
    if (arg === "--help" || arg === "-h") return "help";
    if (arg === "--demo") { options.demo = true; continue; }
    if (arg === "--smoke") { options.smoke = true; continue; }
    if (arg === "--reduced-motion") { options.reducedMotion = true; continue; }
    if (["--theme", "--socket", "-s", "--route", "--debug"].includes(arg)) {
      const value = args[++index];
      if (!value || value.startsWith("-")) return `${arg} requires a value`;
      if (arg === "--theme") { if (!isThemeName(value)) return "--theme must be light or dark"; options.theme = value; }
      else if (arg === "--route") { if (!(ROUTES as readonly string[]).includes(value)) return `--route must be one of ${ROUTES.join(", ")}`; options.route = value as Route; }
      else if (arg === "--debug") options.debug = value;
      else options.socket = value;
      continue;
    }
    return `Unknown argument: ${arg}`;
  }
  if (!options.demo && !options.smoke && !options.socket) return "Provide --socket PATH or use --demo";
  if (options.demo && options.socket) return "Choose either --demo or --socket PATH";
  return options;
}
async function smoke(): Promise<void> {
  const parsed = parseStatus({ mode: { Paused: 0 }, playable: null, prototype: { position_ms: 0, discovery: 50, volume_percent: 50, radio_active: false, radio_waiting: false, up_next: [] } });
  if (!parsed || commandForKey({ name: "space" }) !== "playpause") throw new Error("Status/command smoke check failed");
  const api = new DemoApi();
  const page = await api.call<{ items: unknown[] }>("library.list", { kind: "tracks" });
  if (!page.items.length) throw new Error("API fixture smoke check failed");
  process.stdout.write("resonance-opentui smoke: status parser, command mapping, and workspace API OK\n");
}
async function main(): Promise<void> {
  const parsed = parseArgs(Bun.argv.slice(2));
  if (parsed === "help") { process.stdout.write(usage); return; }
  if (typeof parsed === "string") { process.stderr.write(`${parsed}\n\n${usage}`); process.exitCode = 2; return; }
  if (parsed.smoke) { await smoke(); return; }
  const theme = parsed.theme ?? (parsed.demo ? DEFAULT_THEME : readThemePreference().theme);
  let descriptor: number | undefined;
  if (parsed.debug) descriptor = openSync(parsed.debug, "a", 0o600);
  const trace = (message: string) => { if (descriptor !== undefined) appendFileSync(descriptor, `${new Date().toISOString()} ${message}\n`); };
  const renderer = await createCliRenderer({ exitOnCtrlC: false, targetFps: 30, useMouse: true, backgroundColor: paletteForTheme(theme).background });
  let app: ReturnType<typeof mountWorkspace>;
  let client: WorkspaceClient | undefined;
  let demo: DemoApi | undefined;
  let transport: RpcApi;
  if (parsed.demo) {
    demo = new DemoApi(status => app?.setStatus(status)); transport = demo;
  } else {
    client = new WorkspaceClient(parsed.socket!, {
      onStatus: status => app?.setStatus(status),
      onClose: error => app?.setConnection(false, error ? `Engine error: ${error.message}` : "Engine disconnected; reopen to attach"),
    });
    transport = client;
  }
  const api: RpcApi = { async call<T>(method: string, params?: Params): Promise<T> {
    const start = performance.now();
    try {
      if (method === "settings.action" && params?.action === "command" && typeof params.command === "string") {
        const { routeForCommand } = await import("./workspace/bindings.js");
        if (routeForCommand(params.command) || params.command.startsWith("move ") || ["quit", "q", "x"].includes(params.command.trim())) return await app.command(params.command) as T;
      }
      const value = await transport.call<T>(method, params);
      if (method === "settings.get" && app && value && typeof value === "object") {
        const settings = value as { values?: { keybindings?: unknown }; bindings?: Record<string, string> };
        app.setBindings(settings.values?.keybindings); settings.bindings = app.bindings();
      }
      trace(`rpc ${method} ok duration_ms=${Math.round(performance.now() - start)}`); return value;
    }
    catch (error) { trace(`rpc ${method} error=${error instanceof RpcError ? error.code : "error"} duration_ms=${Math.round(performance.now() - start)}`); throw error; }
  } };
  let timer: ReturnType<typeof setInterval> | undefined;
  app = mountWorkspace(renderer, {
    api, theme, route: parsed.route,
    reducedMotion: parsed.reducedMotion ?? (parsed.demo ? false : readReducedMotion()),
    onThemeChange: parsed.demo ? undefined : value => { const saved = saveThemePreference(value); if (!saved.ok) throw new Error(saved.error); },
    onReducedMotionChange: parsed.demo ? undefined : saveReducedMotion,
    onQuit: () => client?.close(),
  });
  const teardown = () => { if (timer) clearInterval(timer); app.dispose(); client?.close(); if (descriptor !== undefined) { closeSync(descriptor); descriptor = undefined; } };
  renderer.once("destroy", teardown);
  if (demo) { app.setStatus(demo.status); app.setConnection(false, "Offline interactive preview"); timer = setInterval(() => demo!.tick(), 1000); }
  else {
    try { await client!.connect(); app.setConnection(true, "Live engine"); await api.call("settings.get"); await app.refresh(); }
    catch (error) { app.setConnection(false, `Unable to attach: ${error instanceof Error ? error.message : error}`); app.notify("Engine unavailable. Close this window and reopen Resonance; --legacy-ui remains available."); }
  }
}
if (import.meta.main) main().catch(error => { process.stderr.write(`resonance-opentui: ${error instanceof Error ? error.message : error}\n`); process.exitCode = 1; });
