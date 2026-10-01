import { TextRenderable } from "@opentui/core";
import { formatTime, positionAt } from "../../status.js";
import { paletteForTheme } from "../../theme.js";
import type { Row, ScreenFactory } from "../../workspace/contracts.js";
import { createSurface } from "../../workspace/surface.js";
import { PodcastsController } from "./controller.js";

export const createPodcastsScreen: ScreenFactory = (ctx, params = {}) => {
  const surface = createSurface(ctx, "Podcasts", "Enter open/play · Esc back · / filter · s save · a queue · n next in queue · [ ] pages · r refresh");
  const playing = new TextRenderable(ctx.renderer, {
    height: 1, flexShrink: 0, width: "100%", truncate: true, selectable: false,
    fg: paletteForTheme(ctx.theme()).accent,
    onMouseDown(event) { if (event.button === 0) { ctx.navigate("now-playing"); event.preventDefault(); } },
  });
  surface.body.add(playing, 0);
  const controller = new PodcastsController(ctx.api, {
    rows: () => render(), message: text => surface.setMessage(text, text.startsWith("Could not") || text.startsWith("Podcast action failed")),
    notify: text => ctx.notify(text),
  });
  function activate(row: Row) {
    if (row.kind === "show") void controller.open(row);
    else void controller.episodeAction(row, "play");
  }
  function render() {
    const status = ctx.status();
    const episode = status?.playable?.type === "Episode" ? status.playable : undefined;
    playing.visible = !!episode;
    playing.content = episode ? `Now playing ↗ ${episode.name} · ${formatTime(positionAt(status!))} / ${formatTime(episode.duration)} · p to open` : "";
    surface.heading.content = controller.show ? `Podcasts › ${controller.show.title}${controller.show.saved === false ? " · unsaved" : ""}` : `Podcasts${controller.filter ? ` · ${controller.filter}` : ""}`;
    surface.setLines(controller.show?.detail ? [controller.show.detail] : []);
    const rows = controller.rows.map(row => {
      if (row.kind !== "episode") return row;
      const resume = row.meta?.resume_position_ms ?? (row.meta?.resume_point as Record<string, unknown> | undefined)?.resume_position_ms;
      const current = !!episode && ((episode.uri && episode.uri === row.uri) || (!!episode.id && episode.id === row.id));
      const info = [row.subtitle, typeof row.duration_ms === "number" ? formatTime(row.duration_ms) : "",
        current ? `▶ ${formatTime(positionAt(status!))}` : typeof resume === "number" && resume > 0 ? `Resume ${formatTime(resume)}` : "",
        row.meta?.fully_played === true ? "Played" : ""].filter(Boolean).join(" · ");
      return { ...row, subtitle: info };
    });
    surface.setRows(rows, undefined, activate);
  }
  const unsubscribe = ctx.onStatus(render);
  let disposed = false;
  if (typeof params.id === "string") {
    void controller.open({ id: params.id, kind: "show", title: typeof params.title === "string" ? params.title : "Show episodes", subtitle: "", uri: typeof params.uri === "string" ? params.uri : `spotify:show:${params.id}`, saved: typeof params.saved === "boolean" ? params.saved : undefined });
  } else void controller.refresh();
  return {
    root: surface.root, title: "Podcasts", editing: () => surface.editing(),
    handleKey(key) {
      if (surface.handleKey(key)) return true;
      if (surface.editing()) return false;
      if (key.ctrl || key.meta) return false;
      const name = key.name?.toLowerCase() ?? key.sequence;
      const selected = surface.selected();
      if (name === "return" || name === "enter") { if (selected) activate(selected); return true; }
      if (name === "escape" || name === "backspace") { if (!controller.show) return false; void controller.back(); return true; }
      if (name === "/") { if (!controller.show) surface.prompt("Filter saved shows", controller.filter, value => { void controller.search(value); }); return true; }
      if (name === "r") { void controller.refresh(); return true; }
      if (name === "]") { void controller.next(); return true; }
      if (name === "[") { void controller.previous(); return true; }
      if (name === "p") { ctx.navigate("now-playing"); return true; }
      if (name === "s") { const show = controller.show ?? selected; if (show) void controller.toggleSaved(show); return true; }
      if (name === "a" || name === "n") { if (selected) void controller.episodeAction(selected, name === "a" ? "append" : "play_next"); return true; }
      return false;
    },
    refresh: () => controller.refresh(),
    setTheme(theme) { surface.setTheme(theme); playing.fg = paletteForTheme(theme).accent; },
    dispose() { if (disposed) return; disposed = true; unsubscribe(); controller.dispose(); surface.dispose(); },
  };
};
