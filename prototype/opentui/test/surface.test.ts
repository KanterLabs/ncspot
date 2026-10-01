import { expect, test } from "bun:test";
import { BoxRenderable, ScrollBoxRenderable, type KeyEvent } from "@opentui/core";
import { createTestRenderer } from "@opentui/core/testing";
import { createSurface } from "../src/workspace/surface.js";
import type { Row, ScreenContext } from "../src/workspace/contracts.js";

async function fixture() {
  const setup = await createTestRenderer({ width: 80, height: 24, useMouse: true, kittyKeyboard: true });
  const ctx: ScreenContext = { renderer: setup.renderer, api: { async call<T>() { return {} as T; } }, theme: () => "light", setTheme() {}, reducedMotion: () => true, setReducedMotion() {}, status: () => null, onStatus: () => () => {}, navigate() {}, notify() {} };
  const surface = createSurface(ctx, "Library", "↑↓ select · Enter play");
  setup.renderer.root.add(surface.root);
  const listener = (key: KeyEvent) => { if (surface.handleKey(key)) key.preventDefault(); };
  setup.renderer.keyInput.on("keypress", listener);
  return { ...setup, surface, cleanup() { setup.renderer.keyInput.off("keypress", listener); surface.dispose(); setup.renderer.destroy(); } };
}
const row = (id: string): Row => ({ id, kind: "track", title: `Song ${id}`, subtitle: "Artist" });

test("surface renders compact Unicode rows and preserves identity on refresh", async () => {
  const f = await fixture();
  try {
    f.surface.setRows([row("a"), { ...row("b"), title: "雨の音 🎵 café" }, row("c")]);
    await f.renderOnce();
    f.mockInput.pressArrow("down");
    expect(f.surface.selected()?.id).toBe("b");
    const old = f.surface.body.getChildren()[0]!.getChildren()[1]!;
    f.surface.setRows([row("z"), row("c"), { ...row("b"), title: "雨の音 🎵 café" }]);
    expect(old.isDestroyed).toBe(true);
    expect(f.surface.selected()?.id).toBe("b");
    await f.renderOnce();
    expect(f.captureCharFrame()).toContain("雨の音");
    expect(f.surface.handleKey({ name: "j" })).toBe(false);
    f.mockInput.pressKey("HOME");
    expect(f.surface.selected()?.id).toBe("z");
    f.mockInput.pressKey("END");
    expect(f.surface.selected()?.id).toBe("b");
    f.surface.setLines(["Saved tracks"]);
    expect(f.surface.selected()?.id).toBe("b");
    f.surface.setRows([row("a"), row("b")]);
    await f.renderOnce();
    expect(f.captureCharFrame()).toContain("Saved tracks");
    expect(f.surface.selected()?.id).toBe("b");
    f.surface.setRows([]);
    expect(f.surface.selected()).toBeUndefined();
  } finally { f.cleanup(); }
});

test("surface scrolls with mouse and clicks select before activation", async () => {
  const f = await fixture();
  const activated: string[] = [];
  try {
    f.surface.setRows(Array.from({ length: 60 }, (_, i) => row(String(i))), undefined, value => activated.push(value.id));
    await f.renderOnce();
    const scroll = f.surface.body.getChildren()[0] as ScrollBoxRenderable;
    const target = scroll.getChildren()[2] as BoxRenderable;
    await f.mockMouse.click(target.screenX + 2, target.screenY);
    expect(f.surface.selected()?.id).toBe("2");
    expect(activated).toEqual([]);
    await f.mockMouse.click(target.screenX + 2, target.screenY);
    expect(activated).toEqual(["2"]);
    await f.mockMouse.scroll(scroll.screenX + 3, scroll.screenY + 4, "down");
    await f.renderOnce();
    expect(scroll.scrollTop).toBeGreaterThan(0);
    f.surface.handleKey({ name: "end" });
    await f.renderOnce();
    expect(f.surface.selected()?.id).toBe("59");
    expect(f.captureCharFrame()).toContain("Song 59");
  } finally { f.cleanup(); }
});

test("native focused prompt submits, cancels, and confirms safely", async () => {
  const f = await fixture();
  const submitted: string[] = [];
  let confirmed = 0;
  try {
    f.surface.prompt("Name", "", value => submitted.push(value));
    expect(f.surface.editing()).toBe(true);
    await f.renderOnce();
    await f.mockInput.typeText("café");
    f.mockInput.pressEnter();
    expect(submitted).toEqual(["café"]);
    expect(f.surface.editing()).toBe(false);
    f.surface.prompt("Name", "discard", value => submitted.push(value));
    f.mockInput.pressEscape();
    expect(submitted).toEqual(["café"]);
    expect(f.surface.editing()).toBe(false);
    f.surface.confirm("Remove track?", () => confirmed++);
    f.mockInput.pressEnter();
    expect(confirmed).toBe(0);
    f.surface.confirm("Remove track?", () => confirmed++);
    f.mockInput.pressKey("y");
    expect(confirmed).toBe(1);
    expect(f.surface.editing()).toBe(false);
    f.surface.setLines(["Settings", "Dark mode"]);
    f.surface.setTheme("dark");
    await f.renderOnce();
    expect(f.captureCharFrame()).toContain("Dark mode");
    f.surface.prompt("Pending name", "", value => submitted.push(value));
    const focused = f.renderer.currentFocusedRenderable;
    expect(focused).not.toBeNull();
    f.surface.dispose(); f.surface.dispose();
    expect(f.surface.root.isDestroyed).toBe(true);
    expect(focused?.isDestroyed).toBe(true);
    f.mockInput.pressEnter();
    expect(submitted).toEqual(["café"]);
  } finally { f.cleanup(); }
});
