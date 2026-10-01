import { DEMO_TRACKS, demoStatus, type ParsedStatus, type Playable, type Track } from "../status.js";
import { RpcError } from "./client.js";
import { RPC_VERSION, type Page, type Params, type Row, type RpcApi } from "./contracts.js";

const source = "offline preview";
const clone = <T>(value: T): T => structuredClone(value);
function fail(code: string, message: string): never { throw new RpcError(code, message); }
const string = (p: Params, key: string): string => typeof p[key] === "string" && p[key] ? p[key] as string : fail("invalid_params", `${key} is required`);
const number = (p: Params, key: string): number => typeof p[key] === "number" && Number.isFinite(p[key]) ? p[key] as number : fail("invalid_params", `${key} must be a finite number`);
const kinds: Record<string, string> = { tracks: "track", albums: "album", artists: "artist", playlists: "playlist", shows: "show", episodes: "episode", categories: "category", browse: "category" };
const singular = (kind: string) => kinds[kind] ?? kind;

/** Fictional, mutable fixtures. This adapter never opens a socket or persists data. */
export class DemoApi implements RpcApi {
  status: ParsedStatus = clone(demoStatus());
  private nextId = 10;
  private revision = 1;
  private currentId = "entry-1";
  private repeat = "off";
  private shuffled = false;
  private played = 0;
  private values: Params = { volume: 68, repeat: "off", shuffle: false, demo: true, source, logged_in: true };
  private tracks: Row[] = DEMO_TRACKS.map(track => ({ id: track.id!, kind: "track", title: track.title, subtitle: track.artists.join(" · "), detail: track.album, uri: `spotify:track:${track.id}`, duration_ms: track.duration, saved: true }));
  private queue: Row[] = this.tracks.map((row, i) => ({ ...row, id: `entry-${i + 1}`, meta: { playable_id: row.id } }));
  private albums: Row[] = DEMO_TRACKS.map((track, i) => ({ id: `album-${i}`, kind: "album", title: track.album, subtitle: track.artists[0]!, uri: `spotify:album:demo-${i}`, saved: true }));
  private artists: Row[] = DEMO_TRACKS.map((track, i) => ({ id: `artist-${i}`, kind: "artist", title: track.artists[0]!, subtitle: "Demo artist", uri: `spotify:artist:demo-${i}`, saved: true }));
  private playlists: Row[] = [{ id: "playlist-1", kind: "playlist", title: "Pearl sessions", subtitle: "Offline demo playlist", uri: "spotify:playlist:demo-1", saved: true, meta: { owner: true, editable: true } }];
  private playlistTracks = new Map<string, Row[]>([["playlist-1", this.tracks.slice(0, 3).map(clone)]]);
  private shows: Row[] = [{ id: "show-1", kind: "show", title: "Signals in Sound", subtitle: "Offline demo podcast", uri: "spotify:show:demo-1", saved: true }];
  private episodes: Row[] = [1, 2, 3].map(i => ({ id: `episode-${i}`, kind: "episode", title: `Demo episode ${i}: Listening closely`, subtitle: "Signals in Sound", uri: `spotify:episode:demo-${i}`, duration_ms: 1_200_000 + i * 60_000, saved: false }));
  private categories: Row[] = [{ id: "category-1", kind: "category", title: "Focus", subtitle: "Offline demo category" }, { id: "category-2", kind: "category", title: "Electronic", subtitle: "Offline demo category" }];
  private casts: Row[] = [{ id: "cast-1", kind: "device", title: "Demo living room", subtitle: "Simulated speaker", meta: { active: false, is_active: false } }, { id: "cast-2", kind: "device", title: "Demo desk", subtitle: "Simulated speaker", meta: { active: false, is_active: false } }];

