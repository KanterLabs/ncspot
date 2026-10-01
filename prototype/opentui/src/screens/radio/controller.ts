import type { ParsedStatus } from "../../status.js";
import type { Params, Row, RpcApi } from "../../workspace/contracts.js";
import { diagnosticRows, discoveryLevel, radioLines, record, type RecordValue } from "./model.js";

type RefreshOutcome = "confirmed" | "stale" | "failed" | "disposed";

export interface RadioView {
  lines(lines: string[]): void;
  rows(rows: Row[]): void;
  message(message: string): void;
  /** Notify a parent that pending or acknowledged controller state changed. */
  changed?(): void;
}

export class RadioController {
  status: RecordValue = {};
  /** Latest slider intent while a discovery write or its confirming refresh is pending. */
  desiredDiscovery: number | undefined;
  private diagnostic: RecordValue | undefined;
  private pending: Promise<RefreshOutcome> | undefined;
  private pendingRefreshGeneration = 0;
  private pendingExpectedDiscovery: number | undefined;
  private discoveryTask: Promise<void> | undefined;
  private discoveryGeneration = 0;
  private discoveryInFlight = false;
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

  get busy(): boolean {
    return !this.disposed && (this.pending !== undefined || this.actionPending || this.debugPending || this.discoveryTask !== undefined || this.discoveryInFlight);
  }

  private draw(): void {
    if (this.disposed) return;
    this.view.lines(radioLines(this.status, this.diagnostic));
    this.view.rows(this.diagnostic ? diagnosticRows(this.diagnostic) : []);
    this.view.changed?.();
  }

  refresh(force = false, expectedDiscoveryGeneration?: number): Promise<void> {
    return this.refreshStatus(force, expectedDiscoveryGeneration).then(() => undefined);
  }

  private refreshStatus(force = false, expectedDiscoveryGeneration?: number, expectedDiscovery?: number): Promise<RefreshOutcome> {
    if (this.disposed) return Promise.resolve("disposed");
    // Heartbeats while a discovery intent is pending must not replace the
    // slider's desired value with an older backend snapshot. The final write
    // below uses force=true to request one confirming status refresh.
    if (!force && (this.discoveryTask || this.discoveryInFlight || this.desiredDiscovery !== undefined)) return Promise.resolve("stale");
    if (this.pending) {
      // A refresh that started before a discovery write is stale for the
      // confirming read. Wait for it to settle, then issue the post-write read.
      const generationChanged = this.pendingRefreshGeneration !== this.discoveryGeneration;
      const expectedChanged = expectedDiscovery !== undefined && this.pendingExpectedDiscovery !== expectedDiscovery;
      if (force && (generationChanged || expectedChanged)) {
        const prior = this.pending;
        return prior.then(() => this.refreshStatus(true, expectedDiscoveryGeneration, expectedDiscovery));
      }
      return this.pending;
    }

    const generation = this.discoveryGeneration;
    this.pendingRefreshGeneration = generation;
    this.pendingExpectedDiscovery = expectedDiscovery;
    this.lastRefresh = Date.now();
    const pending = this.api.call("radio.status").then(result => {
      if (this.disposed) return "disposed" as const;
      // Any write that began after this read started makes this response stale.
      // Preserve the last confirmed status until the serialized write performs
      // its own confirming refresh.
      if (generation !== this.discoveryGeneration || expectedDiscoveryGeneration !== undefined && expectedDiscoveryGeneration !== generation) return "stale" as const;
      const next = record(result);
      if (expectedDiscovery !== undefined && discoveryLevel(next.discovery) !== expectedDiscovery) {
        if (!this.disposed) {
          const actual = discoveryLevel(next.discovery);
          this.view.message(`Discovery status confirmation failed: backend reported ${actual === undefined ? "unavailable" : `${actual}%`}.`);
        }
        return "failed" as const;
      }
      this.status = next;
      this.draw();
      return "confirmed" as const;
    }).catch(error => {
      if (!this.disposed) this.view.message(expectedDiscovery === undefined ? `Radio status unavailable: ${errorText(error)}` : `Discovery status confirmation failed: ${errorText(error)}`);
      return "failed" as const;
    }).finally(() => {
      if (this.pending === pending) this.pending = undefined;
    });
    this.pending = pending;
    return pending;
  }

