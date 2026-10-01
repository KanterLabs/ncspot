import { expect, test } from "bun:test";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { WorkspaceClient, RpcError } from "../src/workspace/client.js";
import { demoStatus } from "../src/status.js";

function serverFixture() {
  const path = join(tmpdir(), `resonance-rpc-${crypto.randomUUID()}.sock`);
  let identity = "engine-one";
  const buffers = new Map<any, string>();
  const server = Bun.listen({ unix: path, socket: {
    open(socket) { buffers.set(socket, ""); socket.write(JSON.stringify(demoStatus()) + "\n"); },
    data(socket, bytes) {
      let data = buffers.get(socket)! + new TextDecoder().decode(bytes);
      let newline;
      while ((newline = data.indexOf("\n")) >= 0) {
        const request = JSON.parse(data.slice(0, newline)); data = data.slice(newline + 1);
        if (request.method === "hang") continue;
        const response = { protocol: "resonance", version: 1, id: request.id, instance_id: identity,
          ...(request.method === "fail" ? { ok: false, error: { code: "stale_revision", message: "Queue changed" } } : { ok: true, result: request.method === "session.info" ? { instance_id: identity } : { method: request.method } }) };
        if (request.method === "slow") setTimeout(() => socket.write(JSON.stringify(response) + "\n"), 25);
        else socket.write(JSON.stringify(response) + "\n");
      }
      buffers.set(socket, data);
    }, close(socket) { buffers.delete(socket); },
  } });
  return { path, server, changeIdentity: () => identity = "engine-two" };
}
test("RPC client correlates out-of-order responses and structured errors alongside status", async () => {
  const fixture = serverFixture();
  let statuses = 0;
  const client = new WorkspaceClient(fixture.path, { onStatus: () => statuses++ });
  try {
    await client.connect();
    const slow = client.call<{ method: string }>("slow");
    expect(await client.call<{ method: string }>("fast")).toEqual({ method: "fast" });
    expect(await slow).toEqual({ method: "slow" }); expect(statuses).toBe(1);
    await expect(client.call("fail")).rejects.toMatchObject({ code: "stale_revision", message: "Queue changed" });
  } finally { client.close(); fixture.server.stop(true); }
});
test("RPC client rejects unknown outcomes, engine identity changes and pending shutdown", async () => {
  const fixture = serverFixture();
  const client = new WorkspaceClient(fixture.path, { onStatus: () => undefined }, 30);
  try {
    await client.connect();
    await expect(client.call("hang")).rejects.toBeInstanceOf(RpcError);
    fixture.changeIdentity();
    await expect(client.call("fast")).rejects.toMatchObject({ code: "instance_changed" });
    await expect(client.call("fast")).rejects.toMatchObject({ code: "disconnected" });
  } finally { client.close(); fixture.server.stop(true); }
});
