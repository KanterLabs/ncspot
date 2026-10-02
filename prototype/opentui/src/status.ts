/**
 * The small part of ncspot's IPC status that the Resonance view needs.
 *
 * The Rust side deliberately serializes its existing model without an
 * adapter, so this parser accepts the exact serde shape while keeping the UI
 * independent from the rest of ncspot's data model.
 */

export interface Track {
  /** Track is also used for queue entries, which do not carry a `type` key. */
  type?: "Track";
  id?: string | null;
  uri?: string;
  title: string;
  artists: string[];
  album: string;
  duration: number;
  cover_url?: string | null;
}

export interface Episode {
  type: "Episode";
  id?: string;
  uri?: string;
  name: string;
  duration: number;
  cover_url?: string | null;
}

export type Playable = Track | Episode;

/** Provenance for one item in the backend's eligible up-next window. */
export type UpNextOrigin = "explicit" | "radio" | "context";

export type PlayerMode =
  | { kind: "playing"; startedAtMs: number }
  | { kind: "paused"; positionMs: number }
  | { kind: "stopped" }
  | { kind: "finished" };

export interface PrototypeStatus {
  position_ms: number;
  discovery: number;
  volume_percent: number;
  radio_active: boolean;
  radio_waiting: boolean;
  up_next: Track[];
  /** Parallel to `up_next`; absent on older backends. */
  up_next_origins?: UpNextOrigin[];
  audio?: { bands: number[]; level: number; pulse: number; tempo: number | null };
}

export interface ParsedStatus {
  mode: PlayerMode;
  playable: Playable | null;
  prototype: PrototypeStatus | null;
  notifications?: Array<{ id: number; message: string; created_at_ms: number }>;
}

export interface UiState extends ParsedStatus {
  connected: boolean;
  connectionMessage: string;
  receivedAtMs: number;
  /** Position at `receivedAtMs`; playing time advances locally between IPC updates. */
  positionMs: number;
  notice: string;
}

const EMPTY_PROTOTYPE: PrototypeStatus = {
  position_ms: 0,
  discovery: 50,
  volume_percent: 68,
  radio_active: false,
  radio_waiting: false,
  up_next: [],
  up_next_origins: [],
};

const clamp = (value: number, min: number, max: number): number =>
  Math.min(max, Math.max(min, value));

const finiteNumber = (value: unknown, fallback = 0): number => {
  if (typeof value === "number" && Number.isFinite(value)) return value;
  return fallback;
};

const nonNegativeInt = (value: unknown, fallback = 0): number =>
  Math.max(0, Math.round(finiteNumber(value, fallback)));

const objectRecord = (value: unknown): Record<string, unknown> | null =>
  value !== null && typeof value === "object" && !Array.isArray(value)
    ? (value as Record<string, unknown>)
    : null;

function parseDuration(value: unknown): number {
  const raw = objectRecord(value);
  if (!raw) return nonNegativeInt(value);

  const seconds = nonNegativeInt(raw.secs, nonNegativeInt(raw.secs_since_epoch));
  const nanos = clamp(nonNegativeInt(raw.nanos, nonNegativeInt(raw.nanos_since_epoch)), 0, 999_999_999);
  return seconds * 1000 + Math.floor(nanos / 1_000_000);
}

function parseSystemTime(value: unknown): number {
  const raw = objectRecord(value);
  if (!raw) {
    const number = finiteNumber(value, 0);
    // A numeric Playing value is most useful when interpreted as epoch ms;
    // accepting seconds here makes hand-written demo fixtures convenient.
    return number > 10_000_000_000 ? number : number * 1000;
  }

  const seconds = nonNegativeInt(raw.secs_since_epoch, nonNegativeInt(raw.secs));
  const nanos = clamp(
    nonNegativeInt(raw.nanos_since_epoch, nonNegativeInt(raw.nanos)),
    0,
    999_999_999,
  );
  return seconds * 1000 + Math.floor(nanos / 1_000_000);
}

function parseArtists(value: unknown): string[] {
  if (!Array.isArray(value)) return [];
  return value.filter((artist): artist is string => typeof artist === "string" && artist.length > 0);
}

