import { expect, test } from "bun:test";
import { BoxRenderable } from "@opentui/core";
import { createTestRenderer } from "@opentui/core/testing";
import { mountResonance } from "../src/app.js";
import { DemoTransport } from "../src/ipc.js";
import { demoStatus } from "../src/status.js";

test("OpenTUI renderer draws the surface and reacts to live status and keyboard input", async () => {
  const setup = await createTestRenderer({ width: 118, height: 42, useMouse: true });
  const transport = new DemoTransport();
  const app = mountResonance(setup.renderer, {
    transport,
    connectionMessage: "offline preview",
  });
  app.setStatus(demoStatus(0, 82_000));

  try {
    await setup.renderOnce();
    let frame = setup.captureCharFrame();
    expect(frame).toContain("KANTERLABS");
    expect(frame).toContain("UP NEXT");
    expect(frame).toContain("The Colour of Air");

    await setup.mockMouse.click(15, 36);
    expect(transport.commands).toContain("discovery 75");

    setup.mockInput.pressKey(" ");
    expect(transport.commands).toContain("playpause");

    app.setStatus(demoStatus(1, 14_000));
    await setup.renderOnce();
    frame = setup.captureCharFrame();
    expect(frame).toContain("Soft Geometry");
    expect(app.state.positionMs).toBe(14_000);
  } finally {
    app.dispose();
    setup.renderer.destroy();
  }
});

test("OpenTUI controller cleanup closes its transport exactly once", async () => {
  const setup = await createTestRenderer({ width: 90, height: 32 });
  const transport = new DemoTransport();
  let closed = 0;
  const wrappedTransport = {
    send: (command: string) => transport.send(command),
    close: () => {
      closed += 1;
      transport.close();
    },
  };
  const app = mountResonance(setup.renderer, { transport: wrappedTransport });
  app.quit();
  app.quit();
  expect(closed).toBe(1);
  expect(setup.renderer.isDestroyed).toBe(true);
});

test("compact 80x24 layout keeps the complete transport surface visible", async () => {
  const setup = await createTestRenderer({ width: 80, height: 24 });
  const app = mountResonance(setup.renderer, { transport: new DemoTransport() });
  app.setStatus(demoStatus(0, 82_000));
  try {
    await setup.renderOnce();
    const frame = setup.captureCharFrame();
    expect(frame).toContain("The Colour of Air");
    expect(frame).toContain("PAUSE");
    expect(frame).toContain("DISCOVERY");
    expect(frame).toContain("UP NEXT");
  } finally {
    app.dispose();
    setup.renderer.destroy();
  }
});

test("light/dark theme toggles recolor the renderer without changing playback or IPC", async () => {
  const setup = await createTestRenderer({ width: 118, height: 42, useMouse: true });
  const transport = new DemoTransport();
  const themes: string[] = [];
  const app = mountResonance(setup.renderer, {
    transport,
    theme: "light",
    onThemeChange: (theme) => themes.push(theme),
  });
  app.setStatus(demoStatus(0, 82_000));

  try {
    await setup.renderOnce();
    const shell = setup.renderer.root.findDescendantById("resonance-shell") as BoxRenderable;
    const cover = setup.renderer.root.findDescendantById("cover-art") as BoxRenderable;
    const toggle = setup.renderer.root.findDescendantById("theme-toggle");
    expect(app.theme).toBe("light");
    expect(shell.backgroundColor.toInts()).toEqual([238, 243, 249, 255]);
    expect(cover.backgroundColor.toInts()).toEqual([220, 234, 250, 255]);
    expect(setup.captureCharFrame()).toContain("☼ LIGHT");

    setup.mockInput.pressKey("l");
    expect(app.theme).toBe("dark");
    expect(app.state.positionMs).toBe(82_000);
    expect(transport.commands).toEqual([]);
    expect(themes).toEqual(["dark"]);
    expect(shell.backgroundColor.toInts()).toEqual([8, 10, 16, 255]);
    expect(cover.backgroundColor.toInts()).toEqual([37, 32, 68, 255]);
    await setup.renderOnce();
    expect(setup.captureCharFrame()).toContain("☾ DARK");

    // The header control uses the same local setter and remains an UI-only
    // action, so it must not create a transport command either.
    expect(toggle).toBeDefined();
    if (toggle) await setup.mockMouse.click(toggle.screenX + 1, toggle.screenY);
    expect(app.theme).toBe("light");
    expect(app.state.positionMs).toBe(82_000);
    expect(transport.commands).toEqual([]);
    expect(themes).toEqual(["dark", "light"]);
  } finally {
    app.dispose();
    setup.renderer.destroy();
  }
});
