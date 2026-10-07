// A symmetric call/reply/event peer over any message transport. Used by the
// service (over the bridge WebSocket) and by the extension's background
// script (over the native messaging port). No platform APIs here.

import type { BridgeMessage } from "./types.ts";

export type Handler = (params: any) => unknown | Promise<unknown>;

export class RpcPeer {
  private nextId = 1;
  private pending = new Map<number, { resolve: (v: any) => void; reject: (e: Error) => void }>();
  private send: (msg: BridgeMessage) => void;
  private handlers: Record<string, Handler>;
  private onEvent: (name: string, data: any) => void;

  constructor(
    send: (msg: BridgeMessage) => void,
    handlers: Record<string, Handler>,
    onEvent: (name: string, data: any) => void,
  ) {
    this.send = send;
    this.handlers = handlers;
    this.onEvent = onEvent;
  }

  call<T = any>(method: string, params?: unknown, timeoutMs = 60_000): Promise<T> {
    const id = this.nextId++;
    return new Promise<T>((resolve, reject) => {
      const timer = setTimeout(() => {
        this.pending.delete(id);
        reject(new Error(`${method}: no reply after ${timeoutMs} ms`));
      }, timeoutMs);
      this.pending.set(id, {
        resolve: v => { clearTimeout(timer); resolve(v); },
        reject: e => { clearTimeout(timer); reject(e); },
      });
      try {
        this.send({ t: "call", id, method, params });
      } catch (e) {
        this.pending.delete(id);
        clearTimeout(timer);
        reject(e instanceof Error ? e : new Error(String(e)));
      }
    });
  }

  emit(name: string, data?: unknown): void {
    this.send({ t: "event", name, data });
  }

  /** Fail every outstanding call, e.g. when the transport closes. */
  close(reason: string): void {
    for (const p of this.pending.values()) p.reject(new Error(reason));
    this.pending.clear();
  }

  async receive(msg: BridgeMessage): Promise<void> {
    if (msg.t === "reply") {
      const p = this.pending.get(msg.id);
      if (!p) return;
      this.pending.delete(msg.id);
      if (msg.ok) p.resolve(msg.result);
      else p.reject(new Error(msg.error));
    } else if (msg.t === "event") {
      this.onEvent(msg.name, msg.data);
    } else if (msg.t === "call") {
      const handler = this.handlers[msg.method];
      let reply: BridgeMessage;
      if (!handler) {
        reply = { t: "reply", id: msg.id, ok: false, error: `unknown method ${msg.method}` };
      } else {
        try {
          reply = { t: "reply", id: msg.id, ok: true, result: await handler(msg.params) };
        } catch (e) {
          reply = { t: "reply", id: msg.id, ok: false, error: e instanceof Error ? e.message : String(e) };
        }
      }
      try {
        this.send(reply);
      } catch {
        // transport gone; the caller's timeout reports it
      }
    }
  }
}

// Native messaging caps host → extension messages at 1 MB. Larger messages are
// split into chunks by the host and reassembled by the extension.
// Chunks are measured in UTF-16 units; 256K units is at most 768 KB of UTF-8.
export const CHUNK_CHARS = 256 * 1024;

export interface Chunk {
  chunk: string;
  i: number;
  n: number;
  data: string;
}

export function splitMessage(json: string, id: string): Chunk[] | null {
  if (json.length <= CHUNK_CHARS) return null;
  const n = Math.ceil(json.length / CHUNK_CHARS);
  const out: Chunk[] = [];
  for (let i = 0; i < n; i++) out.push({ chunk: id, i, n, data: json.slice(i * CHUNK_CHARS, (i + 1) * CHUNK_CHARS) });
  return out;
}

export class ChunkAssembler {
  private parts = new Map<string, string[]>();

  /** Returns the full message once the last chunk arrives, else null. */
  add(c: Chunk): BridgeMessage | null {
    let parts = this.parts.get(c.chunk);
    if (!parts) {
      parts = new Array(c.n);
      this.parts.set(c.chunk, parts);
    }
    parts[c.i] = c.data;
    for (let i = 0; i < c.n; i++) if (parts[i] === undefined) return null;
    this.parts.delete(c.chunk);
    return JSON.parse(parts.join("")) as BridgeMessage;
  }
}

export function isChunk(m: unknown): m is Chunk {
  return typeof m === "object" && m !== null && "chunk" in m && "data" in m;
}
