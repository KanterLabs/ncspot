import { expect, test } from "bun:test";
import { artworkTint, blendHex, blendPixels } from "../src/screens/now-playing/motion-colors.js";

test("blendHex clamps amounts and preserves exact endpoints", () => {
  expect(blendHex("#102030", "#a0b0c0", 0)).toBe("#102030");
  expect(blendHex("#102030", "#a0b0c0", 1)).toBe("#a0b0c0");
  expect(blendHex("#102030", "#a0b0c0", -1)).toBe("#102030");
  expect(blendHex("#102030", "#a0b0c0", 2)).toBe("#a0b0c0");
  expect(blendHex("#000000", "#ffffff", 0.5)).toBe("#808080");
});

test("artworkTint favors a representative vivid patch over neutral cover space", () => {
  const pixels = [
    ...Array.from({ length: 12 }, () => "#242628"),
    ...Array.from({ length: 4 }, () => "#e8e8e8"),
    ...Array.from({ length: 6 }, () => "#e67814"),
  ];
  const tint = artworkTint(pixels);
  expect(tint).not.toBeNull();
  const red = Number.parseInt(tint!.slice(1, 3), 16);
  const green = Number.parseInt(tint!.slice(3, 5), 16);
  const blue = Number.parseInt(tint!.slice(5, 7), 16);
  expect(red).toBeGreaterThan(green);
  expect(green).toBeGreaterThan(blue);
});

test("artworkTint returns null for monochrome, empty, or malformed pixels", () => {
  expect(artworkTint([])).toBeNull();
  expect(artworkTint(["#343434", "#787878", "#eeeeee"])).toBeNull();
  expect(artworkTint(["#112233", "not-a-pixel"])).toBeNull();
});

test("a single saturated outlier does not beat a much larger muted patch", () => {
  const pixels = [
    ...Array.from({ length: 48 }, () => "#587487"),
    "#ff00ff",
  ];
  const tint = artworkTint(pixels);
  expect(tint).not.toBe("#ff00ff");
  expect(tint).not.toBeNull();
});

test("a bright primary dominant color remains eligible for tinting", () => {
  const tint = artworkTint([
    ...Array.from({ length: 24 }, () => "#ff0000"),
    ...Array.from({ length: 8 }, () => "#202020"),
    ...Array.from({ length: 4 }, () => "#eeeeee"),
  ]);
  expect(tint).toBe("#ff0000");
});

test("blendPixels blends matching covers and uses fallback for missing or mismatched old covers", () => {
  const target = ["#ffffff", "#000000"];
  expect(blendPixels(["#000000", "#ffffff"], target, 0.5, "#224466")).toEqual(["#808080", "#808080"]);
  expect(blendPixels(null, target, 0, "#224466")).toEqual(["#224466", "#224466"]);
  expect(blendPixels(["#000000"], target, 1, "#224466")).toEqual(target);
  expect(blendPixels([], [], 0.5, "#224466")).toEqual([]);
});
