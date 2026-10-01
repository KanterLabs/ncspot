import type { Row } from "../../workspace/contracts.js";

export const GLOBAL_SHORTCUTS = [
  ["1", "Now Playing"], ["2", "Queue"], ["3", "Library"], ["4", "Search"],
  ["5", "Playlists"], ["6", "Podcasts"], ["7", "Radio"], ["8", "Settings"],
  ["9", "Cast"], ["b", "Browse"], ["?", "Help"], [":", "Command palette"],
] as const;

export function helpRows(bindings: Record<string, string>): Row[] {
  return [
    { id: "command", kind: "action", title: "Run ncspot command", subtitle: ": or Enter · existing CLI commands and aliases" },
    ...GLOBAL_SHORTCUTS.map(([key, title]) => ({ id: `global:${key}`, kind: "shortcut", title: `${key}  ${title}`, subtitle: "Workspace shortcut" })),
    ...Object.entries(bindings).sort(([a], [b]) => a.localeCompare(b)).map(([key, command]) => ({ id: `binding:${key}`, kind: "binding", title: key, subtitle: command })),
  ];
}
