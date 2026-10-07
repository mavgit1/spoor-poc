// Native messaging host. Firefox starts this program when the extension calls
// runtime.connectNative("spoor_bridge") and talks to it over stdio:
// each message is a 4-byte little-endian length followed by UTF-8 JSON.
//
// The host is only a relay: it connects to the local service's /bridge
// WebSocket (starting the service if needed) and pipes messages both ways.
// Host → extension messages are capped at 1 MB by Firefox, so large ones are
// split into chunks the extension reassembles.
//
// Nothing may be written to stdout except framed messages.

import { appendFileSync, mkdirSync } from "node:fs";
import { join } from "node:path";
import WebSocket from "ws";

import { splitMessage } from "../shared/rpc.ts";
import { ensureService } from "../service/client.ts";
import { nowIso, paths } from "../service/paths.ts";

function log(msg: string): void {
  try {
    mkdirSync(paths.logs(), { recursive: true });
    appendFileSync(join(paths.logs(), "host.log"), `${nowIso()} [${process.pid}] ${msg}\n`);
  } catch {}
}

function writeMessage(json: string): void {
  const body = Buffer.from(json, "utf8");
  const header = Buffer.alloc(4);
  header.writeUInt32LE(body.length, 0);
  process.stdout.write(Buffer.concat([header, body]));
}

let chunkSeq = 0;
function toExtension(json: string): void {
  const chunks = splitMessage(json, `${process.pid}-${++chunkSeq}`);
  if (!chunks) return writeMessage(json);
  for (const c of chunks) writeMessage(JSON.stringify(c));
}

/** Parse framed messages from stdin; calls `onMessage` with each JSON string. */
export function frameReader(onMessage: (json: string) => void): (chunk: Buffer) => void {
  let buf = Buffer.alloc(0);
  return chunk => {
    buf = Buffer.concat([buf, chunk]);
    while (buf.length >= 4) {
      const len = buf.readUInt32LE(0);
      if (buf.length < 4 + len) break;
      onMessage(buf.subarray(4, 4 + len).toString("utf8"));
      buf = buf.subarray(4 + len);
    }
  };
}

async function main(): Promise<void> {
  log(`start argv=${JSON.stringify(process.argv.slice(2))}`);
  const pending: string[] = [];
  let ws: WebSocket | null = null;

  process.stdin.on("data", frameReader(json => {
    if (ws?.readyState === WebSocket.OPEN) ws.send(json);
    else pending.push(json);
  }));
  process.stdin.on("end", () => {
    log("stdin closed (Firefox went away)");
    process.exit(0);
  });

  let info;
  try {
    info = await ensureService();
  } catch (e) {
    log(`cannot start service: ${(e as Error).message}`);
    toExtension(JSON.stringify({ t: "event", name: "host.error", data: { error: (e as Error).message } }));
    process.exit(1);
  }
  const url = info.url.replace(/^http/, "ws") + "/bridge";
  ws = new WebSocket(url, { headers: { authorization: `Bearer ${info.token}` }, maxPayload: 256 * 1024 * 1024 });
  ws.on("open", () => {
    log(`connected to ${url}`);
    for (const m of pending.splice(0)) ws!.send(m);
  });
  ws.on("message", data => toExtension(String(data)));
  // When the service goes away the extension reconnects, which starts a new
  // host, which starts the service again.
  ws.on("close", (code, reason) => {
    log(`service closed the bridge (${code} ${reason})`);
    process.exit(0);
  });
  ws.on("error", e => {
    log(`bridge error: ${e.message}`);
    process.exit(1);
  });
}

if (import.meta.main) void main();
