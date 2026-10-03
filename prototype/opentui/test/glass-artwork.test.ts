import { expect, test } from "bun:test";
import { NativeImage, type TerminalCapabilities } from "@opentui/core";
import { glassArtwork, glassPixels, letterboxNativeArtwork, supportsImageArtwork } from "../src/screens/now-playing/glass-artwork.js";
import { paletteForTheme } from "../src/theme.js";
import { testPng } from "./helpers/png.js";

test("glass filtering preserves fine image contrast and source edges for both themes", () => {
  const original = new Uint8Array(64 * 64 * 4);
  for (let y = 0; y < 64; y++) for (let x = 0; x < 64; x++) {
    const i = (y * 64 + x) * 4;
    original.set(x % 2 ? [240, 220, 200, 255] : [20, 30, 40, 255], i);
  }
  const before = original.slice();
  for (const theme of ["light", "dark"] as const) {
    const filtered = glassPixels(original, 64, 64, theme);
    expect(filtered.length).toBe(original.length);
    const center = (32 * 64 + 32) * 4;
    expect(filtered[center + 4]! - filtered[center]!).toBeGreaterThan(190);
    const background = paletteForTheme(theme).cover;
    const cover = [1, 3, 5].map(offset => parseInt(background.slice(offset, offset + 2), 16));
    for (const x of [0, 63]) for (const y of [0, 63]) {
      const corner = (y * 64 + x) * 4;
      expect([...filtered.slice(corner, corner + 3)]).not.toEqual(cover);
      expect(filtered[corner + 3]).toBe(255);
    }
  }
  expect(original).toEqual(before);
  expect(() => glassPixels(original, 641, 64, "dark")).toThrow("Invalid cover");
});

test("native letterboxing keeps a square source uncropped under rounded physical cell geometry", () => {
  const original = new Uint8Array(64 * 64 * 4);
  for (let y = 0; y < 64; y++) for (let x = 0; x < 64; x++) {
    const i = (y * 64 + x) * 4;
    original.set([x === 0 || x === 63 || y === 0 || y === 63 ? 240 : 32, 48, 64, 255], i);
  }
  const source = NativeImage.fromRgba(original, 64, 64);
  try {
    const fitted = letterboxNativeArtwork(source, { width: 96, height: 34, resolution: { width: 960, height: 707 } }, 20, 10, [10, 11, 20, 255]);
    expect(fitted).not.toBeNull();
    expect(fitted?.width).toBe(64);
    expect(fitted?.height).toBeGreaterThan(64);
    const raw = fitted!.raw();
    const top = Math.floor((fitted!.height - 64) / 2);
    expect([...raw.data.slice((top * fitted!.width) * 4, (top * fitted!.width) * 4 + 4)]).toEqual([240, 48, 64, 255]);
    for (let row = 0; row < 64; row++) {
      expect(raw.data.subarray((top + row) * raw.stride, (top + row) * raw.stride + 64 * 4)).toEqual(original.subarray(row * 64 * 4, (row + 1) * 64 * 4));
    }
    fitted?.dispose();
    expect(letterboxNativeArtwork(source, { width: 96, height: 34, resolution: { width: 960, height: 680 } }, 20, 10, [10, 11, 20, 255])).toBeNull();
  } finally { source.dispose(); }
});

test("encoded high-resolution artwork is decoded at full size and malformed or mismatched data is rejected", () => {
  const value = { mime: "image/png" as const, width: 64, height: 64, data: testPng(64, 64) };
  const image = glassArtwork(value, "dark");
  try { expect(image?.width).toBe(64); expect(image?.height).toBe(64); }
  finally { image?.dispose(); }
  expect(glassArtwork({ ...value, width: 63 }, "dark")).toBeNull();
  expect(glassArtwork({ ...value, data: "not a png" }, "dark")).toBeNull();
  expect(glassArtwork({ ...value, width: 10000 }, "dark")).toBeNull();
});

test("native protocol selection honors terminal capabilities, geometry, and multiplexer fallback", () => {
  const capabilities = { kitty_graphics: false, sixel: true, multiplexer: "none", image_protocol: "auto" } as TerminalCapabilities;
  expect(supportsImageArtwork({ capabilities: null, resolution: null })).toBe(false);
  expect(supportsImageArtwork({ capabilities, resolution: null })).toBe(false);
  expect(supportsImageArtwork({ capabilities, resolution: { width: 800, height: 600 } })).toBe(true);
  expect(supportsImageArtwork({ capabilities: { ...capabilities, kitty_graphics: true, sixel: false }, resolution: null })).toBe(true);
  expect(supportsImageArtwork({ capabilities: { ...capabilities, multiplexer: "tmux" }, resolution: { width: 800, height: 600 } })).toBe(false);
});
