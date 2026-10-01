import { expect, test } from "bun:test";
import { MOTION, PlayerMotion, playableKey, playingBars, queueKeys } from "../src/screens/now-playing/motion.js";
import { demoStatus } from "../src/status.js";

test("track and queue transitions happen once and a moving window is not a refill", () => {
  const before = demoStatus();
  before.prototype!.radio_active = true;
  const current = before.prototype!.up_next[0]!;
  const next = { ...before, playable: current, prototype: { ...before.prototype!, up_next: before.prototype!.up_next.slice(1) } };
  const motion = new PlayerMotion();
  motion.observe(before, next, 1000, false);
  expect(motion.trackProgress(1000)).toBe(0);
  expect(motion.queueOffset(1000)).toBe(1);
  expect(motion.refills.size).toBe(0);
  motion.observe(next, { ...next }, 1100, false);
  expect(motion.trackAt).toBe(1000);
  expect(motion.advanceAt).toBe(1000);
  expect(motion.queueOffset(1000 + MOTION.queue)).toBe(0);
  expect(motion.active(2000)).toBe(false);
});

test("radio marks only added tracks, preserves markers through heartbeats, and expires them", () => {
  const before = demoStatus();
  before.prototype!.radio_active = true;
  const fresh = { ...before.prototype!.up_next[0]!, id: "fresh", uri: "spotify:track:fresh", title: "Fresh recommendation" };
  const next = { ...before, prototype: { ...before.prototype!, up_next: [...before.prototype!.up_next, fresh] } };
  const motion = new PlayerMotion();
  motion.observe(before, next, 1000, false);
  const keys = queueKeys(next.prototype.up_next);
  expect(motion.refills.size).toBe(1);
  expect(motion.refill(keys[0]!, 1300)).toBe(0);
  expect(motion.refill(keys.at(-1)!, 1300)).toBeGreaterThan(0.5);
  motion.observe(next, next, 1400, false);
  expect(motion.refills.get(keys.at(-1)!)).toBe(1000);
  expect(motion.active(2000)).toBe(false);
  expect(motion.refills.size).toBe(0);
  motion.observe(before, next, 2200, true);
  expect(motion.active(2200)).toBe(false);
});

test("button effects settle and queue duplicates receive distinct animation keys", () => {
  const motion = new PlayerMotion();
  motion.pulse("play", 1000);
  expect(motion.button("play", 1000)).toBe(1);
  expect(motion.button("save", 1000)).toBe(0);
  expect(motion.button("play", 1420)).toBe(0);
  motion.pulse("radio", 1500);
  motion.settle();
  expect(motion.active(1500)).toBe(false);
  const track = demoStatus().prototype!.up_next[0]!;
  const keys = queueKeys([track, track]);
  expect(keys[0]).not.toBe(keys[1]);
  expect(playableKey({ ...track, id: "same", uri: "" })).toBe(playableKey({ ...track, id: "same", uri: "spotify:track:same" }));
});

test("playing bars follow measured audio, settle in silence or pause, and freeze in reduced motion", () => {
  expect(playingBars(true, [0, 0.5, 1], 0.5, 1000, false)).toBe("▁▅█");
  expect(playingBars(true, [1, 1, 1], 0, 1000, false)).toBe("▁▁▁");
  expect(playingBars(false, [1, 1, 1], 1, 1000, false)).toBe("▁▁▁");
  expect(playingBars(true, undefined, undefined, 1000, true)).toBe(playingBars(true, undefined, undefined, 5000, true));
  expect(playingBars(true, undefined, undefined, 0, false)).not.toBe(playingBars(true, undefined, undefined, 160, false));
});
