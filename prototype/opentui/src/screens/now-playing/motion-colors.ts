/**
 * Small, renderer-independent colour helpers for the now-playing motion layer.
 *
 * Artwork is returned as opaque RGB hex strings by the workspace contract. The
 * functions here deliberately keep that representation at the boundary so a
 * cover transition can be tested without importing OpenTUI (or allocating any
 * renderer objects).
 */

const RGB_HEX = /^#([\da-f]{6})$/i;
const ACCENT_BINS = 6;
const BIN_COUNT = ACCENT_BINS * ACCENT_BINS * ACCENT_BINS;
const MAX_ARTWORK_PIXELS = 1_600;

type Rgb = readonly [number, number, number];

function parseHex(value: string): Rgb | null {
  if (typeof value !== "string") return null;
  const match = RGB_HEX.exec(value);
  if (!match) return null;
  const hex = match[1]!;
  return [
    Number.parseInt(hex.slice(0, 2), 16),
    Number.parseInt(hex.slice(2, 4), 16),
    Number.parseInt(hex.slice(4, 6), 16),
  ];
}

function formatHex([red, green, blue]: Rgb): string {
  return `#${[red, green, blue].map(channel => channel.toString(16).padStart(2, "0")).join("")}`;
}

function clampAmount(amount: number): number {
  // NaN has no useful direction for a transition; treating it as the start
  // makes the helper deterministic while infinities still clamp naturally.
  if (Number.isNaN(amount)) return 0;
  return Math.min(1, Math.max(0, amount));
}

/** Blend two opaque RGB hex colours, clamping the transition amount to [0, 1]. */
export function blendHex(from: string, to: string, amount: number): string {
  const source: Rgb = parseHex(from) ?? [0, 0, 0];
  const target: Rgb = parseHex(to) ?? [0, 0, 0];
  const t = clampAmount(amount);
  if (t === 0) return formatHex(source);
  if (t === 1) return formatHex(target);
  return formatHex([
    Math.round(source[0] + (target[0] - source[0]) * t),
    Math.round(source[1] + (target[1] - source[1]) * t),
    Math.round(source[2] + (target[2] - source[2]) * t),
  ]);
}

function saturationAndValue([red, green, blue]: Rgb): readonly [number, number] {
  const channels = [red / 255, green / 255, blue / 255];
  const value = Math.max(...channels);
  const low = Math.min(...channels);
  return [value <= 0 ? 0 : (value - low) / value, value];
}

/** Lift a dark accent enough to read against the player card while preserving hue. */
function readable(rgb: Rgb): Rgb {
  const [, value] = saturationAndValue(rgb);
  const floor = 0.62;
  if (value <= 0 || value >= floor) return rgb;
  const scale = floor / value;
  return [
    Math.floor(Math.min(255, rgb[0] * scale)),
    Math.floor(Math.min(255, rgb[1] * scale)),
    Math.floor(Math.min(255, rgb[2] * scale)),
  ];
}

/**
 * Pick a restrained representative accent from an artwork pixel array.
 *
 * Neutral bins are ignored, so a mostly black or white cover does not wash out
 * the accent. Bins are scored by vividness and a sublinear area weight:
 * a meaningful band wins over background noise, while one saturated pixel does
 * not outweigh a representative patch of the image.
 */
export function artworkTint(pixels: readonly string[]): string | null {
  if (!pixels.length || pixels.length > MAX_ARTWORK_PIXELS) return null;

  const counts = new Uint32Array(BIN_COUNT);
  const sums = new Uint32Array(BIN_COUNT * 3);

  for (const pixel of pixels) {
    const rgb = parseHex(pixel);
    if (!rgb) return null;
    const redBin = Math.min(ACCENT_BINS - 1, Math.floor(rgb[0] * ACCENT_BINS / 256));
    const greenBin = Math.min(ACCENT_BINS - 1, Math.floor(rgb[1] * ACCENT_BINS / 256));
    const blueBin = Math.min(ACCENT_BINS - 1, Math.floor(rgb[2] * ACCENT_BINS / 256));
    const bin = redBin * ACCENT_BINS * ACCENT_BINS + greenBin * ACCENT_BINS + blueBin;
    counts[bin]++;
    const offset = bin * 3;
    sums[offset] += rgb[0];
    sums[offset + 1] += rgb[1];
    sums[offset + 2] += rgb[2];
  }

  let bestScore = -Infinity;
  let best: Rgb | null = null;
  for (let bin = 0; bin < BIN_COUNT; bin++) {
    const count = counts[bin]!;
    if (!count) continue;
    const offset = bin * 3;
    const mean: Rgb = [
      Math.floor(sums[offset]! / count),
      Math.floor(sums[offset + 1]! / count),
      Math.floor(sums[offset + 2]! / count),
    ];
    const [saturation, value] = saturationAndValue(mean);
    // Black, white, and washed-out shades make poor motion highlights whatever
    // share of the cover they occupy.
    if (saturation < 0.28 || value < 0.12) continue;

    // Area gets enough weight to keep a single vivid speck from replacing the
    // colour of a real patch, while the sublinear exponent still lets a vivid
    // patch beat a much larger neutral background.
    const score = count ** 0.75 * saturation * (0.4 + 0.6 * value);
    if (score > bestScore) {
      bestScore = score;
      best = mean;
    }
  }

  return best ? formatHex(readable(best)) : null;
}

/**
 * Blend an old cover into a new one pixel by pixel. If no old cover is present,
 * or its dimensions do not match, the supplied fallback is used as the source
 * colour for every target pixel so a resize or late artwork response is safe.
 */
export function blendPixels(
  from: readonly string[] | null,
  to: readonly string[],
  amount: number,
  fallback: string,
): string[] {
  const source = parseHex(fallback) ? fallback : "#000000";
  const matching = from !== null && from.length === to.length;
  return to.map((pixel, index) => blendHex(matching ? from[index]! : source, pixel, amount));
}
