import { expect, test } from "bun:test";
import { createTestRenderer } from "@opentui/core/testing";
import { mountWorkspace } from "../src/workspace/app.js";
import { DemoApi } from "../src/workspace/demo.js";
import { ROUTES } from "../src/workspace/contracts.js";

const settle = () => new Promise(resolve => setTimeout(resolve, 10));
test("complete workspace navigates every real screen in both themes at 80x24", async () => {
  const { renderer, renderOnce, captureCharFrame, mockInput } = await createTestRenderer({ width: 80, height: 24 });
  const api = new DemoApi();
  const app = mountWorkspace(renderer, { api, theme: "light", reducedMotion: true });
  try {
    app.setStatus(api.status);
    for (const theme of ["light", "dark"] as const) {
      app.context.setTheme(theme);
      for (const route of ROUTES) {
        app.navigate(route); await settle(); await renderOnce();
        const frame = captureCharFrame();
        expect(frame).toContain("RESONANCE");
        expect(frame).toContain("The Colour of Air");
        expect(frame.split("\n").length).toBeLessThanOrEqual(25);
        expect(app.route).toBe(route);
      }
    }
    // Prompts capture route digits, appearance letters and quit without navigating.
    app.navigate("search"); await settle(); await mockInput.typeText("1lq"); await renderOnce();
    expect(app.route).toBe("search");
    expect(captureCharFrame()).toContain("1lq");
    mockInput.pressKey("ESCAPE"); await new Promise(resolve => setTimeout(resolve, 60));
    await mockInput.typeText("2"); await settle();
    expect(app.route).toBe("queue");
  } finally { app.dispose(); renderer.destroy(); }
});

test("workspace global transport, notification dedup, motion restart and disposal", async () => {
  const { renderer, renderOnce, captureCharFrame, mockInput } = await createTestRenderer({ width: 120, height: 35 });
  const api = new DemoApi();
  const calls: string[] = [];
  let quit = 0;
  const app = mountWorkspace(renderer, { api: { async call(method, params) { calls.push(method); return api.call(method, params) as any; } }, theme: "light", onQuit: () => quit++ });
  try {
    app.setStatus({ ...api.status, notifications: [{ id: 1, message: "Radio refilled", created_at_ms: 1 }] });
    await renderOnce(); expect(captureCharFrame()).toContain("Radio refilled");
    app.notify("New notice"); app.setStatus({ ...api.status, notifications: [{ id: 1, message: "Radio refilled", created_at_ms: 1 }] });
    await renderOnce(); expect(captureCharFrame()).toContain("New notice");
    app.navigate("queue"); await settle(); await mockInput.typeText(" "); await settle();
    expect(calls).toContain("player.action");
    app.context.setReducedMotion(true); await settle(); expect(app.context.reducedMotion()).toBe(true);
    await mockInput.typeText("q"); expect(quit).toBe(1);
    app.setStatus(api.status); app.dispose();
    expect(() => app.setConnection(false, "Late socket close")).not.toThrow();
    expect(() => app.context.setReducedMotion(false)).not.toThrow();
  } finally { app.dispose(); renderer.destroy(); }
});

test("configured workspace bindings navigate and move locally while prompts retain normal text", async () => {
  const { renderer, mockInput, renderOnce, captureCharFrame } = await createTestRenderer({ width: 80, height: 24 });
  const api = new DemoApi();
  const app = mountWorkspace(renderer, { api, theme: "light", reducedMotion: true });
  try {
    app.setBindings({ z: "focus queue", j: "move down", "Ctrl+p": "focus playlists" });
    await mockInput.typeText("z"); await settle(); expect(app.route).toBe("queue");
    await mockInput.typeText("j"); await settle(); await renderOnce();
    expect(captureCharFrame()).toContain("The Colour of Air");
    expect(app.bindings().z).toBe("focus queue");
    mockInput.pressKey("p", { ctrl: true }); await settle(); expect(app.route).toBe("playlists");
    app.navigate("search"); await settle(); await mockInput.typeText("zj"); await renderOnce();
    expect(app.route).toBe("search"); expect(captureCharFrame()).toContain("zj");
    await app.command("focus library"); expect(app.route).toBe("library");
  } finally { app.dispose(); renderer.destroy(); }
});
