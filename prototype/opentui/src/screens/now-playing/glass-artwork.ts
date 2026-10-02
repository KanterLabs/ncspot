import { NativeImage, resolveImageRenderProtocol, type CliRenderer } from "@opentui/core";
import { paletteForTheme, type ThemeName } from "../../theme.js";

export interface HighResolutionArtwork {
  mime: "image/png";
  width: number;
  height: number;
  data: string;
}

export function supportsImageArtwork(renderer: Pick<CliRenderer, "capabilities" | "resolution">): boolean {
  const resolution = renderer.resolution;
  return resolveImageRenderProtocol("auto", renderer.capabilities, !!resolution && resolution.width > 0 && resolution.height > 0) !== "blocks";
}

/** Apply the glass finish once at image resolution, retaining the cover's fine detail. */
export function glassPixels(pixels: Uint8Array, width: number, height: number, theme: ThemeName, stride = width * 4): Uint8Array {
  if (!Number.isInteger(width) || !Number.isInteger(height) || width < 1 || height < 1 || width > 640 || height > 640 || stride < width * 4 || pixels.length < stride * height) throw new Error("Invalid cover dimensions");
  const colors = paletteForTheme(theme);
  const rgb = (hex: string) => [1, 3, 5].map(offset => parseInt(hex.slice(offset, offset + 2), 16));
  const background = rgb(colors.cover);
  const tint = theme === "light" ? [225, 230, 255] : [183, 166, 231];
  const result = new Uint8Array(width * height * 4);
  const radius = Math.min(width, height) * 0.055;
  for (let y = 0; y < height; y++) for (let x = 0; x < width; x++) {
    const input = y * stride + x * 4, output = (y * width + x) * 4;
    const nx = (x + 0.5) / width, ny = (y + 0.5) / height;
    // A restrained diagonal reflection and edge shade; no blur of the artwork.
    const reflection = Math.max(0, 1 - Math.abs(nx + ny * 0.75 - 0.32) / 0.26) * (theme === "light" ? 0.09 : 0.055);
    const edge = Math.max(0, Math.hypot(nx - 0.5, ny - 0.5) - 0.38) * 0.13;
    const cx = Math.max(radius - x - 0.5, x + 0.5 - (width - radius), 0);
    const cy = Math.max(radius - y - 0.5, y + 0.5 - (height - radius), 0);
    const mask = cx === 0 && cy === 0 ? 1 : Math.min(1, Math.max(0, radius + 0.5 - Math.hypot(cx, cy)));
    const alpha = mask * pixels[input + 3]! / 255;
    for (let channel = 0; channel < 3; channel++) {
      const washed = pixels[input + channel]! * 0.965 + tint[channel]! * 0.035;
      const finished = (washed * (1 - reflection) + 255 * reflection) * (1 - edge);
      result[output + channel] = Math.round(finished * alpha + background[channel]! * (1 - alpha));
    }
    // Composite rounded edges into the theme so Sixel and Kitty look alike.
    result[output + 3] = 255;
  }
  return result;
}

export function glassArtwork(value: HighResolutionArtwork, theme: ThemeName): NativeImage | null {
  if (!value || value.mime !== "image/png" || typeof value.data !== "string" || value.data.length > 3 * 1024 * 1024 || !Number.isInteger(value.width) || !Number.isInteger(value.height) || value.width < 1 || value.height < 1 || value.width > 640 || value.height > 640) return null;
  let original: NativeImage | undefined;
  try {
    const bytes = Buffer.from(value.data, "base64");
    if (bytes.length < 8 || bytes.subarray(0, 8).toString("hex") !== "89504e470d0a1a0a") return null;
    original = NativeImage.decode(bytes);
    if (original.width !== value.width || original.height !== value.height) return null;
    const raw = original.raw();
    return NativeImage.fromRgba(glassPixels(raw.data, raw.width, raw.height, theme, raw.stride), raw.width, raw.height);
  } catch { return null; }
  finally { original?.dispose(); }
}
