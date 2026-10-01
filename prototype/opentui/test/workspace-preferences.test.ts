import { expect, test } from "bun:test";
import { mkdtempSync, readFileSync, readdirSync, rmSync, statSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { readReducedMotion, saveReducedMotion } from "../src/workspace/preferences.js";

function temporary(run: (path: string, directory: string) => void): void {
  const directory = mkdtempSync(join(tmpdir(), "resonance-workspace-preferences-"));
  try { run(join(directory, "workspace.json"), directory); }
  finally { rmSync(directory, { recursive: true, force: true }); }
}

test("reduced motion atomically updates a populated private preference while preserving metadata", () => {
  temporary((path, directory) => {
    const metadata = { source: "user", layout: { routes: ["queue", "library"], width: 80 }, futureField: null };
    writeFileSync(path, JSON.stringify({ version: 1, reducedMotion: false, ...metadata }), { mode: 0o644 });
    expect(readReducedMotion(path)).toBe(false);
    saveReducedMotion(true, path);
    expect(readReducedMotion(path)).toBe(true);
    expect(JSON.parse(readFileSync(path, "utf8"))).toEqual({ version: 1, reducedMotion: true, ...metadata });
    expect(statSync(path).mode & 0o777).toBe(0o600);
    expect(readdirSync(directory)).toEqual(["workspace.json"]);
    saveReducedMotion(false, path);
    expect(JSON.parse(readFileSync(path, "utf8"))).toEqual({ version: 1, reducedMotion: false, ...metadata });
    expect(statSync(path).mode & 0o777).toBe(0o600);
    expect(readdirSync(directory)).toEqual(["workspace.json"]);
  });
});

test("missing reduced motion preference defaults off and creates a private file", () => {
  temporary((path, directory) => {
    expect(readReducedMotion(path)).toBe(false);
    saveReducedMotion(true, path);
    expect(JSON.parse(readFileSync(path, "utf8"))).toEqual({ version: 1, reducedMotion: true });
    expect(statSync(path).mode & 0o777).toBe(0o600);
    expect(readdirSync(directory)).toEqual(["workspace.json"]);
  });
});

test("invalid or future preference files remain byte-for-byte unchanged without orphan temporaries", () => {
  temporary((path, directory) => {
    const invalid = ["{ malformed", "null", "true", '"preferences"', "[]", '{"version":99,"reducedMotion":true,"retained":"future"}', '{"version":1.5,"reducedMotion":true}'];
    for (const content of invalid) {
      writeFileSync(path, content, { mode: 0o600 });
      expect(() => saveReducedMotion(false, path)).toThrow();
      expect(readFileSync(path, "utf8")).toBe(content);
      expect(readdirSync(directory)).toEqual(["workspace.json"]);
    }
  });
});

test("reduced motion reads reject unsupported schema versions and nonboolean values", () => {
  temporary(path => {
    for (const preference of [{ version: 99, reducedMotion: true }, { version: 1.5, reducedMotion: true }, { version: 1, reducedMotion: "true" }, { reducedMotion: 1 }, { reducedMotion: null }]) {
      writeFileSync(path, JSON.stringify(preference));
      expect(readReducedMotion(path)).toBe(false);
    }
    writeFileSync(path, JSON.stringify({ reducedMotion: true, metadata: "legacy object" }));
    expect(readReducedMotion(path)).toBe(true);
  });
});
