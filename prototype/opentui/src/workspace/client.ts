import { IpcClient, type IpcClientEvents } from "../ipc.js";
import { RPC_VERSION, type Params, type RpcApi } from "./contracts.js";

export class RpcError extends Error {
  constructor(public readonly code: string, message: string) { super(message); this.name = "RpcError"; }
}
export class WorkspaceClient implements RpcApi {
  private client: IpcClient;
  private nextId = 0;
  private instance?: string;
  private pending = new Map<string, { resolve(value: unknown): void; reject(error: Error): void; timer: ReturnType<typeof setTimeout> }>();
  constructor(path: string, events: IpcClientEvents, private timeoutMs = 30_000) {
    this.client = new IpcClient(path, {
      ...events,
      onMalformedLine: (line) => {
        if (!this.receive(line)) events.onMalformedLine?.(line);
      },
      onClose: (error) => { this.rejectPending(new RpcError("disconnected", error?.message ?? "Engine disconnected")); events.onClose?.(error); },
    });
  }
  async connect(): Promise<void> {
    await this.client.connect();
    const session = await this.call<{ instance_id: string }>("session.info");
    if (!session.instance_id || session.instance_id !== this.instance) throw new RpcError("invalid_session", "Engine identity handshake failed");
  }
  call<T = unknown>(method: string, params: Params = {}): Promise<T> {
    if (!this.client.isOpen) return Promise.reject(new RpcError("disconnected", "Engine is disconnected"));
    const id = String(++this.nextId);
    return new Promise<T>((resolve, reject) => {
      const timer = setTimeout(() => { this.pending.delete(id); reject(new RpcError("timeout", `${method} timed out; its result is unknown. Refresh before retrying a mutation.`)); }, this.timeoutMs);
      this.pending.set(id, { resolve: (value) => resolve(value as T), reject, timer });
      if (!this.client.send(JSON.stringify({ protocol: "resonance", version: RPC_VERSION, id, method, params }))) {
        clearTimeout(timer); this.pending.delete(id); reject(new RpcError("disconnected", "Unable to send command to engine"));
      }
    });
  }
  private receive(line: string): boolean {
    let response: any;
    try { response = JSON.parse(line); } catch { return false; }
    if (response?.protocol !== "resonance") return false;
    const entry = this.pending.get(String(response.id));
    if (!entry) return true;
    this.pending.delete(String(response.id)); clearTimeout(entry.timer);
    if (response.version !== RPC_VERSION || typeof response.instance_id !== "string") { entry.reject(new RpcError("invalid_response", "Invalid engine response")); return true; }
    if (this.instance && this.instance !== response.instance_id) {
      entry.reject(new RpcError("instance_changed", "Engine identity changed; reopen Resonance"));
      this.client.close(); return true;
    }
    this.instance = response.instance_id;
    if (response.ok === true) entry.resolve(response.result);
    else entry.reject(new RpcError(response.error?.code ?? "engine_error", response.error?.message ?? "Engine rejected the request"));
    return true;
  }
  private rejectPending(error: Error): void {
    for (const entry of this.pending.values()) { clearTimeout(entry.timer); entry.reject(error); }
    this.pending.clear();
  }
  close(): void { this.rejectPending(new RpcError("disconnected", "Frontend closed")); this.client.close(); }
}
