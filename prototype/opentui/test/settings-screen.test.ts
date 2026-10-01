import { expect, test } from "bun:test";
import { createTestRenderer } from "@opentui/core/testing";
import type { Params, RpcApi, ScreenContext } from "../src/workspace/contracts.js";
import type { ThemeName } from "../src/theme.js";
import { createSettingsScreen } from "../src/screens/settings/index.js";
import { createHelpScreen } from "../src/screens/help/index.js";
import { actionNotice, settingRows, settingsAction } from "../src/screens/settings/model.js";
import { helpRows } from "../src/screens/help/model.js";

function context(renderer: ScreenContext["renderer"], call: (method: string, params?: Params) => Promise<unknown>) {
  let theme: ThemeName = "light";
  let motion = false;
  const ctx: ScreenContext = {
    renderer, api: { call } as RpcApi, theme: () => theme, setTheme: next => { theme = next; },
    reducedMotion: () => motion, setReducedMotion: next => { motion = next; },
    status: () => null, onStatus: () => () => {}, navigate: () => {}, notify: () => {},
  };
  return ctx;
}

test("safe settings rows omit credential keys and opaque objects", () => {
  const rows = settingRows({ volume: 75, autoplay: true, username: "private", access_token: "private", authentication: { password: "private" } });
  expect(rows.map(row => row.title)).toEqual(["autoplay", "volume"]);
  expect(JSON.stringify(rows)).not.toContain("private");
});

test("commands map to one settings action and reject blank or multiline input", async () => {
  const calls: unknown[] = [];
  const api = { call: async (method: string, params: Params) => { calls.push([method, params]); } } as RpcApi;
  await settingsAction(api, "command", "  volup  ");
  expect(calls).toEqual([["settings.action", { action: "command", command: "volup" }]]);
  await expect(settingsAction(api, "command", "")).rejects.toThrow("Enter an ncspot command");
  await expect(settingsAction(api, "command", "play\nlogout")).rejects.toThrow("single command");
  expect(calls).toHaveLength(1);
});

test("help includes every workspace route and effective backend bindings", () => {
  const rows = helpRows({ "Ctrl+p": "playpause" });
  expect(rows.some(row => row.title === "9  Cast")).toBe(true);
  expect(rows.some(row => row.title === "b  Browse")).toBe(true);
  expect(rows.some(row => row.title === "Ctrl+p" && row.subtitle === "playpause")).toBe(true);
});

test("action acknowledgments distinguish accepted requests from completed operations", () => {
  expect(actionNotice("Command", { accepted: true, completed: false })).toBe("Command accepted");
  expect(actionNotice("reconnect", { accepted: true })).toBe("reconnect accepted");
  expect(actionNotice("reload", { completed: false })).toBe("reload started");
  expect(actionNotice("reload", { accepted: true, completed: true })).toBe("reload completed");
  expect(actionNotice("Command", { applied: true })).toBe("Command completed");
});

test("settings and command palette show accepted notices for asynchronous acknowledgments", async () => {
  const setup = await createTestRenderer({ width: 80, height: 24 });
  const notices: string[] = [];
  const ctx = context(setup.renderer, async method => method === "settings.get"
    ? { values: {}, bindings: {} } : { accepted: true, completed: false });
  ctx.notify = message => { notices.push(message); };
  const settings = createSettingsScreen(ctx);
  setup.renderer.root.add(settings.root);
  try {
    await settings.refresh();
    settings.handleKey({ name: "down" });
    settings.handleKey({ name: "down" });
    settings.handleKey({ name: "down" });
    settings.handleKey({ name: "return" });
    await new Promise(resolve => setTimeout(resolve, 0));
    await setup.renderOnce();
    expect(setup.captureCharFrame()).toContain("reconnect accepted");
    expect(notices).toEqual(["reconnect accepted"]);
  } finally { settings.dispose(); }
  const help = createHelpScreen(ctx, { command: true });
  setup.renderer.root.add(help.root);
  const handler = (key: Parameters<typeof help.handleKey>[0]) => { help.handleKey(key); };
  setup.renderer.keyInput.on("keypress", handler);
  try {
    await setup.mockInput.typeText("reconnect");
    setup.mockInput.pressKey("RETURN");
    await new Promise(resolve => setTimeout(resolve, 0));
    await setup.renderOnce();
    expect(setup.captureCharFrame()).toContain("Command accepted");
    expect(notices).toEqual(["reconnect accepted", "Command accepted"]);
    expect(setup.captureCharFrame()).not.toContain("Command completed");
  } finally { setup.renderer.keyInput.off("keypress", handler); help.dispose(); setup.renderer.destroy(); }
});

