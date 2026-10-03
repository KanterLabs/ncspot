import { parseStatus, type ParsedStatus } from "./status.js";

export interface IpcClientEvents {
  onStatus: (status: ParsedStatus) => void;
  onMalformedLine?: (line: string) => void;
  onOpen?: () => void;
  onClose?: (error?: Error) => void;
}

type BunUnixSocket = Bun.Socket<unknown>;

/**
 * One-shot newline JSON client for Resonance's Unix IPC socket.
 *
 * It intentionally never reconnects. A path can be reused by a new Resonance
 * process after a disconnect, and silently reconnecting would make the
 * prototype control the wrong instance.
 */
export class IpcClient {
  readonly path: string;
  private readonly events: IpcClientEvents;
  private socket: BunUnixSocket | null = null;
  private decoder = new TextDecoder();
  private receiveBuffer = "";
  private closed = false;
  private opened = false;
  private closeNotified = false;
  private writes: Uint8Array[] = [];
  private writeOffset = 0;
  private queuedBytes = 0;

  constructor(path: string, events: IpcClientEvents) {
    this.path = path;
    this.events = events;
  }

  get isOpen(): boolean {
    return this.opened && !this.closed && this.socket !== null && this.socket.readyState > 0;
  }

  async connect(): Promise<void> {
    if (this.closed) throw new Error("IPC client is already closed");
    if (this.socket) throw new Error("IPC client is already connected");

    try {
      const socket = await Bun.connect({
        unix: this.path,
        socket: {
          open: (openedSocket) => {
            this.socket = openedSocket;
            this.opened = true;
            this.events.onOpen?.();
          },
          data: (_openedSocket, data) => {
            this.receiveBuffer += this.decoder.decode(data, { stream: true });
            this.drainLines();
            if (this.receiveBuffer.length > 8 * 1024 * 1024) this.fail(new Error("Engine frame exceeds the IPC limit"));
          },
          drain: () => { this.flushWrites(); },
          close: (_openedSocket, error) => {
            this.opened = false;
            this.socket = null;
            this.notifyClose(error);
          },
          error: (_openedSocket, error) => {
            // Bun may deliver an error before `close`; expose it immediately
            // while allowing the close callback to be idempotent.
            this.opened = false;
            this.socket = null;
            this.notifyClose(error);
          },
        },
      });
      // `open` normally assigned this first. Keep the resolved handle as a
      // fallback for Bun versions that resolve just before invoking `open`.
      if (!this.opened) {
        this.socket = socket;
        this.opened = true;
        this.events.onOpen?.();
      }
    } catch (error) {
      const normalized = error instanceof Error ? error : new Error(String(error));
      this.notifyClose(normalized);
      throw normalized;
    }
  }

  /** Write exactly one Resonance command line. Returns false when disconnected. */
  send(command: string): boolean {
    if (!this.isOpen || !command.trim()) return false;
    const data = new TextEncoder().encode(`${command.trim()}\n`);
    if (data.length > 1024 * 1024 || this.queuedBytes + data.length > 2 * 1024 * 1024) return false;
    this.writes.push(data); this.queuedBytes += data.length;
    return this.flushWrites();
  }

  private flushWrites(): boolean {
    try {
      while (this.socket && this.writes.length) {
        const head = this.writes[0]!;
        const written = this.socket.write(head, this.writeOffset, head.length - this.writeOffset);
        if (written < 0) { this.fail(new Error("Engine socket closed while sending")); return false; }
        if (written === 0) break;
        this.writeOffset += written; this.queuedBytes -= written;
        if (this.writeOffset < head.length) break;
        this.writes.shift(); this.writeOffset = 0;
      }
      return !this.closed;
    } catch (error) { this.fail(error instanceof Error ? error : new Error(String(error))); return false; }
  }

  private fail(error: Error): void { this.notifyClose(error); this.close(); }

  close(): void {
    if (this.closed) return;
    this.closed = true;
    this.opened = false;
    const socket = this.socket;
    this.socket = null;
    this.writes = []; this.writeOffset = 0; this.queuedBytes = 0;
    socket?.close();
    this.notifyClose();
  }

  private drainLines(): void {
    let newlineIndex = this.receiveBuffer.indexOf("\n");
    while (newlineIndex >= 0) {
      const line = this.receiveBuffer.slice(0, newlineIndex).replace(/\r$/, "");
      this.receiveBuffer = this.receiveBuffer.slice(newlineIndex + 1);
      if (line.length > 8 * 1024 * 1024) { this.fail(new Error("Engine frame exceeds the IPC limit")); return; }
      if (line.trim()) {
        const status = parseStatus(line);
        if (status) this.events.onStatus(status);
        else this.events.onMalformedLine?.(line);
      }
      newlineIndex = this.receiveBuffer.indexOf("\n");
    }
  }

  private notifyClose(error?: Error): void {
    if (this.closeNotified) return;
    this.closeNotified = true;
    this.events.onClose?.(error);
  }
}

/** A transport-shaped sink used by the demo and by the view tests. */
export interface CommandTransport {
  send(command: string): boolean;
  close(): void;
}

export class DemoTransport implements CommandTransport {
  readonly commands: string[] = [];

  send(command: string): boolean {
    this.commands.push(command);
    return true;
  }

  close(): void {}
}
