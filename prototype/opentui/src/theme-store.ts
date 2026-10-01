import {
  chmodSync,
  closeSync,
  fsyncSync,
  mkdirSync,
  openSync,
  readFileSync,
  renameSync,
  unlinkSync,
  writeSync,
} from "node:fs";
import { homedir } from "node:os";
import { dirname, isAbsolute, join } from "node:path";
import { randomBytes } from "node:crypto";
import { DEFAULT_THEME, isThemeName, type ThemeName } from "./theme.js";

export const THEME_CONFIG_FILE = "opentui-theme.json";
export const THEME_CONFIG_VERSION = 1;

export interface ThemePreference {
  theme: ThemeName;
  /** True when a save may safely update the existing JSON object. */
  writable: boolean;
  reason?: string;
}

export interface ThemeStoreOptions {
  configPath?: string;
}

export interface ThemeSaveResult {
  ok: boolean;
  path: string;
  error?: string;
}

type JsonObject = Record<string, unknown>;

function isJsonObject(value: unknown): value is JsonObject {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

/** Resolve the frontend-only preference path without writing anything. */
export function themeConfigPath(env: NodeJS.ProcessEnv = process.env, home = homedir()): string {
  const requestedRoot = env.XDG_CONFIG_HOME?.trim();
  const configRoot = requestedRoot && isAbsolute(requestedRoot) ? requestedRoot : join(home, ".config");
  return join(configRoot, "resonance", THEME_CONFIG_FILE);
}

function pathFor(options: ThemeStoreOptions = {}): string {
  return options.configPath ?? themeConfigPath();
}

function parseObject(value: unknown): ThemePreference {
  if (!isJsonObject(value)) {
    return { theme: DEFAULT_THEME, writable: false, reason: "theme preference is not a JSON object" };
  }

  const version = value.version;
  if (
    version !== undefined &&
    (typeof version !== "number" || !Number.isFinite(version) || !Number.isInteger(version) || version < 0)
  ) {
    return { theme: DEFAULT_THEME, writable: false, reason: "theme preference version is invalid" };
  }
  if (typeof version === "number" && version > THEME_CONFIG_VERSION) {
    return {
      theme: DEFAULT_THEME,
      writable: false,
      reason: `theme preference version ${version} is newer than supported version ${THEME_CONFIG_VERSION}`,
    };
  }
  if (value.theme !== undefined && !isThemeName(value.theme)) {
    return { theme: DEFAULT_THEME, writable: false, reason: "theme preference has an invalid theme" };
  }
  return { theme: value.theme ?? DEFAULT_THEME, writable: true };
}

/** Validate a decoded preference object. Malformed input always falls back light. */
export function parseThemePreference(value: unknown): ThemePreference {
  return parseObject(value);
}

/** Read the preference without creating a file or directory. */
export function readThemePreference(options: ThemeStoreOptions = {}): ThemePreference {
  const path = pathFor(options);
  let source: string;
  try {
    source = readFileSync(path, "utf8");
  } catch (error) {
    const code = error && typeof error === "object" && "code" in error ? error.code : undefined;
    if (code === "ENOENT") return { theme: DEFAULT_THEME, writable: true };
    return {
      theme: DEFAULT_THEME,
      writable: false,
      reason: `unable to read theme preference: ${error instanceof Error ? error.message : String(error)}`,
    };
  }
  try {
    return parseObject(JSON.parse(source));
  } catch {
    return { theme: DEFAULT_THEME, writable: false, reason: "theme preference is malformed JSON" };
  }
}

function readExistingObject(path: string): { object: JsonObject; error?: undefined } | { object?: undefined; error: string } {
  let source: string;
  try {
    source = readFileSync(path, "utf8");
  } catch (error) {
    const code = error && typeof error === "object" && "code" in error ? error.code : undefined;
    if (code === "ENOENT") return { object: {} };
    return { error: `unable to read theme preference: ${error instanceof Error ? error.message : String(error)}` };
  }
  let decoded: unknown;
  try {
    decoded = JSON.parse(source);
  } catch {
    return { error: "refusing to overwrite malformed theme preference" };
  }
  if (!isJsonObject(decoded)) return { error: "refusing to overwrite non-object theme preference" };
  const parsed = parseObject(decoded);
  if (!parsed.writable) return { error: `refusing to overwrite ${parsed.reason ?? "invalid theme preference"}` };
  return { object: decoded };
}

/**
 * Persist one theme atomically. Existing JSON fields are preserved so a newer
 * frontend can carry metadata forward; invalid or future-version files are
 * never overwritten by an explicit toggle.
 */
export function saveThemePreference(theme: ThemeName, options: ThemeStoreOptions = {}): ThemeSaveResult {
  const path = pathFor(options);
  if (!isThemeName(theme)) return { ok: false, path, error: "invalid theme" };

  const existing = readExistingObject(path);
  if (existing.error) return { ok: false, path, error: existing.error };

  const next = { ...existing.object, theme };
  const directory = dirname(path);
  const temporaryPath = `${path}.tmp-${process.pid}-${randomBytes(8).toString("hex")}`;
  let descriptor: number | undefined;
  try {
    mkdirSync(directory, { recursive: true, mode: 0o700 });
    descriptor = openSync(temporaryPath, "wx", 0o600);
    const bytes = Buffer.from(`${JSON.stringify(next, null, 2)}\n`, "utf8");
    let offset = 0;
    while (offset < bytes.length) {
      const written = writeSync(descriptor, bytes, offset, bytes.length - offset);
      if (written <= 0) throw new Error("unable to write theme preference");
      offset += written;
    }
    fsyncSync(descriptor);
    closeSync(descriptor);
    descriptor = undefined;
    chmodSync(temporaryPath, 0o600);
    renameSync(temporaryPath, path);
    // rename preserves the temporary file's private mode on POSIX.
    chmodSync(path, 0o600);
    return { ok: true, path };
  } catch (error) {
    if (descriptor !== undefined) {
      try {
        closeSync(descriptor);
      } catch {
        // Preserve the original write error.
      }
    }
    try {
      unlinkSync(temporaryPath);
    } catch {
      // The temporary file may already have been renamed or never created.
    }
    return { ok: false, path, error: error instanceof Error ? error.message : String(error) };
  }
}

// Concise aliases make the store convenient for callers and tests while the
// explicit names above describe their read/write behavior.
export const loadThemePreference = readThemePreference;
export const persistThemePreference = saveThemePreference;