test("settings appearance callbacks, confirmation and failures at 80x24", async () => {
  const setup = await createTestRenderer({ width: 80, height: 24 });
  const calls: [string, Params | undefined][] = [];
  const ctx = context(setup.renderer, async (method, params) => {
    calls.push([method, params]);
    if (method === "settings.get") return { values: { volume: 50, password: "secret" }, bindings: {} };
    throw new Error("Session unavailable");
  });
  const screen = createSettingsScreen(ctx);
  setup.renderer.root.add(screen.root);
  try {
    await screen.refresh();
    screen.handleKey({ name: "return" });
    expect(ctx.theme()).toBe("dark");
    screen.setTheme("dark");
    screen.handleKey({ name: "down" });
    screen.handleKey({ name: "return" });
    expect(ctx.reducedMotion()).toBe(true);
    screen.handleKey({ name: "down" });
    screen.handleKey({ name: "return" });
    await new Promise(resolve => setTimeout(resolve, 0));
    await setup.renderOnce();
    const frame = setup.captureCharFrame();
    expect(frame).toContain("Session unavailable");
    expect(frame).not.toContain("secret");
    screen.handleKey({ name: "down" });
    screen.handleKey({ name: "down" });
    screen.handleKey({ name: "return" });
    expect(screen.editing?.()).toBe(true);
    screen.handleKey({ name: "n" });
    expect(calls.filter(([, params]) => params?.action === "logout")).toHaveLength(0);
    screen.handleKey({ name: "return" });
    screen.handleKey({ name: "y" });
    await new Promise(resolve => setTimeout(resolve, 0));
    expect(calls.some(([, params]) => params?.action === "logout")).toBe(true);
  } finally { screen.dispose(); setup.renderer.destroy(); }
});

test("command palette starts focused and reports backend errors", async () => {
  const setup = await createTestRenderer({ width: 80, height: 24 });
  const calls: [string, Params | undefined][] = [];
  const ctx = context(setup.renderer, async (method, params) => {
    calls.push([method, params]);
    if (method === "settings.get") return { values: {}, bindings: { p: "playpause" } };
    throw new Error("Unsupported UI command");
  });
  const screen = createHelpScreen(ctx, { command: true });
  setup.renderer.root.add(screen.root);
  setup.renderer.keyInput.on("keypress", key => { screen.handleKey(key); });
  try {
    expect(screen.editing?.()).toBe(true);
    await screen.refresh();
    await setup.mockInput.typeText("help");
    setup.mockInput.pressKey("RETURN");
    await new Promise(resolve => setTimeout(resolve, 0));
    await setup.renderOnce();
    expect(calls).toContainEqual(["settings.action", { action: "command", command: "help" }]);
    expect(setup.captureCharFrame()).toContain("Unsupported UI command");
  } finally { screen.dispose(); setup.renderer.destroy(); }
});

test("settings actions activate by mouse in compact layout", async () => {
  const setup = await createTestRenderer({ width: 80, height: 24, useMouse: true });
  const ctx = context(setup.renderer, async () => ({ values: {}, bindings: {} }));
  const screen = createSettingsScreen(ctx);
  setup.renderer.root.add(screen.root);
  try {
    await screen.refresh();
    await setup.renderOnce();
    await setup.mockMouse.click(8, 3);
    await setup.mockMouse.click(8, 3);
    expect(ctx.theme()).toBe("dark");
    screen.setTheme("dark");
    await setup.renderOnce();
    expect(setup.captureCharFrame()).toContain("dark");
  } finally { screen.dispose(); setup.renderer.destroy(); }
});
