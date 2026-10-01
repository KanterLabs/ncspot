import { expect, test } from "bun:test";
// main() is guarded by import.meta.main: importing the parser cannot launch a UI.
import { parseArgs } from "../src/main.js";
import { ROUTES } from "../src/workspace/contracts.js";

test("workspace parser accepts offline and attached launches with appearance and debugging options", () => {
  expect(parseArgs(["--demo"])).toEqual({ demo: true, smoke: false });
  expect(parseArgs(["--socket", "/tmp/resonance.sock", "--theme", "dark", "--route", "queue", "--reduced-motion", "--debug", "/tmp/resonance-debug.log"])).toEqual({ demo: false, smoke: false, socket: "/tmp/resonance.sock", theme: "dark", route: "queue", reducedMotion: true, debug: "/tmp/resonance-debug.log" });
  expect(parseArgs(["-s", "/tmp/socket with spaces", "--theme", "light"])).toEqual({ demo: false, smoke: false, socket: "/tmp/socket with spaces", theme: "light" });
  expect(parseArgs(["--smoke"])).toEqual({ demo: false, smoke: true });
  for (const route of ROUTES) expect(parseArgs(["--demo", "--route", route])).toMatchObject({ route });
});

test("help flags work without an engine and parser rejects conflicting launch modes", () => {
  expect(parseArgs(["--help"])).toBe("help");
  expect(parseArgs(["-h"])).toBe("help");
  expect(parseArgs([])).toBe("Provide --socket PATH or use --demo");
  expect(parseArgs(["--demo", "--socket", "/tmp/engine.sock"])).toBe("Choose either --demo or --socket PATH");
  expect(parseArgs(["-s", "/tmp/engine.sock", "--demo"])).toBe("Choose either --demo or --socket PATH");
});

test("workspace parser rejects invalid themes, routes, unknown flags, and missing option values", () => {
  expect(parseArgs(["--demo", "--theme", "sepia"])).toBe("--theme must be light or dark");
  expect(parseArgs(["--demo", "--route", "unknown"])).toBe(`--route must be one of ${ROUTES.join(", ")}`);
  expect(parseArgs(["--demo", "--reduced-motion=false"])).toBe("Unknown argument: --reduced-motion=false");
  expect(parseArgs(["--demo", "extra"])).toBe("Unknown argument: extra");
  for (const flag of ["--socket", "-s", "--theme", "--route", "--debug"]) {
    expect(parseArgs([flag])).toBe(`${flag} requires a value`);
    expect(parseArgs([flag, "--demo"])).toBe(`${flag} requires a value`);
    expect(parseArgs([flag, ""])).toBe(`${flag} requires a value`);
  }
});

test("short flags cannot be mistaken for socket or debug paths", () => {
  expect(parseArgs(["-s", "-h"])).toBe("-s requires a value");
  expect(parseArgs(["--demo", "--debug", "-h"])).toBe("--debug requires a value");
});
