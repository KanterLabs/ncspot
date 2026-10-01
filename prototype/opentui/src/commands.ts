export interface KeyLike {
  name?: string;
  sequence?: string;
  ctrl?: boolean;
  meta?: boolean;
}

/** Translate OpenTUI key events to the ncspot command strings accepted by IPC. */
export function commandForKey(key: KeyLike, discovery = 50): string | "quit" | null {
  if (key.ctrl || key.meta) return null;
  const name = (key.name ?? key.sequence ?? "").toLowerCase();

  switch (name) {
    case "q":
    case "escape":
    case "esc":
    case "f5":
      return "quit";
    case "space":
    case "enter":
      return "playpause";
    case "left":
    case "p":
      return "previous";
    case "right":
    case "n":
      return "next";
    case "r":
      return "radio";
    case "d":
      return `discovery ${nextDiscovery(discovery)}`;
    case "+":
    case "=":
    case "up":
      return "volup";
    case "-":
    case "down":
      return "voldown";
    case "l":
      // Theme is frontend-local; the key handler consumes it before command
      // dispatch so it can never become an ncspot IPC command.
      return null;
    default:
      return null;
  }
}

export function nextDiscovery(level: number): number {
  const levels = [0, 25, 50, 75, 100];
  const current = levels.findIndex((candidate) => candidate >= Math.max(0, Math.min(100, level)));
  return levels[(current < 0 ? 0 : current + 1) % levels.length];
}
