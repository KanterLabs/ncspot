import { mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { RGBA, TextAttributes, type CliRenderer } from "@opentui/core";
import { createTestRenderer } from "@opentui/core/testing";
import { mountWorkspace } from "../src/workspace/app.js";
import { DemoApi } from "../src/workspace/demo.js";
import { demoStatus, type ParsedStatus } from "../src/status.js";
import { isThemeName, type ThemeName } from "../src/theme.js";
import { ROUTES, type Params, type Route, type RpcApi } from "../src/workspace/contracts.js";

/**
 * Capture the real OpenTUI native cell buffer for visual review.
 *
 * This is intentionally a runnable utility rather than a snapshot test. It
 * mounts the same workspace used by the application, drives it with a fixed
 * offline fixture, and writes every terminal cell with its resolved
 * character, foreground/background color, and attributes. The SVG and text
 * files are derivatives of that native cell JSON.
 */

const DEFAULT_OUTPUT = "/tmp/resonance-visual";
const DEFAULT_SIZES: readonly CaptureSize[] = [
  { width: 189, height: 34 },
  { width: 80, height: 24 },
];
const DEFAULT_ROUTE: Route = "now-playing";
const CELL_WIDTH = 10;
const CELL_HEIGHT = 20;
const CHAR_CONTINUATION_MASK = 0xc0000000;
const CHAR_CONTINUATION_FLAG = 0xc0000000;

interface CaptureSize {
  width: number;
  height: number;
}

interface CliOptions {
  output: string;
  sizes: CaptureSize[];
  themes: ThemeName[];
  route: Route;
  artworkMethod: string;
  artworkFixture?: ArtworkSource;
  statusFixture?: ParsedStatus;
}

interface CellColor {
  r: number;
  g: number;
  b: number;
  a: number;
  hex: string;
  /** Raw native RGBA words, retaining indexed/default-color metadata. */
  packed: [number, number, number, number];
}

export interface NativeCell {
  x: number;
  y: number;
  /** Empty for a native wide-character continuation cell. */
  char: string;
  /** The raw native character value, including wide-character flags. */
  rawChar: number;
  continuation: boolean;
  fg: CellColor;
  bg: CellColor;
  attributes: number;
  bold: boolean;
}

export interface VisualCaptureArtifact {
  version: 1;
  renderer: "@opentui/core/testing currentRenderBuffer";
  width: number;
  height: number;
  theme: ThemeName;
  route: Route;
  cellWidth: number;
  cellHeight: number;
  frame: string;
  cells: NativeCell[];
  rpcCalls: Array<{ method: string; params?: Params; artworkFixture?: boolean }>;
  artwork: {
    method: string;
    response: ArtworkResult | null;
    requestCount: number;
  };
}

interface ArtworkResult {
  available: true;
  uri: string;
  width: number;
  height: number;
  /** Two RGB pixel rows per terminal cell row, as required by player.artwork. */
  pixels: string[];
  source: "offline visual fixture" | "json artwork fixture";
  fixture: true;
}

interface ArtworkSource {
  width: number;
  height: number;
  pixels: string[];
}

function usage(): string {
  return [
    "Usage: bun run test/visual-capture.ts [options]",
    "",
    "Options:",
    `  --output DIR             Output directory (default ${DEFAULT_OUTPUT})`,
    "  --size WIDTHxHEIGHT      Capture one size; repeat for multiple sizes",
    "  --width N --height N     Capture one size",
    "  --theme light|dark|both  Theme(s) to capture (default light)",
    `  --route ROUTE            Workspace route (default ${DEFAULT_ROUTE})`,
    "  --artwork-method METHOD  Fixture RPC method (default player.artwork)",
    "  --artwork-json PATH      Load {width,height,pixels} artwork and resample it",
    "  --status-json PATH       Override the deterministic status fixture",
    "  --help                   Show this help",
  ].join("\n");
}

function positiveInteger(value: string, flag: string): number {
  const parsed = Number(value);
  if (!Number.isSafeInteger(parsed) || parsed < 1) throw new Error(`${flag} must be a positive integer`);
  return parsed;
}

function parseSize(value: string): CaptureSize {
  const match = /^(\d+)x(\d+)$/i.exec(value);
  if (!match) throw new Error(`--size must use WIDTHxHEIGHT, got ${value}`);
  return { width: positiveInteger(match[1]!, "width"), height: positiveInteger(match[2]!, "height") };
}

function record(value: unknown): Record<string, unknown> {
  if (value === null || typeof value !== "object" || Array.isArray(value)) throw new Error("fixture JSON must contain an object");
  return value as Record<string, unknown>;
}

function readArtworkSource(path: string): ArtworkSource {
  let value: unknown;
  try { value = JSON.parse(readFileSync(path, "utf8")); }
  catch (error) { throw new Error(`could not read --artwork-json ${path}: ${error instanceof Error ? error.message : String(error)}`); }
  const raw = record(value);
  const width = raw.width;
  const height = raw.height;
  const pixels = raw.pixels;
  if (!Number.isSafeInteger(width) || (width as number) < 1 || !Number.isSafeInteger(height) || (height as number) < 1 || !Array.isArray(pixels) || pixels.length !== (width as number) * (height as number) * 2 || pixels.some(pixel => typeof pixel !== "string" || !/^#[a-f\d]{6}$/i.test(pixel))) {
    throw new Error(`--artwork-json ${path} must contain width, height, and width*height*2 RGB pixels`);
  }
  return { width: width as number, height: height as number, pixels: [...pixels] as string[] };
}

function readStatusFixture(path: string): ParsedStatus {
  let value: unknown;
  try { value = JSON.parse(readFileSync(path, "utf8")); }
  catch (error) { throw new Error(`could not read --status-json ${path}: ${error instanceof Error ? error.message : String(error)}`); }
  const raw = record(value);
  const base = deterministicStatus();
  const basePrototype = base.prototype;
  const rawPrototype = raw.prototype === undefined || raw.prototype === null ? raw.prototype : record(raw.prototype);
  return {
    ...base,
    ...raw,
    mode: raw.mode === undefined ? base.mode : raw.mode as ParsedStatus["mode"],
    playable: raw.playable === undefined ? base.playable : raw.playable as ParsedStatus["playable"],
    prototype: raw.prototype === undefined
      ? basePrototype
      : raw.prototype === null
        ? null
        : { ...basePrototype, ...rawPrototype } as ParsedStatus["prototype"],
  } as ParsedStatus;
}

function parseArgs(args: readonly string[]): CliOptions | "help" {
  let output = DEFAULT_OUTPUT;
  let sizes: CaptureSize[] = [];
  let themeValue: ThemeName | "both" = "light";
  let route: Route = DEFAULT_ROUTE;
  let artworkMethod = "player.artwork";
  let artworkPath: string | undefined;
  let statusPath: string | undefined;
  let width: number | undefined;
  let height: number | undefined;

  for (let index = 0; index < args.length; index++) {
    const arg = args[index];
    if (arg === "--help" || arg === "-h") return "help";
    if (arg === "--output") { output = args[++index] ?? ""; if (!output) throw new Error("--output requires a directory"); continue; }
    if (arg === "--size") { sizes.push(parseSize(args[++index] ?? "")); continue; }
    if (arg === "--width") { width = positiveInteger(args[++index] ?? "", "--width"); continue; }
    if (arg === "--height") { height = positiveInteger(args[++index] ?? "", "--height"); continue; }
    if (arg === "--theme") {
      const value = args[++index];
      if (value !== "both" && !isThemeName(value)) throw new Error("--theme must be light, dark, or both");
      themeValue = value;
      continue;
    }
    if (arg === "--route") {
      const value = args[++index];
      if (!value || !(ROUTES as readonly string[]).includes(value)) throw new Error(`--route must be one of ${ROUTES.join(", ")}`);
      route = value as Route;
      continue;
    }
    if (arg === "--artwork-method") { artworkMethod = args[++index] ?? ""; if (!artworkMethod) throw new Error("--artwork-method requires a method"); continue; }
    if (arg === "--artwork-json") { artworkPath = args[++index] ?? ""; if (!artworkPath) throw new Error("--artwork-json requires a path"); continue; }
    if (arg === "--status-json") { statusPath = args[++index] ?? ""; if (!statusPath) throw new Error("--status-json requires a path"); continue; }
    throw new Error(`Unknown argument: ${arg}`);
  }

  if (width !== undefined || height !== undefined) {
    if (width === undefined || height === undefined) throw new Error("--width and --height must be used together");
    if (sizes.length) throw new Error("Use either --size or --width/--height, not both");
    sizes = [{ width, height }];
  }
  if (!sizes.length) sizes = [...DEFAULT_SIZES];
  const themes: ThemeName[] = themeValue === "both" ? ["light", "dark"] : [themeValue];
  return {
    output,
    sizes,
    themes,
    route,
    artworkMethod,
    artworkFixture: artworkPath ? readArtworkSource(artworkPath) : undefined,
    statusFixture: statusPath ? readStatusFixture(statusPath) : undefined,
  };
}

function colorFromPacked(values: Uint16Array, offset: number): CellColor {
  const packed: [number, number, number, number] = [
    values[offset]!,
    values[offset + 1]!,
    values[offset + 2]!,
    values[offset + 3]!,
  ];
  const [r, g, b, a] = RGBA.fromArray(new Uint16Array(packed)).toInts();
  const hex = `#${[r, g, b].map(value => value.toString(16).padStart(2, "0")).join("")}`;
  return { r, g, b, a, hex, packed };
}

function cellsFromRenderer(renderer: CliRenderer): { frame: string; cells: NativeCell[] } {
  const buffer = renderer.currentRenderBuffer;
  const frame = new TextDecoder().decode(buffer.getRealCharBytes(true));
  const lines = frame.split("\n");
  const { char, fg, bg, attributes } = buffer.buffers;
  const cells: NativeCell[] = [];

  for (let y = 0; y < buffer.height; y++) {
    const lineChars = [...(lines[y] ?? "")];
    let charIndex = 0;
    for (let x = 0; x < buffer.width; x++) {
      const index = y * buffer.width + x;
      const rawChar = char[index]! >>> 0;
      const continuation = (rawChar & CHAR_CONTINUATION_MASK) === CHAR_CONTINUATION_FLAG;
      const attributesValue = attributes[index]! >>> 0;
      cells.push({
        x,
        y,
        char: continuation ? "" : (lineChars[charIndex++] ?? " "),
        rawChar,
        continuation,
        fg: colorFromPacked(fg, index * 4),
        bg: colorFromPacked(bg, index * 4),
        attributes: attributesValue,
        bold: (attributesValue & TextAttributes.BOLD) !== 0,
      });
    }
  }
  return { frame, cells };
}

class VisualFixtureApi implements RpcApi {
  readonly rpcCalls: Array<{ method: string; params?: Params; artworkFixture?: boolean }> = [];
  readonly artworkResponses: ArtworkResult[] = [];

  constructor(private readonly delegate: DemoApi, private readonly artworkMethod: string, private readonly source?: ArtworkSource) {}

  async call<T = unknown>(method: string, params: Params = {}): Promise<T> {
    const isArtwork = method === this.artworkMethod || /(?:artwork|album[_-]?art|cover)/i.test(method);
    if (isArtwork) {
      this.rpcCalls.push({ method, params: { ...params }, artworkFixture: true });
      const result = this.artwork(params);
      this.artworkResponses.push(result);
      return result as T;
    }
    this.rpcCalls.push({ method, params: { ...params } });
    return this.delegate.call<T>(method, params);
  }

  private artwork(params: Params): ArtworkResult {
    const uri = typeof params.uri === "string" && params.uri ? params.uri : "spotify:album:resonance-studies";
    const requestedWidth = typeof params.width === "number" && Number.isSafeInteger(params.width) && params.width > 0 ? params.width : 20;
    const requestedHeight = typeof params.height === "number" && Number.isSafeInteger(params.height) && params.height > 0 ? params.height : 10;
    const width = Math.min(40, requestedWidth);
    const height = Math.min(20, requestedHeight);
    const pixels = this.source ? resampleArtwork(this.source, width, height) : syntheticArtworkPixels(width, height);
    return {
      available: true,
      uri,
      width,
      height,
      pixels,
      source: this.source ? "json artwork fixture" : "offline visual fixture",
      fixture: true,
    };
  }
}

function resampleArtwork(source: ArtworkSource, width: number, height: number): string[] {
  const targetPixelRows = height * 2;
  const sourcePixelRows = source.height * 2;
  return Array.from({ length: targetPixelRows }, (_, y) => Array.from({ length: width }, (_, x) => {
    const sourceX = Math.min(source.width - 1, Math.floor(x * source.width / width));
    const sourceY = Math.min(sourcePixelRows - 1, Math.floor(y * sourcePixelRows / targetPixelRows));
    return source.pixels[sourceY * source.width + sourceX]!;
  })).flat();
}

/**
 * Make a deterministic, high-contrast RGB fixture for the player.artwork
 * contract. Each terminal row consumes two RGB rows, so the result exercises
 * both foreground and background colors in the native half-block renderer.
 */
function syntheticArtworkPixels(width: number, height: number): string[] {
  const rows = height * 2;
  return Array.from({ length: rows }, (_, y) => Array.from({ length: width }, (_, x) => {
    const u = width === 1 ? 0 : x / (width - 1);
    const v = rows === 1 ? 0 : y / (rows - 1);
    const diagonal = Math.max(0, 1 - Math.abs(u - (0.18 + v * 0.64)) * 5);
    const ring = Math.max(0, 1 - Math.abs(Math.hypot(u - 0.54, v - 0.49) - 0.28) * 9);
    const r = Math.round(20 + 84 * u + 118 * diagonal + 26 * ring);
    const g = Math.round(26 + 58 * (1 - v) + 106 * diagonal + 38 * ring);
    const b = Math.round(56 + 112 * (1 - u) + 76 * v + 62 * ring);
    return `#${[r, g, b].map(value => Math.max(0, Math.min(255, value)).toString(16).padStart(2, "0")).join("")}`;
  })).flat();
}

function deterministicStatus(): ParsedStatus {
  const status = demoStatus(0, 82_000);
  status.mode = { kind: "paused", positionMs: 82_000 };
  status.playable = {
    ...status.playable!,
    cover_url: "offline://resonance-demo-artwork",
  };
  status.prototype = {
    ...status.prototype!,
    position_ms: 82_000,
    volume_percent: 68,
    discovery: 50,
    audio: {
      bands: [0.08, 0.24, 0.5, 0.86, 0.62, 0.34, 0.72, 0.98],
      level: 0.64,
      pulse: 0.35,
      tempo: 128,
    },
  };
  return status;
}

function escapeXml(value: string): string {
  return value.replace(/[&<>"']/g, character => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&apos;" })[character]!);
}

function svgColor(color: CellColor): string {
  return color.hex;
}

function svgOpacity(color: CellColor): string {
  return (color.a / 255).toFixed(4).replace(/0+$/, "").replace(/\.$/, "") || "0";
}

function svgFromArtifact(artifact: VisualCaptureArtifact): string {
  const pixelWidth = artifact.width * artifact.cellWidth;
  const pixelHeight = artifact.height * artifact.cellHeight;
  const cells = artifact.cells.map(cell => {
    const x = cell.x * artifact.cellWidth;
    const y = cell.y * artifact.cellHeight;
    const rect = `<rect x="${x}" y="${y}" width="${artifact.cellWidth}" height="${artifact.cellHeight}" fill="${svgColor(cell.bg)}" fill-opacity="${svgOpacity(cell.bg)}"/>`;
    if (cell.continuation || cell.char === " ") return rect;
    const baseline = y + Math.round(artifact.cellHeight * 0.78);
    const weight = cell.bold ? "700" : "400";
    return `${rect}<text x="${x}" y="${baseline}" fill="${svgColor(cell.fg)}" fill-opacity="${svgOpacity(cell.fg)}" font-family="monospace" font-size="${Math.round(artifact.cellHeight * 0.72)}px" font-weight="${weight}" xml:space="preserve">${escapeXml(cell.char)}</text>`;
  }).join("");
  return [
    `<svg xmlns="http://www.w3.org/2000/svg" width="${pixelWidth}" height="${pixelHeight}" viewBox="0 0 ${pixelWidth} ${pixelHeight}" role="img" aria-label="Resonance ${artifact.width} by ${artifact.height} native terminal capture">`,
    `<rect width="${pixelWidth}" height="${pixelHeight}" fill="#000000"/>`,
    `<g data-source="@opentui/core native currentRenderBuffer" data-cols="${artifact.width}" data-rows="${artifact.height}">${cells}</g>`,
    "</svg>",
  ].join("");
}

function textFromArtifact(artifact: VisualCaptureArtifact): string {
  return artifact.frame.replace(/\n$/, "");
}

function fileStem(size: CaptureSize, theme: ThemeName, route: Route): string {
  return `capture-${size.width}x${size.height}-${theme}-${route}`;
}

async function captureOne(options: CliOptions, size: CaptureSize, theme: ThemeName): Promise<{ stem: string; artifact: VisualCaptureArtifact }> {
  const setup = await createTestRenderer({
    width: size.width,
    height: size.height,
    useMouse: false,
    targetFps: 30,
  });
  const demo = new DemoApi();
  demo.status = options.statusFixture ?? deterministicStatus();
  const api = new VisualFixtureApi(demo, options.artworkMethod, options.artworkFixture);
  const app = mountWorkspace(setup.renderer, {
    api,
    theme,
    route: options.route,
    reducedMotion: true,
  });
  try {
    app.setStatus(demo.status);
    // The first frame mounts the screen; the second observes deterministic
    // async RPC metadata and any artwork request made by the current screen.
    await setup.renderOnce();
    await Promise.resolve();
    await setup.renderOnce();
    await Promise.resolve();
    await setup.renderOnce();
    const captured = cellsFromRenderer(setup.renderer);
    const artwork = api.artworkResponses.at(-1) ?? null;
    const artifact: VisualCaptureArtifact = {
      version: 1,
      renderer: "@opentui/core/testing currentRenderBuffer",
      width: size.width,
      height: size.height,
      theme,
      route: options.route,
      cellWidth: CELL_WIDTH,
      cellHeight: CELL_HEIGHT,
      frame: captured.frame,
      cells: captured.cells,
      rpcCalls: api.rpcCalls,
      artwork: { method: options.artworkMethod, response: artwork, requestCount: api.artworkResponses.length },
    };
    return { stem: fileStem(size, theme, options.route), artifact };
  } finally {
    app.dispose();
    setup.renderer.destroy();
  }
}

async function main(): Promise<void> {
  const parsed = parseArgs(Bun.argv.slice(2));
  if (parsed === "help") { process.stdout.write(`${usage()}\n`); return; }
  mkdirSync(parsed.output, { recursive: true });
  const manifest: Array<{ stem: string; width: number; height: number; theme: ThemeName; route: Route }> = [];
  for (const theme of parsed.themes) {
    for (const size of parsed.sizes) {
      const captured = await captureOne(parsed, size, theme);
      const { stem, artifact } = captured;
      writeFileSync(join(parsed.output, `${stem}.json`), `${JSON.stringify(artifact, null, 2)}\n`);
      writeFileSync(join(parsed.output, `${stem}.svg`), svgFromArtifact(artifact));
      writeFileSync(join(parsed.output, `${stem}.txt`), textFromArtifact(artifact));
      manifest.push({ stem, width: size.width, height: size.height, theme, route: parsed.route });
      process.stdout.write(`${stem}: ${artifact.cells.length} native cells -> ${parsed.output}\n`);
    }
  }
  writeFileSync(join(parsed.output, "manifest.json"), `${JSON.stringify({ version: 1, captures: manifest }, null, 2)}\n`);
}

if (import.meta.main) {
  main().catch(error => {
    process.stderr.write(`visual-capture: ${error instanceof Error ? error.message : String(error)}\n`);
    process.exitCode = 1;
  });
}