  constructor(private onStatus?: (status: ParsedStatus) => void) { this.status.playable = this.playable(this.queue[0]!); this.publish(); }
  tick(): void {
    if (this.status.mode.kind !== "playing") return;
    const position = (this.status.prototype?.position_ms ?? 0) + 1_000;
    if (position >= (this.status.playable?.duration ?? Infinity)) this.next(1);
    else { this.status.prototype!.position_ms = position; this.publish(); }
  }
  private publish(): void {
    this.status.prototype!.up_next = this.queue.filter(row => row.id !== this.currentId && row.kind === "track").map(row => this.playable(row) as Track);
    this.onStatus?.(clone(this.status));
  }
  private playable(row: Row): Playable {
    if (row.kind === "episode") return { type: "Episode", id: row.id, uri: row.uri, name: row.title, duration: row.duration_ms ?? 0 };
    const original = DEMO_TRACKS.find(track => row.uri === `spotify:track:${track.id}`);
    return { ...(original ?? DEMO_TRACKS[0]!), id: row.meta?.playable_id as string ?? row.id, uri: row.uri, title: row.title, duration: row.duration_ms ?? 0 };
  }
  private play(row: Row): void { this.currentId = row.id; this.status.playable = this.playable(row); this.status.mode = { kind: "playing", startedAtMs: Date.now() }; this.status.prototype!.position_ms = 0; this.played++; this.publish(); }
  private next(delta: number): void {
    const index = this.queue.findIndex(row => row.id === this.currentId);
    if (!this.queue.length) { this.status.mode = { kind: "stopped" }; this.status.playable = null; this.publish(); return; }
    this.play(this.queue[(index + delta + this.queue.length) % this.queue.length]!);
  }
  private page(items: Row[], p: Params, revision?: string): Page {
    const offset = p.offset === undefined ? 0 : number(p, "offset");
    const limit = p.limit === undefined ? 50 : number(p, "limit");
    if (!Number.isInteger(offset) || offset < 0 || !Number.isInteger(limit) || limit < 1 || limit > 200) fail("invalid_params", "Invalid page range");
    return clone({ items: items.slice(offset, offset + limit), offset, limit, total: items.length, has_more: offset + limit < items.length, source, ...(revision ? { revision } : {}) });
  }
  private group(kind: string): Row[] {
    switch (singular(kind)) { case "track": return this.tracks; case "album": return this.albums; case "artist": return this.artists; case "playlist": return this.playlists; case "show": return this.shows; case "episode": return this.episodes; case "category": return this.categories; default: return fail("invalid_params", `Unknown demo kind: ${kind}`); }
  }
  private byUri(uri: string): Row[] {
    const direct = [...this.tracks, ...this.episodes].find(row => row.uri === uri);
    if (direct) return [direct];
    const playlist = this.playlists.find(row => row.uri === uri);
    if (playlist) return this.playlistTracks.get(playlist.id)!;
    const album = this.albums.findIndex(row => row.uri === uri);
    if (album >= 0) return [this.tracks[album]!];
    const artist = this.artists.findIndex(row => row.uri === uri);
    if (artist >= 0) return [this.tracks[artist]!];
    if (this.shows.some(row => row.uri === uri)) return this.episodes;
    return fail("not_found", "This URI is not in the offline demo fixtures");
  }
  private entries(items: Row[]): Row[] { return items.map(row => ({ ...clone(row), id: `entry-${++this.nextId}`, meta: { playable_id: row.id } })); }
  async call<T = unknown>(method: string, p: Params = {}): Promise<T> { return clone(this.dispatch(method, p)) as T; }
  private dispatch(method: string, p: Params): unknown {
    switch (method) {
      case "session.info": return { instance_id: "offline-demo", version: RPC_VERSION, capabilities: ["player", "queue", "library", "search", "playlists", "podcasts", "radio", "settings", "cast"], source };
      case "queue.list": return this.page(this.queue.map((row, i) => ({ ...row, meta: { ...row.meta, index: i, current: row.id === this.currentId } })), p, String(this.revision));
      case "library.list": {
        let items = this.group(string(p, "kind"));
        if (p.filter !== undefined) { const filter = typeof p.filter === "string" ? p.filter.toLowerCase() : fail("invalid_params", "filter must be a string"); items = items.filter(row => `${row.title} ${row.subtitle}`.toLowerCase().includes(filter)); }
        if (p.sort !== undefined) { const sort = string(p, "sort"); if (!["title", "name", "artist", "added", "recent", "default"].includes(sort)) fail("invalid_params", "Unsupported demo sort"); if (sort === "title" || sort === "name") items = [...items].sort((a, b) => a.title.localeCompare(b.title)); else if (sort === "artist") items = [...items].sort((a, b) => a.subtitle.localeCompare(b.subtitle)); }
        return this.page(items, p);
      }
      case "library.detail": {
        const kind = singular(string(p, "kind")); const id = string(p, "id");
        const group = this.group(kind); const index = group.findIndex(row => row.id === id || row.uri === id || row.uri === p.uri);
        if (index < 0) fail("not_found", "Demo item not found");
        const row = group[index]!;
        const items = kind === "playlist" ? this.playlistTracks.get(row.id)! : kind === "show" ? this.episodes : kind === "category" ? this.playlists : kind === "album" || kind === "artist" ? [this.tracks[index]!] : [row];
        return this.page(items.map((item, i) => ({ ...item, meta: { ...item.meta, position: i } })), p);
      }
      case "search": {
        const query = typeof p.query === "string" ? p.query.toLowerCase() : fail("invalid_params", "query is required");
        const items = p.kind && p.kind !== "all" ? this.group(string(p, "kind")) : [...this.tracks, ...this.albums, ...this.artists, ...this.playlists, ...this.shows, ...this.episodes];
        return this.page(items.filter(row => `${row.title} ${row.subtitle}`.toLowerCase().includes(query)), p);
      }
      case "player.action": return this.player(p);
      case "player.status": return { source, current: this.status.playable, repeat: this.repeat, shuffle: this.shuffled, saved: this.tracks.find(row => row.uri === this.status.playable?.uri)?.saved ?? false };
      case "queue.action": return this.queueAction(p);
      case "library.action": {
        const action = string(p, "action"); if (action === "refresh") return { source, refreshed: true };
        if (action !== "save" && action !== "unsave") fail("invalid_params", "Unsupported library action");
        const items = p.kind ? this.group(string(p, "kind")) : [...this.tracks, ...this.albums, ...this.artists, ...this.playlists, ...this.shows, ...this.episodes];
        const item = items.find(row => row.id === p.id || row.uri === p.uri); if (!item) fail("not_found", "Demo item not found"); item.saved = action === "save"; return { source, saved: item.saved };
      }
      case "playlist.action": return this.playlistAction(p);
      case "radio.status": return { source, active: this.status.prototype!.radio_active, waiting: false, discovery: this.status.prototype!.discovery, played_count: this.played, cache_tracks: this.tracks.length };
      case "radio.action": {
        const action = string(p, "action");
        if (p.uri !== undefined) this.byUri(string(p, "uri"));
        if (action === "start" || action === "stop") this.status.prototype!.radio_active = action === "start";
        else if (action === "discovery") { const value = number(p, "value"); if (value < 0 || value > 100) fail("invalid_params", "Discovery must be 0–100"); this.status.prototype!.discovery = value; }
        else fail("invalid_params", "Unsupported radio action"); this.publish(); return this.dispatch("radio.status", {});
      }
      case "radio.debug": {
        const limit = p.limit === undefined ? 20 : number(p, "limit"); const seed = p.rng_seed === undefined ? 0 : number(p, "rng_seed");
        if (!Number.isSafeInteger(seed) || seed < 0 || !Number.isInteger(limit) || limit < 1 || limit > 200) fail("invalid_params", "Invalid demo diagnostic parameters");
        return { source, demo: true, history_status: "Offline demo; no listening history loaded", enrichment_status: "Illustrative fixtures; no real cache diagnostics", report: { rng_seed: seed, reasons: ["Offline demo candidates; scores unavailable"], candidates: this.tracks.slice(0, limit).map(row => ({ track: { title: row.title, uri: row.uri }, reasons: ["demo fixture"] })) } };
      }
      case "settings.get": return { values: this.values, bindings: { play_pause: "Space", next: "]", previous: "[", queue: "2", search: "/" }, source };
      case "settings.action": return this.settings(p);
      case "cast.list": return this.page(this.casts, p);
      case "cast.action": {
        const action = string(p, "action"); if (action !== "connect" && action !== "disconnect") fail("invalid_params", "Unsupported cast action");
        const target = action === "connect" ? this.casts.find(row => row.id === p.id) : undefined; if (action === "connect" && !target) fail("not_found", "Demo device not found");
        this.casts.forEach(row => { row.meta = { ...row.meta, active: row === target, is_active: row === target }; row.detail = row === target ? "Connected (simulated)" : "Available (simulated)"; }); return { source, connected: target?.id ?? null };
      }
      case "share": { const uri = string(p, "uri"); this.byUri(uri); return { source, url: `Offline demo link (fictional): https://open.spotify.com/${uri.split(":").slice(1).join("/")}`, demo: true }; }
      default: return fail("unknown_method", `Unknown demo method: ${method}`);
    }
  }
  private player(p: Params): unknown {
    const action = string(p, "action"); const proto = this.status.prototype!;
    switch (action) {
      case "play_pause": this.status.mode = this.status.mode.kind === "playing" ? { kind: "paused", positionMs: proto.position_ms } : { kind: "playing", startedAtMs: Date.now() - proto.position_ms }; break;
      case "stop": this.status.mode = { kind: "stopped" }; proto.position_ms = 0; break;
      case "next": this.next(1); break;
      case "previous": this.next(-1); break;
      case "play": { const items = this.byUri(string(p, "uri")); const entries = this.entries(items); this.queue = entries; this.revision++; if (entries[0]) this.play(entries[0]); break; }
      case "seek": { const value = number(p, "value"); if (value < 0) fail("invalid_params", "Seek position must be nonnegative"); proto.position_ms = Math.min(value, this.status.playable?.duration ?? 0); if (this.status.mode.kind === "paused") this.status.mode.positionMs = proto.position_ms; break; }
      case "volume": { const value = number(p, "value"); if (value < 0 || value > 100) fail("invalid_params", "Volume must be 0–100"); proto.volume_percent = value; this.values.volume = value; break; }
      case "repeat": { const value = p.value ?? (this.repeat === "off" ? "all" : this.repeat === "all" ? "track" : "off"); if (!["off", "all", "track"].includes(value as string)) fail("invalid_params", "Invalid repeat mode"); this.values.repeat = this.repeat = value as string; break; }
      case "shuffle": if (p.value !== undefined && typeof p.value !== "boolean") fail("invalid_params", "Shuffle value must be boolean"); this.values.shuffle = this.shuffled = p.value === undefined ? !this.shuffled : p.value as boolean; break;
      default: fail("invalid_params", "Unsupported player action");
    }
    this.publish(); return { source, status: this.status, repeat: this.repeat, shuffle: this.shuffled };
  }
  private queueAction(p: Params): unknown {
    const action = string(p, "action");
    if (!["play", "remove", "move", "clear", "append", "play_next", "save"].includes(action)) fail("invalid_params", "Unsupported queue action");
    if ((p.revision !== undefined || ["play", "remove", "move", "clear", "save"].includes(action)) && String(p.revision) !== String(this.revision)) fail("stale_revision", "Demo queue changed; refresh before retrying");
    if (action === "append" || action === "play_next") {
      const entries = this.entries(this.byUri(string(p, "uri"))); const at = action === "append" ? this.queue.length : Math.max(0, this.queue.findIndex(row => row.id === this.currentId) + 1); this.queue.splice(at, 0, ...entries);
    } else if (action === "clear") { this.queue = []; this.status.playable = null; this.status.mode = { kind: "stopped" }; }
    else if (action === "save") { const name = string(p, "name"); const playlist = this.createPlaylist(name); this.playlistTracks.set(playlist.id, this.queue.map(row => ({ ...row, id: row.meta?.playable_id as string ?? row.id }))); }
    else {
      const entry = string(p, "entry_id"); const index = this.queue.findIndex(row => row.id === entry); if (index < 0) fail("not_found", "Demo queue entry not found");
      if (action === "play") this.play(this.queue[index]!);
      else if (action === "remove") { const wasCurrent = entry === this.currentId; this.queue.splice(index, 1); if (wasCurrent) { if (this.queue.length) this.play(this.queue[Math.min(index, this.queue.length - 1)]!); else { this.status.playable = null; this.status.mode = { kind: "stopped" }; } } }
      else { const to = number(p, "to"); if (!Number.isInteger(to) || to < 0 || to >= this.queue.length) fail("invalid_params", "Invalid queue destination"); this.queue.splice(to, 0, this.queue.splice(index, 1)[0]!); }
    }
    this.revision++; this.publish(); return { source, revision: String(this.revision) };
  }
  private createPlaylist(name: string): Row { const id = `playlist-${++this.nextId}`; const row: Row = { id, kind: "playlist", title: name, subtitle: "Offline demo playlist", uri: `spotify:playlist:${id}`, saved: true, meta: { owner: true, editable: true } }; this.playlists.push(row); this.playlistTracks.set(id, []); return row; }
  private playlistAction(p: Params): unknown {
    const action = string(p, "action"); if (action === "create") return this.createPlaylist(string(p, "name"));
    if (!["rename", "delete", "add", "remove"].includes(action)) fail("invalid_params", "Unsupported playlist action");
    const id = string(p, "id"); const row = this.playlists.find(row => row.id === id || row.uri === id); if (!row) fail("not_found", "Demo playlist not found"); const items = this.playlistTracks.get(row.id)!;
    if (action === "rename") row.title = string(p, "name");
    else if (action === "delete") { this.playlists = this.playlists.filter(item => item !== row); this.playlistTracks.delete(row.id); }
    else if (action === "add") items.push(...this.byUri(string(p, "uri")).map(clone));
    else { const position = number(p, "position"); if (!Number.isInteger(position) || position < 0 || position >= items.length) fail("invalid_params", "Invalid playlist position"); items.splice(position, 1); }
    return { source, id: row.id, action };
  }
  private settings(p: Params): unknown {
    const action = string(p, "action");
    if (action === "reload" || action === "reconnect") { this.values.last_action = `${action} (simulated)`; return { source, action, simulated: true }; }
    if (action === "logout") { this.values.logged_in = false; this.values.last_action = "logout (simulated)"; this.status.mode = { kind: "stopped" }; this.publish(); return { source, logged_in: false }; }
    if (action !== "command") fail("invalid_params", "Unsupported settings action");
    const command = string(p, "command").trim(); const [name, value, extra] = command.split(/\s+/);
    const actions: Record<string, string> = { playpause: "play_pause", next: "next", previous: "previous", stop: "stop", volume: "volume", seek: "seek", repeat: "repeat", shuffle: "shuffle" };
    if (!actions[name!]) fail("unsupported", "This command is not simulated in offline preview");
    if (extra !== undefined || (value !== undefined && !["volume", "seek", "repeat", "shuffle"].includes(name!))) fail("invalid_params", "Unexpected command arguments");
    if (name === "shuffle" && value !== undefined && !["on", "off", "true", "false"].includes(value)) fail("invalid_params", "Invalid shuffle command value");
    const params: Params = { action: actions[name!] }; if (value !== undefined) params.value = name === "volume" || name === "seek" ? Number(value) : name === "shuffle" ? value === "on" || value === "true" : value;
    const result = this.player(params); this.values.last_command = command; return result;
  }
}
