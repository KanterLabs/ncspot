import { expect, test } from "bun:test";
import { RadioController, type RadioView } from "../src/screens/radio/controller.js";
import type { ParsedStatus } from "../src/status.js";
import type { Params, RpcApi } from "../src/workspace/contracts.js";

interface Deferred<T> {
  promise: Promise<T>;
  resolve(value: T): void;
  reject(error: unknown): void;
}

function deferred<T>(): Deferred<T> {
  let resolve!: (value: T) => void;
  let reject!: (error: unknown) => void;
  const promise = new Promise<T>((done, fail) => { resolve = done; reject = fail; });
  return { promise, resolve, reject };
}

interface Call {
  method: string;
  params?: Params;
  deferred: Deferred<unknown>;
}

interface Fixture {
  controller: RadioController;
  calls: Call[];
  messages: string[];
  changes: number;
  emit(status: ParsedStatus): void;
}

function fixture(discovery = 50): Fixture {
  const calls: Call[] = [];
  const messages: string[] = [];
  let changes = 0;
  let listener: ((status: ParsedStatus) => void) | undefined;
  const api: RpcApi = {
    call<T>(method: string, params?: Params): Promise<T> {
      const call: Call = { method, params, deferred: deferred<unknown>() };
      calls.push(call);
      return call.deferred.promise as Promise<T>;
    },
  };
  const view: RadioView = {
    lines() {},
    rows() {},
    message(value) { messages.push(value); },
    changed() { changes++; },
  };
  const controller = new RadioController(api, view, callback => {
    listener = callback;
    return () => { listener = undefined; };
  });
  controller.status = { active: false, waiting: false, discovery, played_count: 0, cache_tracks: 0 };
  return {
    controller,
    calls,
    messages,
    get changes() { return changes; },
    emit(status) { listener?.(status); },
  };
}

async function flush(): Promise<void> {
  // A coalesced intent can pass through the settling confirmation read and
  // start its replacement write in several promise continuations.
  for (let i = 0; i < 8; i++) await Promise.resolve();
}

function callAt(fixture: Fixture, index: number): Call {
  const call = fixture.calls[index];
  if (!call) throw new Error(`missing call ${index}`);
  return call;
}

function status(discovery: number): ParsedStatus {
  return {
    mode: { kind: "stopped" },
    playable: null,
    prototype: {
      position_ms: 0,
      discovery,
      volume_percent: 50,
      radio_active: false,
      radio_waiting: false,
      up_next: [],
    },
  };
}

test("rapid discovery adjustments coalesce against the latest desired value", async () => {
  const f = fixture(50);
  const first = f.controller.adjust(5);
  const second = f.controller.adjust(5);
  const third = f.controller.adjust(5);
  expect(f.calls[0]?.method).toBe("radio.action");
  expect(f.calls[0]?.params).toEqual({ action: "discovery", value: 55 });
  expect(f.controller.status.discovery).toBe(50);
  expect(f.controller.desiredDiscovery).toBe(65);
  expect(f.controller.busy).toBe(true);

  callAt(f, 0).deferred.resolve({ applied: true });
  await flush();
  expect(f.calls[1]?.params).toEqual({ action: "discovery", value: 65 });
  expect(f.calls).toHaveLength(2);

  callAt(f, 1).deferred.resolve({ applied: true });
  await flush();
  expect(f.calls[2]?.method).toBe("radio.status");
  callAt(f, 2).deferred.resolve({ active: false, waiting: false, discovery: 65 });
  await Promise.all([first, second, third]);
  expect(f.controller.status.discovery).toBe(65);
  expect(f.controller.desiredDiscovery).toBeUndefined();
  expect(f.controller.busy).toBe(false);
});

test("setDiscovery clamps presets and action discovery uses the same serialized lane", async () => {
  const f = fixture(50);
  const first = f.controller.action("discovery", 500);
  const second = f.controller.action("discovery", -20);
  expect(f.calls[0]?.params).toEqual({ action: "discovery", value: 100 });
  expect(f.controller.desiredDiscovery).toBe(0);
  callAt(f, 0).deferred.resolve({ applied: true });
  await flush();
  expect(f.calls[1]?.params).toEqual({ action: "discovery", value: 0 });
  callAt(f, 1).deferred.resolve({ applied: true });
  await flush();
  callAt(f, 2).deferred.resolve({ discovery: 0 });
  await Promise.all([first, second]);
  expect(f.controller.status.discovery).toBe(0);
  expect(f.controller.desiredDiscovery).toBeUndefined();
});

