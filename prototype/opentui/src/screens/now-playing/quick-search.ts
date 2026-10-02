import { BoxRenderable, InputRenderable, LayoutEvents, MouseButton, TextRenderable, type Renderable } from "@opentui/core";
import { formatTime } from "../../status.js";
import { paletteForTheme, type ThemeName, type ThemePalette } from "../../theme.js";
import type { Page, Row, ScreenContext, ScreenKey } from "../../workspace/contracts.js";
import { errorMessage, PageLoader } from "../search/model.js";

/**
 * The small search controller used by Now Playing.  It intentionally owns a
 * detached overlay rather than a second screen: the parent remains mounted,
 * and closing the popup only removes the subtree created here.
 */
export interface QuickSearchController {
  open(): void;
  isOpen(): boolean;
  handleKey(key: ScreenKey): boolean;
  setTheme(theme?: ThemeName): void;
  dispose(): void;
}

interface RowView {
  root: BoxRenderable;
  title: TextRenderable;
  artist: TextRenderable;
  duration: TextRenderable;
}

interface PanelSize {
  width: number;
  height: number;
  rows: number;
  heading: boolean;
  status: boolean;
  footer: boolean;
}

type SearchAction = "play" | "play_next" | "append";

const MAX_ROWS = 12;
const SEARCH_DELAY = 150;

function printableKey(key: ScreenKey): boolean {
  if (key.ctrl || key.meta) return false;
  const value = key.sequence ?? key.name ?? "";
  return value.length === 1 && value >= " " && value !== "\u007f";
}

function keyName(key: ScreenKey): string {
  return (key.name ?? key.sequence ?? "").toLowerCase();
}

function actionLabel(action: SearchAction): string {
  if (action === "play") return "Playing";
  if (action === "play_next") return "Will play next";
  return "Added to queue";
}

