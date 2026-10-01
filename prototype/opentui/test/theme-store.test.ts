import { expect, test } from "bun:test";
import { mkdirSync, mkdtempSync, readFileSync, rmSync, statSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { tmpdir } from "node:os";
import {
  parseThemePreference,
  readThemePreference,
  saveThemePreference,
  themeConfigPath,
} from "../src/theme-store.js";

function withTempPath(run: (path: string) => void): void {
  const directory = mkdtempSync(join(tmpdir(), "resonance-opentui-theme-"));
  try {
    run(join(directory, "resonance", "opentui-theme.json"));
  } finally {
    rmSync(directory, { recursive: true, force: true });
  }
}

test("theme preference defaults light and writes an atomic private file", () => {
  withTempPath((path) => {
    expect(readThemePreference({ configPath: path }).theme).toBe("light");
    expect(saveThemePreference("dark", { configPath: path }).ok).toBe(true);
    expect(readThemePreference({ configPath: path }).theme).toBe("dark");
    expect(statSync(path).mode & 0o777).toBe(0o600);
    expect(JSON.parse(readFileSync(path, "utf8"))).toEqual({ theme: "dark" });
  });
});

test("malformed and future preferences fall back light and are never overwritten", () => {
  withTempPath((path) => {
    mkdirSync(dirname(path), { recursive: true });
    writeFileSync(path, "{ malformed", "utf8");
    expect(readThemePreference({ configPath: path }).theme).toBe("light");
    expect(saveThemePreference("dark", { configPath: path }).ok).toBe(false);
    expect(readFileSync(path, "utf8")).toBe("{ malformed");

    const future = JSON.stringify({ theme: "dark", version: 99, future: true });
    writeFileSync(path, future, "utf8");
    expect(readThemePreference({ configPath: path }).theme).toBe("light");
    expect(saveThemePreference("light", { configPath: path }).ok).toBe(false);
    expect(readFileSync(path, "utf8")).toBe(future);
  });
});

test("theme updates preserve valid preference metadata", () => {
  withTempPath((path) => {
    mkdirSync(dirname(path), { recursive: true });
    writeFileSync(path, JSON.stringify({ theme: "light", version: 1, retained: { source: "test" } }), "utf8");
    expect(saveThemePreference("dark", { configPath: path }).ok).toBe(true);
    expect(JSON.parse(readFileSync(path, "utf8"))).toEqual({
      theme: "dark",
      version: 1,
      retained: { source: "test" },
    });
  });
});

test("preference parser rejects malformed versions and relative XDG roots", () => {
  expect(parseThemePreference({ theme: "dark", version: -1 }).theme).toBe("light");
  expect(parseThemePreference({ theme: "dark", version: 1.5 }).theme).toBe("light");
  expect(themeConfigPath({ XDG_CONFIG_HOME: "relative" }, "/tmp/resonance-home")).toBe(
    "/tmp/resonance-home/.config/resonance/opentui-theme.json",
  );
  expect(themeConfigPath({ XDG_CONFIG_HOME: "/tmp/resonance-config" }, "/tmp/resonance-home")).toBe(
    "/tmp/resonance-config/resonance/opentui-theme.json",
  );
});
