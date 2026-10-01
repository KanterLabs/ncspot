/**
 * Theme values used by the Resonance surface.
 *
 * The palettes intentionally use opaque colors. OpenTUI can render alpha, but
 * the faux-glass treatment here is expressed with pale layered tints and
 * highlighted borders so the terminal does not claim to provide blur.
 */
export type ThemeName = "light" | "dark";

export const DEFAULT_THEME: ThemeName = "light";

export interface ThemePalette {
  background: string;
  panel: string;
  panelRaised: string;
  panelHighlight: string;
  border: string;
  borderSoft: string;
  bevelHighlight: string;
  softShadow: string;
  text: string;
  muted: string;
  dim: string;
  accent: string;
  accentBright: string;
  accentDim: string;
  teal: string;
  amber: string;
  red: string;
  cover: string;
  coverBorder: string;
  progressTrack: string;
  queueCurrent: string;
  queueItem: string;
}

const LIGHT_PALETTE: ThemePalette = {
  background: "#eef3f9",
  panel: "#f7faff",
  panelRaised: "#ffffff",
  panelHighlight: "#fbfdff",
  border: "#c6d3e2",
  borderSoft: "#dce5ef",
  bevelHighlight: "#ffffff",
  softShadow: "#d5dee9",
  text: "#19263a",
  muted: "#5f6f84",
  dim: "#64758a",
  accent: "#3b7ed0",
  accentBright: "#2667b9",
  accentDim: "#9ebce1",
  teal: "#2d827c",
  amber: "#9b6718",
  red: "#bd4653",
  cover: "#dceafa",
  coverBorder: "#99b7dc",
  progressTrack: "#d6e0eb",
  queueCurrent: "#a8c7ec",
  queueItem: "#dfe7f0",
};

const DARK_PALETTE: ThemePalette = {
  background: "#080a10",
  panel: "#0f131d",
  panelRaised: "#151b28",
  panelHighlight: "#1b2333",
  border: "#293247",
  borderSoft: "#1c2432",
  bevelHighlight: "#35415b",
  softShadow: "#1c2432",
  text: "#e8edf8",
  muted: "#8a96ad",
  dim: "#58647b",
  accent: "#9c7bff",
  accentBright: "#c8b8ff",
  accentDim: "#4b3e79",
  teal: "#57d5c8",
  amber: "#f6bd72",
  red: "#f07887",
  cover: "#252044",
  coverBorder: "#4b3e79",
  progressTrack: "#293247",
  queueCurrent: "#4b3e79",
  queueItem: "#1c2432",
};

// Palettes are immutable and shared as read-only values; each app keeps its
// current palette reference, so one mounted renderer cannot recolor another.
export const THEME_PALETTES: Readonly<Record<ThemeName, Readonly<ThemePalette>>> = Object.freeze({
  light: Object.freeze(LIGHT_PALETTE),
  dark: Object.freeze(DARK_PALETTE),
});

export function isThemeName(value: unknown): value is ThemeName {
  return value === "light" || value === "dark";
}

export function normalizeTheme(value: unknown, fallback: ThemeName = DEFAULT_THEME): ThemeName {
  return isThemeName(value) ? value : fallback;
}

export function paletteForTheme(theme: ThemeName): Readonly<ThemePalette> {
  return THEME_PALETTES[theme];
}

export function otherTheme(theme: ThemeName): ThemeName {
  return theme === "light" ? "dark" : "light";
}
