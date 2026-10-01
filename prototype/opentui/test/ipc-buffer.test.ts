import { expect, test } from "bun:test";
import { IpcClient } from "../src/ipc.js";

test("IPC preserves UTF-8 and newline boundaries under partial writes and backpressure", () => {
  const client = new IpcClient("unused", { onStatus() {} });
  const internal = client as any;
  const written: number[] = [];
  let blocked = false;
  internal.opened = true;
  internal.socket = { readyState: 1, write(data: Uint8Array, offset: number, length: number) {
    if (blocked) return 0;
    const count = Math.min(length, 3); written.push(...data.slice(offset, offset + count)); blocked = true; return count;
  }, close() {} };
  expect(client.send("search Björk 🫧")).toBe(true);
  expect(client.send("playpause")).toBe(true);
  while (internal.queuedBytes) { blocked = false; internal.flushWrites(); }
  expect(new TextDecoder().decode(Uint8Array.from(written))).toBe("search Björk 🫧\nplaypause\n");
  client.close(); expect(internal.queuedBytes).toBe(0);
});

test("IPC rejects oversized outgoing requests and closes on oversized incoming frames", () => {
  let error: Error | undefined;
  const client = new IpcClient("unused", { onStatus() {}, onClose(value) { error = value; } });
  const internal = client as any;
  internal.opened = true; internal.socket = { readyState: 1, write() { throw new Error("Must not send oversized request"); }, close() {} };
  expect(client.send("x".repeat(1024 * 1024))).toBe(false);
  internal.receiveBuffer = "x".repeat(8 * 1024 * 1024 + 1) + "\n";
  internal.drainLines(); expect(client.isOpen).toBe(false); expect(error?.message).toContain("limit");
});
