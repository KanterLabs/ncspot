import { expect, test } from "bun:test";
import { createTestRenderer } from "@opentui/core/testing";
import { CastController, type CastView } from "../src/screens/cast/controller.js";
import { createCastScreen } from "../src/screens/cast/index.js";
import type { Page, Params, Row, RpcApi, ScreenContext } from "../src/workspace/contracts.js";

const device = (id: string, active = false): Row => ({ id, kind: "device", title: `Device ${id}`, subtitle: "Speaker", meta: { active, type: "Speaker" } });
const page = (items: Row[]): Page => ({ items, offset: 0, limit: 200, total: items.length, has_more: false });

function fixture(call: (method: string, params?: Params) => Promise<unknown>) {
  let rows: Row[] = [];
  let selectedId: string | undefined;
  const messages: string[] = [];
  const notices: string[] = [];
  const view: CastView = {
    selected: () => rows.find(row => row.id === selectedId),
    rows(next, id) { rows = next; selectedId = next.some(row => row.id === id) ? id : next[0]?.id; },
    message: message => { messages.push(message); },
    notify: message => { notices.push(message); },
  };
  const api: RpcApi = { call: (method, params) => call(method, params) as Promise<any> };
  return { controller: new CastController(api, view), view, api, messages, notices, rows: () => rows, select: (id: string) => { selectedId = id; } };
}

test("cast maps selected device identity, refreshes actual active state, and disconnects locally", async () => {
  const requests: [string, Params | undefined][] = [];
  let active = false;
  const f = fixture(async (method, params) => {
    requests.push([method, params]);
    if (method === "cast.action") { active = params?.action === "connect"; return { accepted: true }; }
    return page([device("one"), device("two", active)]);
  });
  await f.controller.refresh();
  f.select("two");
  await f.controller.connect();
  expect(requests).toContainEqual(["cast.action", { action: "connect", id: "two" }]);
  expect(f.view.selected()?.id).toBe("two");
  expect(f.rows()[1]?.detail).toBe("Connected");
  await f.controller.disconnect();
  expect(requests).toContainEqual(["cast.action", { action: "disconnect" }]);
  expect(f.rows()[1]?.detail).not.toBe("Connected");
  expect(f.messages.at(-1)).toContain("Playback is local");
});

test("cast errors preserve devices and selection with an actionable notice", async () => {
  const f = fixture(async method => {
    if (method === "cast.action") throw new Error("Device offline");
    return page([device("one"), device("two")]);
  });
  await f.controller.refresh();
  f.select("two");
  await f.controller.connect();
  expect(f.rows()).toHaveLength(2);
  expect(f.view.selected()?.id).toBe("two");
  expect(f.messages.at(-1)).toBe("Cast connect failed: Device offline");
  expect(f.notices.at(-1)).toContain("Device offline");
});

test("empty and unsupported devices never send connect requests", async () => {
  let actions = 0;
  let items: Row[] = [];
  const f = fixture(async method => { if (method === "cast.action") actions++; return page(items); });
  await f.controller.refresh();
  await f.controller.connect();
  expect(f.messages.at(-1)).toContain("No devices found");
  items = [{ ...device("roku"), kind: "roku", meta: { supported: false, reason: "Install Spotify on this Roku" } }];
  await f.controller.refresh();
  await f.controller.connect();
  expect(actions).toBe(0);
  expect(f.messages.at(-1)).toBe("Install Spotify on this Roku");
});

test("cast discovery failure retains device browser without claiming a new connection", async () => {
  let fail = false;
  const f = fixture(async () => { if (fail) throw new Error("Network unavailable"); return page([device("one")]); });
  await f.controller.refresh();
  fail = true;
  await f.controller.refresh();
  expect(f.view.selected()?.id).toBe("one");
  expect(f.messages.at(-1)).toContain("Device discovery failed: Network unavailable");
});

test("pending cast rejects duplicate connects and ignores completion after cleanup", async () => {
  let finish!: (value: unknown) => void;
  let actions = 0;
  const f = fixture(async method => {
    if (method === "cast.action") { actions++; return await new Promise(resolve => { finish = resolve; }); }
    return page([device("one")]);
  });
  await f.controller.refresh();
  const connecting = f.controller.connect();
  await f.controller.connect();
  expect(actions).toBe(1);
  f.controller.dispose();
  const messages = f.messages.length;
  finish({ accepted: true });
  await connecting;
  expect(f.messages).toHaveLength(messages);
  expect(f.notices).toHaveLength(0);
});

test("casting renders native compact device browser with theme and keyboard navigation", async () => {
  const setup = await createTestRenderer({ width: 80, height: 24, useMouse: true });
  const f = fixture(async () => page([device("one"), { ...device("two"), kind: "roku", subtitle: "Roku", meta: { type: "Roku" } }]));
  const ctx: ScreenContext = {
    renderer: setup.renderer, api: f.api, theme: () => "light", setTheme() {}, reducedMotion: () => false,
    setReducedMotion() {}, status: () => null, onStatus: () => () => {}, navigate() {}, notify() {},
  };
  const screen = createCastScreen(ctx);
  setup.renderer.root.add(screen.root);
  try {
    await screen.refresh();
    await setup.renderOnce();
    expect(setup.captureCharFrame()).toContain("Casting");
    expect(setup.captureCharFrame()).toContain("Device one");
    expect(screen.handleKey({ name: "down" })).toBe(true);
    screen.setTheme("dark");
    await setup.renderOnce();
    expect(setup.captureCharFrame()).toContain("› Device two");
    expect(screen.handleKey({ name: "r", ctrl: true })).toBe(false);
  } finally { screen.dispose(); setup.renderer.destroy(); }
});