  async action(action: "start" | "stop" | "discovery", value?: number, uri?: string): Promise<void> {
    if (this.disposed) return;
    if (action === "discovery") {
      if (value === undefined) {
        this.view.message("Discovery value is required.");
        return;
      }
      await this.setDiscovery(value);
      return;
    }
    if (this.disposed || this.actionPending) return;
    this.actionPending = true;
    this.view.changed?.();
    const params: Params = { action };
    if (uri) params.uri = uri;
    try {
      await this.api.call("radio.action", params);
      if (this.disposed) return;
      this.diagnostic = undefined;
      this.view.message(`Radio ${action === "start" ? "started" : "stopped"}`);
      await this.refresh(true);
    } catch (error) {
      if (!this.disposed) this.view.message(`Radio action failed: ${errorText(error)}`);
    } finally {
      this.actionPending = false;
      if (!this.disposed) this.view.changed?.();
    }
  }

  setDiscovery(value: number): Promise<void> {
    if (this.disposed) return Promise.resolve();
    const desired = discoveryLevel(value);
    if (desired === undefined) {
      this.view.message("Discovery value must be a finite number.");
      return Promise.resolve();
    }
    if (this.discoveryTask && this.desiredDiscovery === desired) return this.discoveryTask;
    this.desiredDiscovery = desired;
    this.discoveryGeneration++;
    this.view.changed?.();
    if (!this.discoveryTask) {
      this.discoveryTask = this.runDiscovery().finally(() => {
        this.discoveryTask = undefined;
        this.discoveryInFlight = false;
        if (!this.disposed) this.view.changed?.();
      });
    }
    return this.discoveryTask;
  }

  private async runDiscovery(): Promise<void> {
    while (!this.disposed && this.desiredDiscovery !== undefined) {
      const value = this.desiredDiscovery;
      const generation = this.discoveryGeneration;
      this.discoveryInFlight = true;
      this.view.changed?.();
      try {
        await this.api.call("radio.action", { action: "discovery", value });
      } catch (error) {
        if (!this.disposed) this.view.message(`Radio action failed: ${errorText(error)}`);
        // A failed write invalidates every queued intent. Keep status.discovery
        // untouched: it remains the last value confirmed by the backend.
        this.desiredDiscovery = undefined;
        if (!this.disposed) this.view.changed?.();
        return;
      }
      if (this.disposed) return;
      this.discoveryInFlight = false;
      // More slider input arrived while this value was in flight. Coalesce it
      // into one latest write and defer the status refresh until that settles.
      if (generation !== this.discoveryGeneration) continue;

      const outcome = await this.refreshStatus(true, generation, value);
      if (this.disposed) return;
      // A new intent may have arrived while radio.status was in flight. Keep it
      // pending and serialize the latest value after the confirming read.
      if (generation !== this.discoveryGeneration) continue;
      if (outcome !== "confirmed") {
        this.desiredDiscovery = undefined;
        // Let the next heartbeat (or an explicit refresh) retry after a failed
        // confirmation instead of leaving a stale signature suppressing it.
        this.lastSignature = "";
        this.view.changed?.();
        return;
      }
      this.diagnostic = undefined;
      this.view.message(`Discovery set to ${value}%`);
      this.desiredDiscovery = undefined;
      this.view.changed?.();
    }
  }

  adjust(amount: number): Promise<void> {
    const current = this.desiredDiscovery ?? discoveryLevel(this.status.discovery);
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
    this.desiredDiscovery = undefined;
    this.discoveryGeneration++;
    this.unsubscribe();
  }
}

const errorText = (error: unknown): string => error instanceof Error ? error.message : String(error);
