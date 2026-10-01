import {
  BoxRenderable,
  MouseButton,
  TextAttributes,
  TextRenderable,
  bold,
  createTextAttributes,
  dim,
  fg,
  t,
  type CliRenderer,
  type MouseEvent,
} from "@opentui/core";
import { commandForKey, nextDiscovery as nextDiscoveryLevel } from "./commands.js";
import { DemoTransport, type CommandTransport } from "./ipc.js";
import {
  DEMO_TRACKS,
  formatTime,
  initials,
  playableAlbum,
  playableArtists,
  playableTitle,
  positionAt,
  type ParsedStatus,
  type Playable,
  type Track,
  type UiState,
  createUiState,
} from "./status.js";

export const COLORS = {
  background: "#080a10",
  panel: "#0f131d",
  panelRaised: "#151b28",
  border: "#293247",
  borderSoft: "#1c2432",
  text: "#e8edf8",
  muted: "#8a96ad",
  dim: "#58647b",
  accent: "#9c7bff",
  accentBright: "#c8b8ff",
  accentDim: "#4b3e79",
  teal: "#57d5c8",
  amber: "#f6bd72",
  red: "#f07887",
};

const WAVE_HEIGHTS = [
  1, 2, 3, 5, 7, 9, 7, 5, 3, 4, 6, 8, 10, 8, 6, 4, 2, 3, 5, 8, 10, 8, 6, 4,
  2, 3, 5, 7, 8, 6, 4, 2, 1, 3, 6, 9, 7, 5, 3, 4, 7, 9, 6, 4, 2, 1,
];
const WAVE_CHARS = "▁▂▃▄▅▆▇█";

function waveform(width: number, phase: number): string {
  const columns = Math.max(12, Math.min(96, width));
  return Array.from({ length: columns }, (_, index) => {
    const source = WAVE_HEIGHTS[(index + phase) % WAVE_HEIGHTS.length];
    const pulse = Math.sin((index + phase) * 0.42) * 1.25;
    return WAVE_CHARS[Math.max(0, Math.min(WAVE_CHARS.length - 1, Math.round(source + pulse) - 1))];
  }).join("");
}

function text(
  renderer: CliRenderer,
  content: string,
  options: ConstructorParameters<typeof TextRenderable>[1] = {},
): TextRenderable {
  return new TextRenderable(renderer, {
    content,
    fg: COLORS.text,
    wrapMode: "none",
    truncate: true,
    ...options,
  });
}

function panel(renderer: CliRenderer, options: ConstructorParameters<typeof BoxRenderable>[1] = {}): BoxRenderable {
  return new BoxRenderable(renderer, {
    backgroundColor: COLORS.panel,
    border: true,
    borderStyle: "single",
    borderColor: COLORS.border,
    padding: 1,
    ...options,
  });
}

function eventIsPrimaryClick(event: MouseEvent): boolean {
  return event.type === "down" && event.button === MouseButton.LEFT;
}

function modeLabel(status: UiState): string {
  if (status.mode.kind === "playing") return "PLAYING";
  if (status.mode.kind === "paused") return "PAUSED";
  if (status.mode.kind === "finished") return "FINISHED";
  return "STOPPED";
}

function modeIcon(status: UiState): string {
  return status.mode.kind === "playing" ? "Ⅱ" : "▶";
}

function isPlaying(status: UiState): boolean {
  return status.mode.kind === "playing";
}

export interface ResonanceAppOptions {
  transport: CommandTransport;
  connected?: boolean;
  connectionMessage?: string;
  onQuit?: () => void;
}

export interface ResonanceApp {
  readonly state: UiState;
  setStatus(status: ParsedStatus): void;
  setConnection(connected: boolean, message: string): void;
  tick(nowMs?: number): void;
  quit(): void;
  dispose(): void;
}

/**
 * Mount the now-playing surface into a real OpenTUI renderer. The returned
 * controller is intentionally small so smoke tests can drive status changes
 * without a terminal or socket.
 */
