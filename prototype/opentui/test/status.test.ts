import { expect, test } from "bun:test";
import { commandForKey } from "../src/commands.js";
import {
  formatTime,
  parseStatus,
  playableAlbum,
  positionAt,
} from "../src/status.js";

test("parses ncspot's serde status and prototype metadata", () => {
  const status = parseStatus(
    JSON.stringify({
      mode: { Paused: { secs: 25, nanos: 575_000_000 } },
      playable: {
        type: "Track",
        id: "spotify-track",
        title: "Hit Me Where It Hurts",
        artists: ["Caroline Polachek"],
        album: "Pang",
        duration: 184_132,
      },
      prototype: {
        position_ms: 25_575,
        discovery: 75,
        volume_percent: 63,
        radio_active: true,
        radio_waiting: false,
        up_next: [
          {
            id: "next-track",
            title: "New Track",
            artists: ["KanterLabs"],
            album: "Resonance Studies",
            duration: 120_000,
          },
        ],
      },
    }),
  );

  expect(status).not.toBeNull();
  expect(status?.mode).toEqual({ kind: "paused", positionMs: 25_575 });
  expect(status?.playable && "title" in status.playable ? status.playable.title : "").toBe(
    "Hit Me Where It Hurts",
  );
  expect(status?.prototype?.up_next[0]?.title).toBe("New Track");
  expect(positionAt(status!)).toBe(25_575);
  expect(playableAlbum(status?.playable ?? null)).toBe("Pang");
  expect(formatTime(184_132)).toBe("3:04");
});

test("playing status advances from SystemTime and malformed lines are ignored", () => {
  const status = parseStatus(
    JSON.stringify({
      mode: { Playing: { secs_since_epoch: 1_000, nanos_since_epoch: 0 } },
      playable: { type: "Track", title: "Signal", artists: [], album: "Single", duration: 30_000 },
    }),
  );
  expect(positionAt(status!, 1_004_250)).toBe(4_250);
  expect(parseStatus("not json")).toBeNull();
  expect(parseStatus(JSON.stringify({ playable: null }))).toBeNull();
});

test("keyboard transport maps to existing ncspot command strings", () => {
  expect(commandForKey({ name: "space" })).toBe("playpause");
  expect(commandForKey({ name: "left" })).toBe("previous");
  expect(commandForKey({ name: "right" })).toBe("next");
  expect(commandForKey({ name: "r" })).toBe("radio");
  expect(commandForKey({ name: "d" }, 75)).toBe("discovery 100");
  expect(commandForKey({ name: "f5" })).toBe("quit");
  expect(commandForKey({ name: "l" })).toBeNull();
  expect(commandForKey({ name: "c", ctrl: true })).toBeNull();
});
