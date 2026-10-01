import type { ParsedStatus, Playable, Track } from "../../status.js";

export const MOTION = { track: 420, queue: 240, refill: 850, button: 420, cover: 480, tint: 700 } as const;

export function ease(value: number): number {
  const t = Math.max(0, Math.min(1, value));
  return 1 - (1 - t) ** 3;
}

export function playableKey(value: Playable | null | undefined): string {
  if (!value) return "";
  if (value.uri) return value.uri;
  if (value.id) return `spotify:${value.type === "Episode" ? "episode" : "track"}:${value.id}`;
  return JSON.stringify([value.type === "Episode" ? value.name : value.title, value.type === "Episode" ? "" : value.artists, value.duration]);
}

/** Occurrence suffixes keep explicitly queued duplicates separate. */
export function queueKeys(tracks: readonly Track[]): string[] {
  const occurrences = new Map<string, number>();
  return tracks.map(track => {
    const key = playableKey(track);
    const occurrence = occurrences.get(key) ?? 0;
    occurrences.set(key, occurrence + 1);
    return `${key}:${occurrence}`;
  });
}

/** Finite effects are triggered by changed state, never by a render or heartbeat. */
export class PlayerMotion {
  trackAt = -Infinity;
  advanceAt = -Infinity;
  departing: Track | null = null;
  readonly refills = new Map<string, number>();
  readonly buttons = new Map<string, number>();

  observe(previous: ParsedStatus | null, next: ParsedStatus, now: number, reduced: boolean): void {
    const before = previous?.prototype?.up_next ?? [];
    const after = next.prototype?.up_next ?? [];
    const beforeKeys = new Set(queueKeys(before));
    const afterKeys = queueKeys(after);
    const changed = playableKey(previous?.playable) !== playableKey(next.playable);
    const advanced = changed && !!before[0] && playableKey(before[0]) === playableKey(next.playable);
    if (!reduced && changed) this.trackAt = now;
    if (!reduced && advanced) { this.advanceAt = now; this.departing = before[0]!; }
    // Advancing the 16-item window can reveal one existing track. It is not
    // evidence of a radio refill, so do not flash it as a new recommendation.
    if (!reduced && previous && next.prototype?.radio_active && !advanced) {
      for (const key of afterKeys) if (!beforeKeys.has(key)) this.refills.set(key, now);
    }
    for (const [key, started] of this.refills) {
      if (!afterKeys.includes(key) || now - started >= MOTION.refill) this.refills.delete(key);
    }
    if (reduced) this.settle();
  }

  pulse(key: string, now: number): void { this.buttons.set(key, now); }
  trackProgress(now: number, delay = 0): number { return ease((now - this.trackAt - delay) / MOTION.track); }
  queueOffset(now: number): number { return 1 - ease((now - this.advanceAt) / MOTION.queue); }
  refill(key: string, now: number): number {
    const at = this.refills.get(key);
    return at === undefined ? 0 : Math.sin(Math.PI * Math.max(0, Math.min(1, (now - at) / MOTION.refill)));
  }
  button(key: string, now: number): number {
    const at = this.buttons.get(key);
    return at === undefined ? 0 : 1 - ease((now - at) / MOTION.button);
  }
  active(now: number): boolean {
    if (now - this.advanceAt >= MOTION.queue) this.departing = null;
    for (const [key, at] of this.buttons) if (now - at >= MOTION.button) this.buttons.delete(key);
    for (const [key, at] of this.refills) if (now - at >= MOTION.refill) this.refills.delete(key);
    return now - this.trackAt < MOTION.track + 150 || now - this.advanceAt < MOTION.queue || this.refills.size > 0 || this.buttons.size > 0;
  }
  settle(): void { this.trackAt = this.advanceAt = -Infinity; this.departing = null; this.refills.clear(); this.buttons.clear(); }
}

/** The icon represents playback. When samples exist, its bars use those samples. */
export function playingBars(playing: boolean, bands: readonly number[] | undefined, level: number | undefined, now: number, reduced: boolean): string {
  if (!playing || level === 0) return "▁▁▁";
  if (reduced) return "▃▃▃";
  if (!bands?.length) return ["▂▄▂", "▄▆▄", "▆▄▂", "▄▂▄"][Math.floor(now / 160) % 4]!;
  const characters = "▁▂▃▄▅▆▇█";
  return Array.from({ length: 3 }, (_, index) => {
    const start = Math.floor(index * bands.length / 3);
    const end = Math.max(start + 1, Math.floor((index + 1) * bands.length / 3));
    const average = bands.slice(start, end).reduce((sum, value) => sum + value, 0) / (end - start);
    return characters[Math.min(7, Math.floor(Math.max(0, Math.min(1, average)) * 8))]!;
  }).join("");
}
