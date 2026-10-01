import { expect, test } from "bun:test";
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
