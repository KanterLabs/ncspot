import { readFileSync, mkdirSync, writeFileSync, renameSync, unlinkSync } from "node:fs";
import { dirname, join } from "node:path";
import { randomBytes } from "node:crypto";
import { themeConfigPath } from "../theme-store.js";

const path = () => join(dirname(themeConfigPath()), "opentui-workspace.json");
export function readReducedMotion(configPath = path()): boolean {
  try {
    const value = JSON.parse(readFileSync(configPath, "utf8"));
    return !!value && typeof value === "object" && !Array.isArray(value)
      && (value.version === undefined || value.version === 1) && value.reducedMotion === true;
  } catch { return false; }
}
export function saveReducedMotion(value: boolean, configPath = path()): void {
  let existing: Record<string, unknown> = {};
  try {
    const decoded = JSON.parse(readFileSync(configPath, "utf8"));
    if (!decoded || typeof decoded !== "object" || Array.isArray(decoded) || (decoded.version !== undefined && decoded.version !== 1)) throw new Error("Unsupported appearance preference; file preserved");
    existing = decoded;
  } catch (error: any) { if (error.code !== "ENOENT") throw error; }
  mkdirSync(dirname(configPath), { recursive: true, mode: 0o700 });
  const temporary = `${configPath}.${randomBytes(8).toString("hex")}.tmp`;
  try {
    writeFileSync(temporary, JSON.stringify({ ...existing, version: 1, reducedMotion: value }) + "\n", { mode: 0o600, flag: "wx" });
    renameSync(temporary, configPath);
  } finally { try { unlinkSync(temporary); } catch {} }
}