export function mountResonance(renderer: CliRenderer, options: ResonanceAppOptions): ResonanceApp {
  let state: UiState = createUiState({
    connected: options.connected ?? false,
    connectionMessage: options.connectionMessage ?? "disconnected",
  });
  let disposed = false;
  let wavePhase = 0;

  const shell = new BoxRenderable(renderer, {
    id: "resonance-shell",
    width: "100%",
    height: "100%",
    flexDirection: "column",
    backgroundColor: COLORS.background,
    padding: 1,
    gap: 1,
  });
  renderer.root.add(shell);

  const header = new BoxRenderable(renderer, {
    id: "resonance-header",
    width: "100%",
    height: 3,
    flexDirection: "row",
    alignItems: "center",
    justifyContent: "space-between",
    paddingX: 1,
    backgroundColor: COLORS.panel,
    border: true,
    borderColor: COLORS.borderSoft,
  });
  shell.add(header);

  const headerLeft = new BoxRenderable(renderer, {
    flexDirection: "row",
    alignItems: "center",
    gap: 1,
  });
  const brand = text(renderer, "KANTERLABS", {
    fg: COLORS.accentBright,
    attributes: TextAttributes.BOLD,
  });
  const product = text(renderer, "RESONANCE", {
    fg: COLORS.text,
    attributes: createTextAttributes({ bold: true, italic: true }),
  });
  headerLeft.add(brand);
  headerLeft.add(product);
  header.add(headerLeft);

  const headerRight = new BoxRenderable(renderer, {
    flexDirection: "row",
    alignItems: "center",
    gap: 2,
  });
  const connection = text(renderer, "", { fg: COLORS.teal });
  const connectionDot = text(renderer, "●", { fg: COLORS.teal });
  const shortcutHint = text(renderer, "SPACE play  ·  ←/→ skip  ·  D discovery  ·  Q close", {
    fg: COLORS.dim,
  });
  headerRight.add(connectionDot);
  headerRight.add(connection);
  headerRight.add(shortcutHint);
  header.add(headerRight);

  const body = new BoxRenderable(renderer, {
    id: "resonance-body",
    width: "100%",
    flexGrow: 1,
    flexDirection: "row",
    gap: 1,
    minHeight: 16,
  });
  shell.add(body);

  const hero = panel(renderer, {
    id: "now-playing-panel",
    flexGrow: 6,
    flexDirection: "column",
    justifyContent: "space-between",
    minWidth: 52,
    gap: 1,
  });
  body.add(hero);

  const heroTop = new BoxRenderable(renderer, {
    flexDirection: "row",
    gap: 2,
    minHeight: 15,
    flexShrink: 0,
  });
  hero.add(heroTop);

  const cover = new BoxRenderable(renderer, {
    id: "cover-art",
    width: 30,
    height: 15,
    flexShrink: 0,
    backgroundColor: "#252044",
    border: true,
    borderColor: COLORS.accentDim,
    flexDirection: "column",
    alignItems: "center",
    justifyContent: "center",
    gap: 1,
  });
  const coverGlyph = text(renderer, "╱╲  ╱╲\n╲╱  ╲╱", {
    fg: COLORS.accentBright,
    textAlign: "center",
    attributes: TextAttributes.BOLD,
  });
  const coverInitials = text(renderer, "RL", {
    fg: COLORS.text,
    textAlign: "center",
    attributes: createTextAttributes({ bold: true, italic: true }),
  });
  const coverCaption = text(renderer, "NO COVER FETCH", {
    fg: COLORS.dim,
    textAlign: "center",
  });
  cover.add(coverGlyph);
  cover.add(coverInitials);
  cover.add(coverCaption);
  heroTop.add(cover);

  const nowMeta = new BoxRenderable(renderer, {
    flexGrow: 1,
    flexDirection: "column",
    justifyContent: "center",
    gap: 1,
    minWidth: 28,
  });
  const eyebrow = text(renderer, "NOW PLAYING  /  RESONANCE SESSION", {
    fg: COLORS.accent,
    attributes: TextAttributes.BOLD,
  });
  const title = text(renderer, "", {
    fg: COLORS.text,
    attributes: TextAttributes.BOLD,
    wrapMode: "word",
    truncate: false,
  });
  const artists = text(renderer, "", { fg: COLORS.accentBright });
  const album = text(renderer, "", { fg: COLORS.muted });
  const statusText = text(renderer, "", { fg: COLORS.teal });
  nowMeta.add(eyebrow);
  nowMeta.add(title);
  nowMeta.add(artists);
  nowMeta.add(album);
  nowMeta.add(statusText);
  heroTop.add(nowMeta);

  const waveformText = text(renderer, waveform(54, 0), {
    id: "decorative-waveform",
    fg: COLORS.accent,
    attributes: TextAttributes.BOLD,
    textAlign: "center",
    height: 1,
    flexShrink: 0,
  });
  const waveformCaption = text(renderer, "SIGNAL SHAPE  ·  DECORATIVE VISUALIZER", {
    fg: COLORS.dim,
    textAlign: "center",
    height: 1,
    flexShrink: 0,
  });
  hero.add(waveformText);
  hero.add(waveformCaption);

  const progressLabel = text(renderer, "", { fg: COLORS.muted });
  progressLabel.height = 1;
  progressLabel.flexShrink = 0;
  const progressBar = text(renderer, "", { fg: COLORS.accent, height: 1, flexShrink: 0 });
  const progressBox = new BoxRenderable(renderer, {
    flexDirection: "column",
    gap: 1,
    height: 2,
    flexShrink: 0,
  });
  progressBox.add(progressBar);
  progressBox.add(progressLabel);
  hero.add(progressBox);

  const controls = new BoxRenderable(renderer, {
    id: "transport-controls",
    flexDirection: "row",
    justifyContent: "center",
    alignItems: "center",
    gap: 1,
    height: 3,
    flexShrink: 0,
  });
  hero.add(controls);

  const makeButton = (label: string, command: string, width = 11): BoxRenderable => {
    const button = new BoxRenderable(renderer, {
      width,
      height: 3,
      border: true,
      borderColor: COLORS.border,
      focusedBorderColor: COLORS.accent,
      alignItems: "center",
      justifyContent: "center",
      focusable: true,
      onMouseDown: (event) => {
        if (eventIsPrimaryClick(event)) dispatch(command);
      },
    });
    button.add(text(renderer, label, { textAlign: "center", fg: COLORS.muted }));
    controls.add(button);
    return button;
  };

  const previousButton = makeButton("‹  PREV", "previous", 11);
  const playButton = makeButton("▶  PLAY", "playpause", 15);
  const nextButton = makeButton("NEXT  ›", "next", 11);
  const radioButton = makeButton("RADIO  ◈", "radio", 13);

  const setButtonLabel = (button: BoxRenderable, label: string): void => {
    const child = button.getChildren()[0];
    if (child instanceof TextRenderable) child.content = label;
  };

  const footer = new BoxRenderable(renderer, {
    flexDirection: "row",
    justifyContent: "space-between",
    alignItems: "center",
    height: 3,
    backgroundColor: COLORS.panel,
    border: false,
    paddingX: 1,
    flexShrink: 0,
  });
  const discovery = text(renderer, "", { fg: COLORS.amber });
  const discoveryControl = new BoxRenderable(renderer, {
    width: 30,
    height: 3,
    border: true,
    borderColor: COLORS.border,
    focusedBorderColor: COLORS.amber,
    alignItems: "center",
    justifyContent: "center",
    focusable: true,
    onMouseDown: (event) => {
      if (eventIsPrimaryClick(event)) dispatch(`discovery ${nextDiscoveryLevel(state.prototype?.discovery ?? 50)}`);
    },
  });
  discoveryControl.add(discovery);
  const notice = text(renderer, "", { fg: COLORS.dim });
  footer.add(discoveryControl);
  footer.add(notice);
  hero.add(footer);

  const queue = panel(renderer, {
    id: "up-next-panel",
    flexGrow: 4,
    flexDirection: "column",
    minWidth: 32,
    gap: 1,
  });
  body.add(queue);
  const queueHeader = new BoxRenderable(renderer, {
    flexDirection: "row",
    justifyContent: "space-between",
    alignItems: "center",
    height: 2,
  });
  const queueTitle = text(renderer, "UP NEXT", {
    fg: COLORS.text,
    attributes: TextAttributes.BOLD,
  });
  const queueCount = text(renderer, "", { fg: COLORS.dim });
  queueHeader.add(queueTitle);
  queueHeader.add(queueCount);
  queue.add(queueHeader);

  const queueLead = text(renderer, "CURATED FROM YOUR LOCAL RADIO SIGNAL", {
    fg: COLORS.dim,
  });
  queue.add(queueLead);
  const queueList = new BoxRenderable(renderer, {
    id: "queue-list",
    flexDirection: "column",
    flexGrow: 1,
    gap: 1,
    overflow: "hidden",
  });
  queue.add(queueList);

  const queueEmpty = text(renderer, "Queue is waiting for the next signal.", {
    fg: COLORS.dim,
    wrapMode: "word",
    truncate: false,
  });

  let renderedQueueFingerprint: string | undefined;
  let compactLayout = false;

  const send = (command: string): void => {
    if (disposed) return;
    const sent = options.transport.send(command);
    state = {
      ...state,
      notice: sent ? `Command sent  /  ${command}` : "Disconnected  /  command not sent",
    };
    refresh();
  };

  const dispatch = (command: string): void => {
    if (command === "quit") {
      quit();
      return;
    }
    send(command);
  };

  const keyHandler = (key: { name?: string; sequence?: string; ctrl?: boolean; meta?: boolean }): void => {
    const command = commandForKey(key, state.prototype?.discovery ?? 50);
    if (command) dispatch(command);
  };
  renderer.keyInput.on("keypress", keyHandler);

  const refreshQueue = (): void => {
    const tracks = state.prototype?.up_next ?? [];
    const fingerprint = `${compactLayout}:${JSON.stringify(tracks.map((track) => [track.id, track.title, track.duration]))}`;
    if (renderedQueueFingerprint === fingerprint) return;
    renderedQueueFingerprint = fingerprint;
    for (const child of queueList.getChildren()) queueList.remove(child);
    queueCount.content = `${tracks.length} TRACK${tracks.length === 1 ? "" : "S"}`;
    if (!tracks.length) {
      queueList.add(queueEmpty);
      return;
    }
    tracks.slice(0, 7).forEach((track, index) => {
      const row = new BoxRenderable(renderer, {
        height: compactLayout ? 3 : 4,
        flexDirection: "row",
        alignItems: "center",
        gap: 1,
        border: true,
        borderColor: index === 0 ? COLORS.accentDim : COLORS.borderSoft,
        paddingX: 1,
        flexShrink: 0,
      });
      row.add(text(renderer, String(index + 1).padStart(2, "0"), {
        width: 3,
        fg: index === 0 ? COLORS.accent : COLORS.dim,
      }));
      const detail = new BoxRenderable(renderer, {
        flexDirection: "column",
        flexGrow: 1,
        minWidth: 10,
      });
      detail.add(text(renderer, track.title, {
        fg: COLORS.text,
        attributes: index === 0 ? TextAttributes.BOLD : TextAttributes.NONE,
      }));
      detail.add(text(renderer, `${track.artists.join(" • ")}  /  ${track.album}`, {
        fg: COLORS.muted,
        visible: !compactLayout,
      }));
      row.add(detail);
      row.add(text(renderer, formatTime(track.duration), { fg: COLORS.dim }));
      queueList.add(row);
    });
  };

  const livePosition = (nowMs: number): number => {
    const duration = state.playable?.duration ?? 0;
    if (state.mode.kind !== "playing") return Math.min(duration, state.positionMs);
    return Math.min(duration, state.positionMs + Math.max(0, nowMs - state.receivedAtMs));
  };

  const refresh = (): void => {
    const nowMs = Date.now();
    const playable = state.playable;
    const position = livePosition(nowMs);
    const duration = playable?.duration ?? 0;
    const ratio = duration > 0 ? Math.min(1, position / duration) : 0;
    const barWidth = Math.max(20, Math.min(76, (renderer.width ?? 80) - 30));
    const filledWidth = Math.round(barWidth * ratio);
    const filled = "━".repeat(filledWidth);
    const empty = "─".repeat(Math.max(0, barWidth - filledWidth));

    title.content = playableTitle(playable);
    artists.content = playableArtists(playable);
    album.content = `ALBUM  /  ${playableAlbum(playable)}`;
    statusText.content = `${modeIcon(state)}  ${modeLabel(state)}${state.prototype?.radio_waiting ? "  ·  WAITING FOR TRACKS" : ""}`;
    const playLabel = playButton.getChildren()[0];
    if (playLabel instanceof TextRenderable) {
      playLabel.content = compactLayout
        ? `${modeIcon(state)} ${isPlaying(state) ? "PAUSE" : "PLAY"}`
        : `${modeIcon(state)}  ${isPlaying(state) ? "PAUSE" : "PLAY"}`;
    }
    coverInitials.content = initials(playable);
    progressBar.content = t`${fg(COLORS.accent)(filled)}${fg(COLORS.border)(empty)}`;
    progressLabel.content = `${formatTime(position)}  /  ${formatTime(duration)}${state.prototype?.volume_percent !== undefined ? `   VOL ${state.prototype.volume_percent}%` : ""}`;
    connection.content = state.connected ? "LIVE SOCKET" : state.connectionMessage.toUpperCase();
    connectionDot.fg = state.connected ? COLORS.teal : COLORS.red;
    discovery.content = `DISCOVERY  ${state.prototype?.discovery ?? 50}%  ·  D`;
    notice.content = state.notice;
    refreshQueue();
    renderer.requestRender();
  };

  const onResize = (width: number, height = renderer.height): void => {
    const stacked = width < 64;
    compactLayout = height < 29;
    body.flexDirection = stacked ? "column" : "row";
    hero.minWidth = stacked ? 0 : compactLayout ? 0 : 52;
    queue.minWidth = stacked ? 0 : compactLayout ? 0 : 32;
    cover.width = width < 72 ? 21 : width < 102 ? 25 : 30;
    cover.height = width < 72 ? 11 : 15;
    heroTop.minHeight = compactLayout ? 5 : width < 72 ? 11 : 15;
    heroTop.height = compactLayout ? 5 : "auto";
    cover.visible = !compactLayout && width >= 64;
    waveformCaption.visible = !compactLayout;
    hero.gap = compactLayout ? 0 : 1;
    queue.gap = compactLayout ? 0 : 1;
    queueList.gap = compactLayout ? 0 : 1;
    progressBox.gap = compactLayout ? 0 : 1;
    footer.height = compactLayout ? 2 : 3;
    discoveryControl.height = compactLayout ? 2 : 3;
    notice.visible = !compactLayout;
    const buttonWidths = compactLayout ? [8, 11, 8, 10] : [11, 15, 11, 13];
    [previousButton, playButton, nextButton, radioButton].forEach((button, index) => {
      button.width = buttonWidths[index];
      button.height = 3;
    });
    setButtonLabel(previousButton, compactLayout ? "‹" : "‹  PREV");
    setButtonLabel(playButton, compactLayout ? `${modeIcon(state)} ${isPlaying(state) ? "PAUSE" : "PLAY"}` : `${modeIcon(state)}  ${isPlaying(state) ? "PAUSE" : "PLAY"}`);
    setButtonLabel(nextButton, compactLayout ? "›" : "NEXT  ›");
    setButtonLabel(radioButton, compactLayout ? "◈" : "RADIO  ◈");
    if (stacked && !compactLayout) {
      hero.height = 25;
      queue.height = 12;
    } else {
      hero.height = "auto";
      queue.height = "auto";
    }
    shortcutHint.visible = width >= 105;
    refreshQueue();
    renderer.requestRender();
  };
  renderer.on("resize", onResize);

  const animationTimer = setInterval(() => {
    if (disposed) return;
    wavePhase = (wavePhase + 1) % WAVE_HEIGHTS.length;
    waveformText.content = waveform(Math.max(24, Math.min(76, renderer.width - 30)), wavePhase);
    refresh();
  }, 180);

  const setStatus = (next: ParsedStatus): void => {
    if (disposed) return;
    const receivedAtMs = Date.now();
    state = {
      ...state,
      ...next,
      receivedAtMs,
      positionMs: positionAt(next, receivedAtMs),
      notice: "Status synchronized  /  ncspot IPC",
    };
    refresh();
  };

  const setConnection = (connectedState: boolean, message: string): void => {
    if (disposed) return;
    state = { ...state, connected: connectedState, connectionMessage: message };
    refresh();
  };

  const quit = (): void => {
    if (disposed) return;
    disposed = true;
    clearInterval(animationTimer);
    renderer.keyInput.off("keypress", keyHandler);
    options.transport.close();
    renderer.destroy();
    options.onQuit?.();
  };

  const dispose = (): void => {
    if (disposed) return;
    disposed = true;
    clearInterval(animationTimer);
    renderer.keyInput.off("keypress", keyHandler);
    options.transport.close();
  };

  const controller: ResonanceApp = {
    get state() {
      return state;
    },
    setStatus,
    setConnection,
    tick(nowMs = Date.now()) {
      if (disposed) return;
      // The actual position is derived from the status timestamp on refresh;
      // tick exists for tests and for callers that want an immediate redraw.
      if (state.mode.kind === "playing" && state.playable?.duration) {
        const position = livePosition(nowMs);
        if (position >= state.playable.duration && options.transport instanceof DemoTransport) {
          const currentIndex = Math.max(0, DEMO_TRACKS.findIndex((track) => track.id === state.playable?.id));
          setStatus({
            mode: { kind: "playing", startedAtMs: nowMs },
            playable: DEMO_TRACKS[(currentIndex + 1) % DEMO_TRACKS.length],
            prototype: {
              position_ms: 0,
              discovery: state.prototype?.discovery ?? 50,
              volume_percent: state.prototype?.volume_percent ?? 68,
              radio_active: state.prototype?.radio_active ?? false,
              radio_waiting: false,
              up_next: DEMO_TRACKS.slice((currentIndex + 2) % DEMO_TRACKS.length).concat(
                DEMO_TRACKS.slice(0, (currentIndex + 2) % DEMO_TRACKS.length),
              ),
            },
          });
        } else {
          refresh();
        }
      } else {
        refresh();
      }
    },
    quit,
    dispose,
  };

  onResize(renderer.width, renderer.height);
  refresh();
  return controller;
}

// Keep the demo queue available to the app module's type checker without
// making callers import status fixtures directly.
export type { Playable, Track };
