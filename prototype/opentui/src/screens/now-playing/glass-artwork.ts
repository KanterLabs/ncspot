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
  for (let y = 0; y < height; y++) for (let x = 0; x < width; x++) {
    const input = y * stride + x * 4, output = (y * width + x) * 4;
    const nx = (x + 0.5) / width, ny = (y + 0.5) / height;
    // A restrained diagonal reflection and edge shade; no blur of the artwork.
    const reflection = Math.max(0, 1 - Math.abs(nx + ny * 0.75 - 0.32) / 0.26) * (theme === "light" ? 0.09 : 0.055);
    const edge = Math.max(0, Math.hypot(nx - 0.5, ny - 0.5) - 0.38) * 0.13;
    // Keep every source pixel, including the corners.  Rounded masking here
    // clipped the native image before Kitty/Sixel had a chance to fit it.
    const alpha = pixels[input + 3]! / 255;
    for (let channel = 0; channel < 3; channel++) {
      const washed = pixels[input + channel]! * 0.965 + tint[channel]! * 0.035;
      const finished = (washed * (1 - reflection) + 255 * reflection) * (1 - edge);
      result[output + channel] = Math.round(finished * alpha + background[channel]! * (1 - alpha));
    }
    // Composite transparency into the theme so Sixel and Kitty look alike.
    result[output + 3] = 255;
  }
  return result;
}

type NativeArtworkRenderer = Pick<CliRenderer, "resolution" | "width" | "height">;

/**
 * Add a small theme-coloured letterbox when cell rounding would stretch the
 * source. OpenTUI allocates whole cells and then rounds their physical pixel
 * size independently, so a square source can otherwise become 200x208px.
 * Returning null leaves the caller's image ownership unchanged.
 */
export function letterboxNativeArtwork(
  image: NativeImage,
  renderer: NativeArtworkRenderer,
  targetWidth: number,
  targetHeight: number,
  background: readonly [number, number, number, number],
): NativeImage | null {
  const resolution = renderer.resolution;
  const terminalWidth = renderer.width;
  const terminalHeight = renderer.height;
  if (!resolution || resolution.width <= 0 || resolution.height <= 0 || terminalWidth <= 0 || terminalHeight <= 0 || targetWidth <= 0 || targetHeight <= 0) return null;

  const pixelWidth = Math.max(1, Math.round((targetWidth * resolution.width) / terminalWidth));
  const pixelHeight = Math.max(1, Math.round((targetHeight * resolution.height) / terminalHeight));
  const physicalAspect = pixelWidth / pixelHeight;
  if (!Number.isFinite(physicalAspect) || physicalAspect <= 0) return null;

  // Make the complete source fit the physical viewport. ImageRenderable then
  // fits this canvas into the same cells, leaving the added pixels as a
  // centred letterbox instead of stretching or cropping the source.
  let canvasWidth = image.width;
  let canvasHeight = image.height;
  const sourceAspect = image.width / image.height;
  if (sourceAspect > physicalAspect) {
    canvasHeight = Math.max(image.height, Math.ceil(image.width / physicalAspect));
  } else if (sourceAspect < physicalAspect) {
    canvasWidth = Math.max(image.width, Math.ceil(image.height * physicalAspect));
  }
  // Geometry is terminal-reported input; keep a pathological ratio from
  // turning a 640px cover into an unbounded padding allocation.
  const maxCanvasDimension = Math.max(image.width, image.height) * 2;
  if (canvasWidth > maxCanvasDimension || canvasHeight > maxCanvasDimension) return null;
  if (canvasWidth === image.width && canvasHeight === image.height) return null;

  const horizontal = canvasWidth - image.width;
  const vertical = canvasHeight - image.height;
  return image.extend({
    top: Math.floor(vertical / 2),
    right: Math.floor(horizontal / 2),
    bottom: vertical - Math.floor(vertical / 2),
    left: horizontal - Math.floor(horizontal / 2),
    background,
  });
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
