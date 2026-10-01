import { createCliRenderer } from "@opentui/core";
import { mountResonance } from "./app.js";
import { DemoTransport, IpcClient, type CommandTransport } from "./ipc.js";
import { commandForKey } from "./commands.js";
import { demoStatus, parseStatus } from "./status.js";
import { DEFAULT_THEME, isThemeName, paletteForTheme, type ThemeName } from "./theme.js";
import { readThemePreference, saveThemePreference } from "./theme-store.js";

const usage = `Resonance OpenTUI prototype

Usage:
  resonance-opentui --socket PATH
  resonance-opentui --demo
  resonance-opentui --theme light|dark   choose the initial palette
  resonance-opentui --smoke

Keys:
  space / enter  play or pause       ←/→ or p/n  previous / next
  r              local radio          d           cycle discovery
  l              toggle light / dark (saved for the next launch)
  +/- or ↑/↓     volume               q / Esc / F5 close this prototype
`;

export interface CliOptions {
  socket?: string;
  demo: boolean;
  smoke: boolean;
  theme?: ThemeName;
}

export function parseArgs(args: string[]): CliOptions | "help" | string {
  const options: CliOptions = { demo: false, smoke: false };
  for (let index = 0; index < args.length; index += 1) {
    const arg = args[index];
    if (arg === "--help" || arg === "-h") return "help";
    if (arg === "--demo") {
      options.demo = true;
      continue;
    }
    if (arg === "--smoke") {
      options.smoke = true;
      continue;
    }
    if (arg === "--theme") {
      const value = args[index + 1];
      if (!value || value.startsWith("-")) return "--theme requires light or dark";
      if (!isThemeName(value)) return "--theme must be light or dark";
      options.theme = value;
      index += 1;
      continue;
    }
    if (arg === "--socket" || arg === "-s") {
      const path = args[index + 1];
      if (!path || path.startsWith("-")) return "--socket requires a Unix socket path";
      options.socket = path;
      index += 1;
      continue;
    }
    return `Unknown argument: ${arg}`;
  }
  if (!options.demo && !options.smoke && !options.socket) {
    return "Provide --socket PATH, or use --demo for an offline preview";
  }
  if (options.demo && options.socket) return "Choose either --demo or --socket PATH";
  return options;
}

async function smoke(): Promise<void> {
  const fixture = JSON.stringify({
    mode: { Paused: { secs: 91, nanos: 500_000_000 } },
    playable: {
      type: "Track",
      title: "Smoke Signal",
      artists: ["KanterLabs"],
      album: "Resonance",
      duration: 180_000,
    },
    prototype: {
      position_ms: 91_500,
      discovery: 75,
      volume_percent: 64,
      radio_active: true,
      radio_waiting: false,
      up_next: [],
    },
  });
  const parsed = parseStatus(fixture);
  if (!parsed || parsed.playable?.type !== "Track") throw new Error("status parser smoke check failed");
  const commands = [
    commandForKey({ name: "space" }),
    commandForKey({ name: "right" }),
    commandForKey({ name: "d" }, 75),
  ];
  if (commands.join(",") !== "playpause,next,discovery 100") {
    throw new Error(`command smoke check failed: ${commands.join(",")}`);
  }
  process.stdout.write("resonance-opentui smoke: status parser and command mapping OK\n");
}

async function main(): Promise<void> {
  const parsedArgs = parseArgs(Bun.argv.slice(2));
  if (parsedArgs === "help") {
    process.stdout.write(usage);
    return;
  }
  if (typeof parsedArgs === "string") {
    process.stderr.write(`${parsedArgs}\n\n${usage}`);
    process.exitCode = 2;
    return;
  }
  if (parsedArgs.smoke) {
    await smoke();
    return;
  }

  // Reading the frontend preference never creates or mutates a file. Demo
  // mode intentionally starts from light unless explicitly overridden and
  // does not install a persistence callback.
  const initialTheme = parsedArgs.theme ?? (parsedArgs.demo ? DEFAULT_THEME : readThemePreference().theme);
  const persistTheme = parsedArgs.demo
    ? undefined
    : (theme: ThemeName): void => {
        const result = saveThemePreference(theme);
        if (!result.ok) {
          const message = result.error ?? "unable to save theme preference";
          throw new Error(message);
        }
      };

  const renderer = await createCliRenderer({
    exitOnCtrlC: true,
    targetFps: 30,
    useMouse: true,
    backgroundColor: paletteForTheme(initialTheme).background,
    onDestroy: () => undefined,
  });

  let transport: CommandTransport;
  let app: ReturnType<typeof mountResonance>;
  let demoTimer: ReturnType<typeof setInterval> | undefined;

  if (parsedArgs.demo) {
    transport = new DemoTransport();
    app = mountResonance(renderer, {
      transport,
      connected: false,
      connectionMessage: "offline preview",
      theme: initialTheme,
      onThemeChange: persistTheme,
    });
    app.setStatus(demoStatus());
    demoTimer = setInterval(() => app.tick(), 250);
  } else {
    const client = new IpcClient(parsedArgs.socket!, {
      onStatus: (status) => app?.setStatus(status),
      onOpen: () => app?.setConnection(true, "live socket"),
      onClose: (error) => app?.setConnection(false, error ? `socket error: ${error.message}` : "socket closed"),
      // Ignore one malformed line and keep listening for the next valid status.
      onMalformedLine: () => undefined,
    });
    transport = client;
    app = mountResonance(renderer, {
      transport,
      connected: false,
      connectionMessage: "connecting",
      theme: initialTheme,
      onThemeChange: persistTheme,
    });
    try {
      await client.connect();
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      app.setConnection(false, `socket unavailable: ${message}`);
    }
  }

  const teardown = (): void => {
    if (demoTimer) clearInterval(demoTimer);
    app.dispose();
  };
  renderer.once("destroy", teardown);
}

main().catch((error) => {
  process.stderr.write(`resonance-opentui: ${error instanceof Error ? error.stack ?? error.message : String(error)}\n`);
  process.exitCode = 1;
});
