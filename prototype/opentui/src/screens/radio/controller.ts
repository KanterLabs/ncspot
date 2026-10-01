import type { ParsedStatus } from "../../status.js";
import type { Params, Row, RpcApi } from "../../workspace/contracts.js";
import { diagnosticRows, discoveryLevel, radioLines, record, type RecordValue } from "./model.js";

export interface RadioView {
  lines(lines: string[]): void;
  rows(rows: Row[]): void;
  message(message: string): void;
}

export class RadioController {
  status: RecordValue = {};
  private diagnostic: RecordValue | undefined;
  private pending: Promise<void> | undefined;
  private debugPending = false;
  private actionPending = false;
  private disposed = false;
  private lastSignature = "";
  private lastRefresh = 0;
  private unsubscribe: () => void;

  constructor(private api: RpcApi, private view: RadioView, subscribe: (listener: (status: ParsedStatus) => void) => () => void) {
    this.unsubscribe = subscribe(status => {
      const p = status.prototype;
      const signature = JSON.stringify([p?.radio_active, p?.radio_waiting, p?.discovery, status.playable?.uri]);
      if (signature !== this.lastSignature || Date.now() - this.lastRefresh >= 2000) {
        this.lastSignature = signature;
        void this.refresh();
      }
    });
    this.draw();
  }

  private draw(): void {
    if (this.disposed) return;
    this.view.lines(radioLines(this.status, this.diagnostic));
    this.view.rows(this.diagnostic ? diagnosticRows(this.diagnostic) : []);
  }

  refresh(): Promise<void> {
    if (this.disposed) return Promise.resolve();
    if (this.pending) return this.pending;
    this.lastRefresh = Date.now();
    this.pending = this.api.call("radio.status").then(result => {
      if (this.disposed) return;
      this.status = record(result);
      this.draw();
    }).catch(error => {
      if (!this.disposed) this.view.message(`Radio status unavailable: ${errorText(error)}`);
    }).finally(() => { this.pending = undefined; });
    return this.pending;
  }

  async action(action: "start" | "stop" | "discovery", value?: number, uri?: string): Promise<void> {
    if (this.disposed || this.actionPending) return;
    this.actionPending = true;
    const params: Params = { action };
    if (value !== undefined) params.value = discoveryLevel(value);
    if (uri) params.uri = uri;
    try {
      await this.api.call("radio.action", params);
      if (this.disposed) return;
      this.diagnostic = undefined;
      this.view.message(action === "discovery" ? `Discovery set to ${params.value}%` : `Radio ${action === "start" ? "started" : "stopped"}`);
      await this.refresh();
    } catch (error) {
      if (!this.disposed) this.view.message(`Radio action failed: ${errorText(error)}`);
    } finally { this.actionPending = false; }
  }

  adjust(amount: number): Promise<void> {
    const current = discoveryLevel(this.status.discovery);
    if (current === undefined) {
      this.view.message("Discovery unavailable; refresh status first.");
      return Promise.resolve();
    }
    return this.action("discovery", current + amount);
  }

  async debug(seed: string): Promise<void> {
    if (this.disposed || this.debugPending) return;
    if (!/^\d+$/.test(seed.trim()) || !Number.isSafeInteger(Number(seed))) {
      this.view.message("RNG seed must be a nonnegative safe integer.");
      return;
    }
    this.debugPending = true;
    this.view.message("Loading deterministic cache-only diagnostics…");
    try {
      const result = await this.api.call("radio.debug", { limit: 20, rng_seed: Number(seed) });
      if (this.disposed) return;
      this.diagnostic = record(result);
      this.draw();
      this.view.message("Diagnostic snapshot · select a candidate for score components · d to rerun");
    } catch (error) {
      if (!this.disposed) this.view.message(`Radio diagnostics unavailable: ${errorText(error)}`);
    } finally { this.debugPending = false; }
  }

  dispose(): void {
    if (this.disposed) return;
    this.disposed = true;
    this.unsubscribe();
  }
}

const errorText = (error: unknown): string => error instanceof Error ? error.message : String(error);
