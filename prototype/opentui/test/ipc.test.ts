import { expect, test } from "bun:test";
import { unlinkSync } from "node:fs";
import { IpcClient } from "../src/ipc.js";

const waitFor = async (predicate: () => boolean, timeoutMs = 1_500): Promise<void> => {
  const started = Date.now();
  while (!predicate()) {
    if (Date.now() - started > timeoutMs) throw new Error("timed out waiting for IPC test condition");
    await Bun.sleep(5);
  }
};

test("connects once, parses live status changes, emits commands, and cleans up", async () => {
  const socketPath = `/tmp/resonance-opentui-test-${process.pid}-${Date.now()}.sock`;
  try {
    unlinkSync(socketPath);
  } catch {
    // The path is normally absent; a stale test socket is safe to remove.
  }

  let serverSocket: Bun.Socket | undefined;
  let receivedCommands = "";
  const statuses: string[] = [];
  const listener = Bun.listen({
    unix: socketPath,
    socket: {
      open(socket) {
        serverSocket = socket;
        socket.write(
          `${JSON.stringify({
            mode: { Stopped: null },
            playable: null,
            prototype: {
              position_ms: 0,
              discovery: 50,
              volume_percent: 68,
              radio_active: false,
              radio_waiting: false,
              up_next: [],
            },
          })}\n`,
        );
      },
      data(_socket, data) {
        receivedCommands += data.toString();
      },
    },
  });

  let closed = false;
  const client = new IpcClient(socketPath, {
    onStatus: (status) => statuses.push(status.mode.kind),
    onClose: () => {
      closed = true;
    },
  });

  try {
    await client.connect();
    await waitFor(() => statuses.length === 1);
    expect(statuses).toEqual(["stopped"]);
    expect(client.send("next")).toBe(true);
    await waitFor(() => receivedCommands.includes("next\n"));
    expect(receivedCommands).toContain("next\n");

    serverSocket?.write(
      `${JSON.stringify({
        mode: { Paused: { secs: 12, nanos: 500_000_000 } },
        playable: {
          type: "Track",
          title: "Live Change",
          artists: ["KanterLabs"],
          album: "Resonance",
          duration: 90_000,
        },
      })}\n`,
    );
    await waitFor(() => statuses.length === 2);
    expect(statuses).toEqual(["stopped", "paused"]);
  } finally {
    client.close();
    listener.stop(true);
  }

  await waitFor(() => closed);
  expect(client.send("previous")).toBe(false);
});