function parseUpNextOrigins(value: unknown): UpNextOrigin[] {
  if (!Array.isArray(value)) return [];
  const origins = value.map(origin =>
    origin === "explicit" || origin === "radio" || origin === "context" ? origin : undefined,
  );
  return origins.every((origin): origin is UpNextOrigin => origin !== undefined) ? origins : [];
}

/** Parse a Track as serialized by ncspot, including queue entries without `type`. */
export function parseTrack(value: unknown): Track | null {
  const raw = objectRecord(value);
  if (!raw || (raw.type !== undefined && raw.type !== "Track")) return null;

  const title = typeof raw.title === "string" && raw.title.trim() ? raw.title : "Unknown track";
  return {
    type: "Track",
    id: typeof raw.id === "string" ? raw.id : null,
    uri: typeof raw.uri === "string" ? raw.uri : undefined,
    title,
    artists: parseArtists(raw.artists),
    album: typeof raw.album === "string" && raw.album.trim() ? raw.album : "Single",
    duration: nonNegativeInt(raw.duration),
    cover_url: typeof raw.cover_url === "string" ? raw.cover_url : null,
  };
}

function parsePlayable(value: unknown): Playable | null {
  const raw = objectRecord(value);
  if (!raw) return null;
  if (raw.type === "Episode") {
    return {
      type: "Episode",
      id: typeof raw.id === "string" ? raw.id : undefined,
      uri: typeof raw.uri === "string" ? raw.uri : undefined,
      name: typeof raw.name === "string" && raw.name.trim() ? raw.name : "Unknown episode",
      duration: nonNegativeInt(raw.duration),
      cover_url: typeof raw.cover_url === "string" ? raw.cover_url : null,
    };
  }
  return parseTrack(raw);
}

function parseMode(value: unknown): PlayerMode {
  const raw = objectRecord(value);
  if (!raw) return { kind: "stopped" };

  if (Object.prototype.hasOwnProperty.call(raw, "Playing")) {
    return { kind: "playing", startedAtMs: parseSystemTime(raw.Playing) };
  }
  if (Object.prototype.hasOwnProperty.call(raw, "Paused")) {
    return { kind: "paused", positionMs: parseDuration(raw.Paused) };
  }
  if (Object.prototype.hasOwnProperty.call(raw, "FinishedTrack")) return { kind: "finished" };
  return { kind: "stopped" };
}

function parsePrototype(value: unknown): PrototypeStatus | null {
  const raw = objectRecord(value);
  if (!raw) return null;

  return {
    position_ms: nonNegativeInt(raw.position_ms),
    discovery: clamp(nonNegativeInt(raw.discovery, EMPTY_PROTOTYPE.discovery), 0, 100),
    volume_percent: clamp(nonNegativeInt(raw.volume_percent, EMPTY_PROTOTYPE.volume_percent), 0, 100),
    radio_active: raw.radio_active === true,
    radio_waiting: raw.radio_waiting === true,
    up_next: Array.isArray(raw.up_next)
      ? raw.up_next.map(parseTrack).filter((track): track is Track => track !== null)
      : [],
    up_next_origins: parseUpNextOrigins(raw.up_next_origins),
    ...(objectRecord(raw.audio) && Array.isArray(objectRecord(raw.audio)!.bands) ? { audio: {
      bands: (objectRecord(raw.audio)!.bands as unknown[]).map(value => clamp(finiteNumber(value), 0, 1)),
      level: clamp(finiteNumber(objectRecord(raw.audio)!.level), 0, 1),
      pulse: clamp(finiteNumber(objectRecord(raw.audio)!.pulse), 0, 1),
      tempo: typeof objectRecord(raw.audio)!.tempo === "number" ? finiteNumber(objectRecord(raw.audio)!.tempo) : null,
    } } : {}),
  };
}

/**
 * Parse one newline-delimited status message. Malformed lines return null so
 * a stray line cannot take down the socket reader or renderer.
 */