test("failed discovery clears pending intent and keeps the last confirmed value", async () => {
  const f = fixture(42);
  const pending = f.controller.setDiscovery(88);
  callAt(f, 0).deferred.reject(new Error("engine offline"));
  await pending;
  expect(f.controller.status.discovery).toBe(42);
  expect(f.controller.desiredDiscovery).toBeUndefined();
  expect(f.controller.busy).toBe(false);
  expect(f.messages.at(-1)).toBe("Radio action failed: engine offline");
});

test("failed confirming status keeps the old value and permits a later heartbeat retry", async () => {
  const f = fixture(42);
  const pending = f.controller.setDiscovery(88);
  callAt(f, 0).deferred.resolve({ applied: true });
  await flush();
  expect(f.calls[1]?.method).toBe("radio.status");
  callAt(f, 1).deferred.reject(new Error("status temporarily unavailable"));
  await pending;
  expect(f.controller.status.discovery).toBe(42);
  expect(f.controller.desiredDiscovery).toBeUndefined();
  expect(f.controller.busy).toBe(false);
  expect(f.messages.at(-1)).toBe("Discovery status confirmation failed: status temporarily unavailable");

  // The failed confirmation resets the heartbeat signature, so a later status
  // event can retry without requiring another slider interaction.
  f.emit(status(42));
  expect(f.calls[2]?.method).toBe("radio.status");
  callAt(f, 2).deferred.resolve({ active: false, waiting: false, discovery: 42 });
  await flush();
});

test("a newer discovery intent supersedes a confirmation that is still in flight", async () => {
  const f = fixture(50);
  const first = f.controller.setDiscovery(80);
  callAt(f, 0).deferred.resolve({ applied: true });
  await flush();
  expect(f.calls[1]?.method).toBe("radio.status");

  const second = f.controller.setDiscovery(90);
  expect(f.controller.desiredDiscovery).toBe(90);
  expect(f.calls).toHaveLength(2);
  callAt(f, 1).deferred.resolve({ active: false, waiting: false, discovery: 80 });
  await flush();
  expect(f.calls[2]?.params).toEqual({ action: "discovery", value: 90 });
  callAt(f, 2).deferred.resolve({ applied: true });
  await flush();
  expect(f.calls[3]?.method).toBe("radio.status");
  callAt(f, 3).deferred.resolve({ active: false, waiting: false, discovery: 90 });
  await Promise.all([first, second]);
  expect(f.controller.status.discovery).toBe(90);
  expect(f.controller.desiredDiscovery).toBeUndefined();
  expect(f.messages.filter(message => message.startsWith("Discovery status confirmation failed"))).toEqual([]);
});

test("stale status reads cannot overwrite a discovery write and heartbeats stay single-flight", async () => {
  const f = fixture(50);
  const staleRead = f.controller.refresh();
  expect(f.calls).toHaveLength(1);
  const write = f.controller.setDiscovery(80);
  expect(f.calls).toHaveLength(2);
  // Heartbeats with changing signatures are ignored while the write is pending.
  f.emit(status(51)); f.emit(status(52)); f.emit(status(53));
  expect(f.calls).toHaveLength(2);

  callAt(f, 1).deferred.resolve({ applied: true });
  await flush();
  expect(f.controller.desiredDiscovery).toBe(80);
  callAt(f, 0).deferred.resolve({ active: false, waiting: false, discovery: 50 });
  await staleRead;
  await flush();
  expect(f.controller.status.discovery).toBe(50);
  expect(f.calls[2]?.method).toBe("radio.status");
  callAt(f, 2).deferred.resolve({ active: false, waiting: false, discovery: 80 });
  await write;
  expect(f.controller.status.discovery).toBe(80);
  expect(f.controller.desiredDiscovery).toBeUndefined();
});

test("start and stop remain single-flight and disposal prevents late callbacks", async () => {
  const f = fixture(50);
  const start = f.controller.action("start", undefined, "spotify:track:seed");
  const duplicateStart = f.controller.action("start", undefined, "spotify:track:seed");
  expect(f.calls).toHaveLength(1);
  callAt(f, 0).deferred.resolve({ applied: true });
  await flush();
  expect(f.calls[1]?.method).toBe("radio.status");
  callAt(f, 1).deferred.resolve({ active: true, waiting: false, discovery: 50 });
  await Promise.all([start, duplicateStart]);
  const stop = f.controller.action("stop");
  expect(f.calls[2]?.params).toEqual({ action: "stop" });
  f.controller.dispose();
  const changes = f.changes;
  callAt(f, 2).deferred.resolve({ applied: true });
  await stop;
  f.emit(status(50));
  expect(f.changes).toBe(changes);
  expect(f.calls).toHaveLength(3);
});
