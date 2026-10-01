import type { BoxRenderable, CliRenderer } from "@opentui/core";
import type { ParsedStatus } from "../status.js";
import type { ThemeName } from "../theme.js";

export const RPC_VERSION = 1;
export const ROUTES = ["now-playing", "queue", "library", "search", "browse", "playlists", "podcasts", "radio", "settings", "help", "cast"] as const;
export type Route = typeof ROUTES[number];
export type Params = Record<string, unknown>;
export interface Row {
  id: string;
  kind: string;
  title: string;
  subtitle: string;
  detail?: string;
  uri?: string;
  duration_ms?: number;
  saved?: boolean;
  meta?: Params;
}
export interface Page<T = Row> {
  items: T[];
  offset: number;
  limit: number;
  total: number;
  has_more: boolean;
  source?: string;
  revision?: string;
  refresh_available?: boolean;
}
export interface RpcApi {
  call<T = unknown>(method: string, params?: Params): Promise<T>;
}
export interface ScreenKey {
  name?: string;
  sequence?: string;
  ctrl?: boolean;
  meta?: boolean;
  shift?: boolean;
}
export interface ScreenContext {
  renderer: CliRenderer;
  api: RpcApi;
  theme(): ThemeName;
  setTheme(theme: ThemeName): void;
  reducedMotion(): boolean;
  setReducedMotion(value: boolean): void;
  status(): ParsedStatus | null;
  onStatus(listener: (status: ParsedStatus) => void): () => void;
  navigate(route: Route, params?: Params): void;
  notify(message: string): void;
}
export interface Screen {
  root: BoxRenderable;
  title: string;
  handleKey(key: ScreenKey): boolean;
  /** True while typing, so global letter shortcuts do not steal input. */
  editing?(): boolean;
  refresh(): void | Promise<void>;
  setTheme(theme: ThemeName): void;
  dispose(): void;
}
export type ScreenFactory = (context: ScreenContext, params?: Params) => Screen;

/** API vocabulary: all requests are newline JSON {protocol:"resonance",version:1,id,method,params}.
 * Responses carry {protocol,version,id,instance_id,ok,result} or {ok:false,error:{code,message}}.
 * Existing untagged status broadcasts remain compatible with the original IPC client.
 * Methods:
 * session.info -> {instance_id,version,capabilities:string[]}
 * player.action {action:"play_pause"|"previous"|"next"|"stop"|"seek"|"volume"|"repeat"|"shuffle"|"play",value?,uri?}
 * queue.list {offset?,limit?} -> Page (row.id is an entry identity, meta.index/current; revision)
 * queue.action {action:"play"|"remove"|"move"|"clear"|"append"|"play_next"|"save",entry_id?,revision?,to?,uri?,name?}
 * library.list {kind:"tracks"|"albums"|"artists"|"playlists"|"shows"|"browse",offset?,limit?,filter?,sort?} -> Page
 * library.detail {kind,id,uri?,offset?,limit?} -> Page
 * library.action {action:"save"|"unsave"|"refresh",kind?,id?,uri?}
 * search {query,kind?,offset?,limit?} -> Page
 * playlist.action {action:"create"|"rename"|"add"|"remove"|"delete",id?,name?,uri?,position?}
 * radio.status -> {active,waiting,discovery,played_count,cache_tracks,...}
 * radio.action {action:"start"|"stop"|"discovery",value?,uri?}
 * radio.debug {limit?,rng_seed?} -> structured local diagnostic report
 * settings.get -> {values:Record<string,unknown>,bindings:Record<string,string>}
 * settings.action {action:"reload"|"reconnect"|"logout"|"command",command?}
 * cast.list -> Page; cast.action {action:"connect"|"disconnect",id?}
 * share {uri} -> {url}; endpoints never return credentials.
 */