export function createQuickSearch(ctx: ScreenContext, parent: BoxRenderable): QuickSearchController {
  let disposed = false;
  let openState = false;
  let theme = ctx.theme();
  let colors: Readonly<ThemePalette> = paletteForTheme(theme);
  let layer: BoxRenderable | undefined;
  let panel: BoxRenderable | undefined;
  let heading: TextRenderable | undefined;
  let input: InputRenderable | undefined;
  let status: TextRenderable | undefined;
  let resultBox: BoxRenderable | undefined;
  let footer: BoxRenderable | undefined;
  let rows: Row[] = [];
  let rowViews: RowView[] = [];
  let actionButtons: TextRenderable[] = [];
  let selected = -1;
  let firstVisible = 0;
  let query = "";
  let message = "Type to search tracks";
  let messageError = false;
  let searchPending = false;
  let debounce: ReturnType<typeof setTimeout> | undefined;
  let queryGeneration = 0;
  let pendingAction = false;
  let actionToken = 0;
  let previousFocus: Renderable | null = null;
  let lastClick: { identity: string; at: number } | undefined;
  let laidOutParentWidth = -1;
  let laidOutParentHeight = -1;

  const loader = new PageLoader(
    ctx.api,
    (page: Page) => {
      if (disposed || !openState) return;
      const selectedIdentity = rows[selected]?.uri ?? rows[selected]?.id;
      rows = page.items.filter(row => !!row && typeof row.uri === "string" && row.uri.length > 0);
      const retained = selectedIdentity ? rows.findIndex(row => row.uri === selectedIdentity || row.id === selectedIdentity) : -1;
      selected = rows.length ? retained >= 0 ? retained : Math.max(0, Math.min(selected < 0 ? 0 : selected, rows.length - 1)) : -1;
      adjustWindow();
      paint();
    },
    (value: string) => {
      if (disposed || !openState) return;
      // `r` is printable query text while this input has focus, so expose the
      // retry affordance through its non-input Ctrl+R binding instead.
      message = value.replace(/\br retry\b/g, "Ctrl+R retry");
      if (!/loading|refreshing…/i.test(value)) searchPending = false;
      messageError = /request failed|failed:/i.test(value);
      paint();
    },
  );

  function requestRender() {
    if (!disposed) ctx.renderer.requestRender();
  }

  function panelSize(): PanelSize {
    const width = Math.max(1, parent.width || ctx.renderer.width);
    const height = Math.max(1, parent.height || ctx.renderer.height);
    // Leave a cell of breathing room around the bounded panel, but keep a
    // useful minimum on narrow terminals.  The final Math.min handles the
    // tiny-height case where even that minimum cannot fit.
    const panelWidth = Math.max(32, Math.min(84, width - 4));
    const availableHeight = Math.max(1, height - 2);
    const panelHeight = Math.max(1, Math.min(21, availableHeight));
    // Four cells are consumed by border and padding. Keep the input visible
    // first, then add optional heading/status/footer lines as space allows.
    const innerHeight = Math.max(1, panelHeight - 4);
    const showHeading = innerHeight >= 3;
    const showStatus = innerHeight >= 4;
    const showFooter = innerHeight >= 5;
    const fixed = 1 + (showHeading ? 1 : 0) + (showStatus ? 1 : 0) + (showFooter ? 1 : 0);
    const rowLimit = Math.max(1, Math.min(MAX_ROWS, innerHeight - fixed));
    return { width: Math.max(1, Math.min(width, panelWidth)), height: panelHeight, rows: rowLimit, heading: showHeading, status: showStatus, footer: showFooter };
  }

  function layout() {
    if (!panel) return;
    const size = panelSize();
    panel.width = size.width;
    panel.height = size.height;
    if (resultBox) resultBox.height = size.rows;
    if (heading) heading.visible = size.heading;
    if (status) status.visible = size.status;
    if (footer) footer.visible = size.footer;
    adjustWindow();
    paintRows();
    laidOutParentWidth = parent.width;
    laidOutParentHeight = parent.height;
    requestRender();
  }

  function paintRows() {
    const size = panelSize();
    rowViews.forEach((view, slot) => {
      const index = firstVisible + slot;
      const row = rows[index];
      const visible = !!row && slot < size.rows;
      view.root.visible = visible;
      if (!row || !visible) return;
      const chosen = index === selected;
      const rowBackground = chosen ? colors.queueCurrent : colors.panelRaised;
      view.root.backgroundColor = rowBackground;
      view.title.fg = colors.text;
      view.artist.fg = chosen ? colors.text : colors.muted;
      view.duration.fg = chosen ? colors.text : colors.dim;
      view.title.bg = rowBackground;
      view.artist.bg = rowBackground;
      view.duration.bg = rowBackground;
      view.title.content = row.title.replace(/[\r\n\t]/g, " ");
      view.artist.content = row.subtitle.replace(/[\r\n\t]/g, " ");
      view.duration.content = typeof row.duration_ms === "number" ? formatTime(row.duration_ms) : "";
    });
  }

  function paint() {
    if (disposed || !panel) return;
    panel.backgroundColor = colors.panelRaised;
    panel.borderColor = colors.accent;
    if (heading) { heading.fg = colors.text; heading.bg = colors.panelRaised; heading.content = "QUICK SEARCH"; }
    if (status) { status.fg = messageError ? colors.red : colors.muted; status.bg = colors.panelRaised; status.content = message; }
    if (input) {
      input.textColor = colors.text;
      input.backgroundColor = colors.panel;
      input.focusedTextColor = colors.text;
      input.focusedBackgroundColor = colors.panel;
      input.placeholderColor = colors.muted;
    }
    if (resultBox) resultBox.backgroundColor = colors.panelRaised;
    if (footer) footer.backgroundColor = colors.panelRaised;
    actionButtons.forEach(button => { button.fg = colors.accentBright; button.bg = colors.panelRaised; });
    paintRows();
    requestRender();
  }

  function select(index: number) {
    if (!rows.length) { selected = -1; paintRows(); return; }
    selected = Math.max(0, Math.min(rows.length - 1, index));
    adjustWindow();
    paintRows();
    requestRender();
  }

  function adjustWindow() {
    const visible = panelSize().rows;
    if (selected < 0) { firstVisible = 0; return; }
    if (selected < firstVisible) firstVisible = selected;
    else if (selected >= firstVisible + visible) firstVisible = selected - visible + 1;
    const maximum = Math.max(0, rows.length - visible);
    firstVisible = Math.max(0, Math.min(firstVisible, maximum));
  }

  function clearDebounce() {
    if (debounce !== undefined) { clearTimeout(debounce); debounce = undefined; }
  }

  function clearSearchState(value = "Type to search tracks") {
    clearDebounce();
    ++queryGeneration;
    loader.invalidate();
    searchPending = false;
    rows = [];
    selected = -1;
    firstVisible = 0;
    message = value;
    messageError = false;
    paint();
  }

  function loadSearch(generation: number, force = false) {
    if (disposed || !openState || generation !== queryGeneration) return;
    const value = query.trim();
    if (!value) { clearSearchState(); return; }
    searchPending = true;
    void loader.load("search", { query: value, kind: "tracks", offset: 0, limit: 20, ...(force ? { refresh: true } : {}) });
  }

  function scheduleSearch(value: string) {
    query = value;
    clearDebounce();
    ++queryGeneration;
    const generation = queryGeneration;
    loader.invalidate();
    searchPending = false;
    rows = [];
    selected = -1;
    firstVisible = 0;
    if (!value.trim()) {
      message = "Type to search tracks";
      messageError = false;
      paint();
      return;
    }
    message = "Waiting to search…";
    messageError = false;
    paint();
    debounce = setTimeout(() => { debounce = undefined; loadSearch(generation); }, SEARCH_DELAY);
  }

  function action(actionName: SearchAction, index = selected) {
    if (disposed || !openState || pendingAction) return;
    const row = rows[index];
    if (!row?.uri) {
      const waiting = debounce !== undefined || searchPending;
      clearDebounce();
      if (waiting && query.trim()) {
        const generation = queryGeneration;
        if (!searchPending) loadSearch(generation);
        message = "Searching… select a result";
      } else message = "No track selected";
      messageError = false;
      paint();
      return;
    }
    const actionGeneration = queryGeneration;
    const token = ++actionToken;
    pendingAction = true;
    message = `${actionName === "play" ? "Playing" : actionName === "play_next" ? "Queuing next" : "Adding to queue"}…`;
    messageError = false;
    paint();
    const method = actionName === "play" ? "player.action" : "queue.action";
    void ctx.api.call(method, { action: actionName, uri: row.uri }).then(() => {
      if (disposed || !openState || actionGeneration !== queryGeneration || token !== actionToken) return;
      ctx.notify(`${actionLabel(actionName)}: ${row.title}`);
      close();
    }).catch(error => {
      if (disposed || !openState || actionGeneration !== queryGeneration || token !== actionToken) return;
      pendingAction = false;
      message = `Action failed: ${errorMessage(error)} • retry`;
      messageError = true;
      paint();
    }).finally(() => {
      if (!disposed && token === actionToken) {
        pendingAction = false;
        if (openState) paint();
      }
    });
  }

  function button(id: string, content: string, run: () => void): TextRenderable {
    const node = new TextRenderable(ctx.renderer, {
      id,
      content,
      height: 1,
      flexGrow: 1,
      flexShrink: 1,
      flexBasis: 0,
      minWidth: 0,
      textAlign: "center",
      truncate: true,
      selectable: false,
      fg: colors.accentBright,
      bg: colors.panelRaised,
      onMouseDown: event => {
        if (event.button === MouseButton.LEFT) { run(); event.preventDefault(); }
      },
    });
    actionButtons.push(node);
    footer?.add(node);
    return node;
  }

  function buildOverlay() {
    const size = panelSize();
    layer = new BoxRenderable(ctx.renderer, {
      id: "np-quick-search-layer",
      position: "absolute",
      left: 0,
      top: 0,
      width: "100%",
      height: "100%",
      zIndex: 100,
      alignItems: "center",
      justifyContent: "center",
      backgroundColor: "transparent",
    });
    panel = new BoxRenderable(ctx.renderer, {
      id: "np-quick-search",
      width: size.width,
      height: size.height,
      minWidth: 1,
      minHeight: 1,
      maxWidth: "100%",
      maxHeight: "100%",
      border: true,
      borderStyle: "rounded",
      paddingX: 1,
      paddingY: 1,
      flexDirection: "column",
      backgroundColor: colors.panelRaised,
      borderColor: colors.accent,
    });
    heading = new TextRenderable(ctx.renderer, { id: "np-quick-search-heading", content: "QUICK SEARCH", height: 1, flexShrink: 0, truncate: true, selectable: false, fg: colors.text, bg: colors.panelRaised });
    input = new InputRenderable(ctx.renderer, {
      id: "np-quick-search-input",
      value: "",
      width: "100%",
      flexShrink: 0,
      placeholder: "Search tracks…",
      textColor: colors.text,
      backgroundColor: colors.panel,
      focusedTextColor: colors.text,
      focusedBackgroundColor: colors.panel,
      placeholderColor: colors.muted,
    });
    status = new TextRenderable(ctx.renderer, { id: "np-quick-search-status", content: message, height: 1, flexShrink: 0, truncate: true, selectable: false, fg: colors.muted, bg: colors.panelRaised });
    resultBox = new BoxRenderable(ctx.renderer, { id: "np-quick-search-results", width: "100%", height: size.rows, flexGrow: 1, minHeight: 1, flexShrink: 1, overflow: "hidden", flexDirection: "column", backgroundColor: colors.panelRaised });
    footer = new BoxRenderable(ctx.renderer, { id: "np-quick-search-footer", width: "100%", height: 1, flexShrink: 0, flexDirection: "row", gap: 1, backgroundColor: colors.panelRaised });
    panel.add(heading); panel.add(input); panel.add(status); panel.add(resultBox); panel.add(footer);

    for (let index = 0; index < MAX_ROWS; index++) {
      const root = new BoxRenderable(ctx.renderer, {
        id: `np-quick-search-row-${index}`,
        width: "100%",
        height: 1,
        flexShrink: 0,
        minWidth: 0,
        flexDirection: "row",
        gap: 1,
        backgroundColor: colors.panelRaised,
        visible: false,
        onMouseDown: event => {
          if (event.button !== MouseButton.LEFT || !openState) return;
          const actualIndex = firstVisible + index;
          const row = rows[actualIndex];
          if (!row) return;
          select(actualIndex);
          const now = Date.now();
          const identity = row.uri ?? row.id;
          if (lastClick?.identity === identity && now - lastClick.at < 400) {
            lastClick = undefined;
            action("play", actualIndex);
          } else lastClick = { identity, at: now };
          event.preventDefault();
        },
      });
      const title = new TextRenderable(ctx.renderer, { id: `np-quick-search-row-${index}-title`, content: "", height: 1, flexGrow: 2, flexShrink: 1, flexBasis: 0, minWidth: 0, truncate: true, selectable: false, fg: colors.text, bg: colors.panelRaised });
      const artist = new TextRenderable(ctx.renderer, { id: `np-quick-search-row-${index}-artist`, content: "", height: 1, flexGrow: 1, flexShrink: 1, flexBasis: 0, minWidth: 0, truncate: true, selectable: false, fg: colors.muted, bg: colors.panelRaised });
      const duration = new TextRenderable(ctx.renderer, { id: `np-quick-search-row-${index}-duration`, content: "", width: 6, height: 1, flexShrink: 0, textAlign: "right", truncate: true, selectable: false, fg: colors.dim, bg: colors.panelRaised });
      root.add(title); root.add(artist); root.add(duration); resultBox.add(root);
      rowViews.push({ root, title, artist, duration });
    }
    button("np-quick-search-play", "▶ Play · Enter", () => action("play"));
    button("np-quick-search-next", "Ctrl+N Next", () => action("play_next"));
    button("np-quick-search-queue", "Ctrl+E Queue", () => action("append"));
    button("np-quick-search-close", "Esc Close", () => close());

    input.on("input", value => scheduleSearch(String(value)));
    input.on("enter", value => {
      if (typeof value === "string" && value !== query) { scheduleSearch(value); return; }
      action("play");
    });
    layer.add(panel);
    parent.add(layer);
    layout();
    paint();
    input.focus();
  }

  function close(restoreFocus = true) {
    if (!openState) return;
    openState = false;
    ++actionToken;
    clearDebounce();
    ++queryGeneration;
    loader.invalidate();
    searchPending = false;
    pendingAction = false;
    input?.removeAllListeners();
    input?.blur();
    const owned = layer;
    layer = undefined; panel = undefined; heading = undefined; input = undefined; status = undefined; resultBox = undefined; footer = undefined; rowViews = []; actionButtons = [];
    if (owned) {
      if (owned.parent === parent) parent.remove(owned);
      if (!owned.isDestroyed) owned.destroyRecursively();
    }
    lastClick = undefined;
    if (restoreFocus && previousFocus && !previousFocus.isDestroyed) previousFocus.focus();
    previousFocus = null;
    requestRender();
  }

  function open() {
    if (disposed) return;
    if (openState) { input?.focus(); return; }
    openState = true;
    previousFocus = ctx.renderer.currentFocusedRenderable;
    query = "";
    clearSearchState();
    buildOverlay();
  }

  function onResize() { if (openState) layout(); }
  function onFrame() {
    if (openState && (parent.width !== laidOutParentWidth || parent.height !== laidOutParentHeight)) layout();
  }
  ctx.renderer.on("resize", onResize);
  ctx.renderer.on("frame", onFrame);
  parent.on(LayoutEvents.RESIZED, onResize);

  const controller: QuickSearchController = {
    open,
    isOpen: () => openState,
    handleKey(key) {
      if (disposed || !openState) return false;
      const name = keyName(key);
      if (name === "escape") { close(); return true; }
      if (key.ctrl || key.meta) {
        if (key.meta) return false;
        if (name === "n" || key.sequence === "\u000e") { action("play_next"); return true; }
        if (name === "e" || key.sequence === "\u0005") { action("append"); return true; }
        if (name === "r" || key.sequence === "\u0012") {
          clearDebounce();
          const generation = queryGeneration;
          loadSearch(generation, true);
          return true;
        }
        // Native text editing uses modified keys such as Ctrl+A/E/U/W and
        // Alt+word movement. Let the focused input process all other ones.
        return false;
      }
      if (name === "up") { select(selected - 1); return true; }
      if (name === "down") { select(selected + 1); return true; }
      if (name === "pageup") { select(selected - Math.max(1, panelSize().rows)); return true; }
      if (name === "pagedown") { select(selected + Math.max(1, panelSize().rows)); return true; }
      if (name === "return" || name === "enter") { action("play"); return true; }
      // Let the focused native input own printable text and its editing keys.
      // A one-character name is common in direct ScreenKey tests, while real
      // OpenTUI events generally carry it in sequence.
      if (printableKey(key) || ["backspace", "delete", "left", "right", "home", "end", "tab"].includes(name)) return false;
      // The popup is modal while open: unknown non-input keys must not leak to
      // Now Playing or the workspace's global shortcuts.
      return true;
    },
    setTheme(next = ctx.theme()) {
      if (disposed) return;
      theme = next;
      colors = paletteForTheme(theme);
      paint();
    },
    dispose() {
      if (disposed) return;
      disposed = true;
      clearDebounce();
      loader.dispose();
      ctx.renderer.off("resize", onResize);
      ctx.renderer.off("frame", onFrame);
      parent.off(LayoutEvents.RESIZED, onResize);
      ++actionToken;
      close(false);
      rows = [];
      previousFocus = null;
    },
  };
  return controller;
}