export function parseStatus(line: string | unknown): ParsedStatus | null {
  let value: unknown = line;
  if (typeof line === "string") {
    try {
      value = JSON.parse(line);
    } catch {
      return null;
    }
  }

  const raw = objectRecord(value);
  if (!raw || !("mode" in raw)) return null;

  const mode = parseMode(raw.mode);
  const playable = parsePlayable(raw.playable);
  const prototype = parsePrototype(raw.prototype);
  const positionMs = prototype?.position_ms ?? (mode.kind === "paused" ? mode.positionMs : 0);

  return {
    mode,
    playable,
    prototype,
    ...(Array.isArray(raw.notifications) ? { notifications: raw.notifications.filter((entry): entry is {id:number;message:string;created_at_ms:number} => {
      const notice = objectRecord(entry);
      return notice !== null && typeof notice.id === "number" && typeof notice.message === "string" && typeof notice.created_at_ms === "number";
    }) } : {}),
  };
}

/** Position represented by a status at `nowMs`, clamped to the current item's duration. */
export function positionAt(status: ParsedStatus, nowMs = Date.now()): number {
  const duration = status.playable?.duration ?? Number.MAX_SAFE_INTEGER;
  const base = status.prototype?.position_ms ??
    (status.mode.kind === "paused"
      ? status.mode.positionMs
      : status.mode.kind === "playing"
        ? Math.max(0, nowMs - status.mode.startedAtMs)
        : 0);
  return clamp(Math.round(base), 0, duration);
}

export function playableTitle(playable: Playable | null): string {
  if (!playable) return "Nothing queued";
  return playable.type === "Episode" ? playable.name : playable.title;
}

export function playableArtists(playable: Playable | null): string {
  if (!playable) return "Waiting for a signal";
  return playable.type === "Episode" ? "Podcast episode" : playable.artists.join(" • ") || "Unknown artist";
}

export function playableAlbum(playable: Playable | null): string {
  if (!playable) return "—";
  return playable.type === "Episode" ? "Episode" : playable.album || "Single";
}

export function initials(playable: Playable | null): string {
  const source = playableTitle(playable)
    .split(/\s+/)
    .filter(Boolean)
    .slice(0, 2)
    .map((word) => word[0]?.toUpperCase() ?? "")
    .join("");
  return source || "RL";
}

export function formatTime(ms: number): string {
  const seconds = Math.max(0, Math.floor(ms / 1000));
  const minutes = Math.floor(seconds / 60);
  return `${minutes}:${String(seconds % 60).padStart(2, "0")}`;
}

export function createUiState(overrides: Partial<UiState> = {}): UiState {
  const base: UiState = {
    mode: { kind: "stopped" },
    playable: null,
    prototype: null,
    connected: false,
    connectionMessage: "offline preview",
    receivedAtMs: Date.now(),
    positionMs: 0,
    notice: "Press space to play · q to close",
  };
  return { ...base, ...overrides };
}

export const DEMO_TRACKS: Track[] = [
  {
    type: "Track",
    id: "demo-1",
    title: "The Colour of Air",
    artists: ["KanterLabs", "Lumen Field"],
    album: "Resonance Studies",
    duration: 232_000,
    cover_url: null,
  },
  {
    type: "Track",
    id: "demo-2",
    title: "Soft Geometry",
    artists: ["Nara Sato"],
    album: "Night Signals",
    duration: 197_000,
    cover_url: null,
  },
  {
    type: "Track",
    id: "demo-3",
    title: "Low Tide Memory",
    artists: ["Drift Assembly"],
    album: "Coastal Machines",
    duration: 264_000,
    cover_url: null,
  },
  {
    type: "Track",
    id: "demo-4",
    title: "Afterimage / 04",
    artists: ["Mira Vale"],
    album: "Liminal FM",
    duration: 218_000,
    cover_url: null,
  },
];

export function demoStatus(index = 0, positionMs = 82_000): ParsedStatus {
  const current = DEMO_TRACKS[index % DEMO_TRACKS.length];
  return {
    mode: { kind: "playing", startedAtMs: Date.now() - positionMs },
    playable: current,
    prototype: {
      ...EMPTY_PROTOTYPE,
      position_ms: positionMs,
      up_next: DEMO_TRACKS.slice((index + 1) % DEMO_TRACKS.length).concat(
        DEMO_TRACKS.slice(0, (index + 1) % DEMO_TRACKS.length),
      ),
    },
  };
}
