// Finding (and if needed starting) the local service. Used by the CLI and by
// the native messaging host.

import { spawn } from "node:child_process";
import { mkdirSync, openSync } from "node:fs";
import { request as httpRequest } from "node:http";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import { paths } from "./paths.ts";
import { readServeInfo, type ServeInfo } from "./server.ts";

const CLI_ENTRY = join(dirname(fileURLToPath(import.meta.url)), "..", "cli", "main.ts");

async function healthy(info: ServeInfo): Promise<boolean> {
  try {
    const r = await fetch(`${info.url}/health`, { headers: { authorization: `Bearer ${info.token}` }, signal: AbortSignal.timeout(2000) });
    return r.ok;
  } catch {
    return false;
  }
}

export async function runningService(): Promise<ServeInfo | null> {
  const info = readServeInfo();
  return info && (await healthy(info)) ? info : null;
}

/** Return the running service, starting `spoor serve` in the background if there is none. */
export async function ensureService(): Promise<ServeInfo> {
  const running = await runningService();
  if (running) return running;
  mkdirSync(paths.logs(), { recursive: true });
  const out = openSync(join(paths.logs(), "serve.out"), "a");
  const child = spawn(process.execPath, ["--no-warnings=ExperimentalWarning", CLI_ENTRY, "serve"], {
    detached: true,
    stdio: ["ignore", out, out],
    env: process.env,
    windowsHide: true,
  });
  child.unref();
  const deadline = Date.now() + 15_000;
  while (Date.now() < deadline) {
    await new Promise(r => setTimeout(r, 200));
    const info = await runningService();
    if (info) return info;
  }
  throw new Error(`spoor serve did not start — see ${join(paths.logs(), "serve.out")}`);
}

export class ApiError extends Error {
  status: number;
  constructor(status: number, message: string) {
    super(message);
    this.status = status;
  }
}

/**
 * One request to the local service. Uses node:http rather than fetch:
 * fetch (undici) gives up on a response after 5 minutes without headers,
 * and some calls here legitimately wait longer (a login, a run with wait).
 */
function request(url: string, method: string, token: string, body?: unknown): Promise<{ status: number; contentType: string; text: string }> {
  return new Promise((resolve, reject) => {
    const payload = body === undefined ? undefined : JSON.stringify(body);
    const req = httpRequest(url, {
      method,
      headers: { authorization: `Bearer ${token}`, "content-type": "application/json", ...(payload ? { "content-length": Buffer.byteLength(payload) } : {}) },
    }, res => {
      const chunks: Buffer[] = [];
      res.on("data", c => chunks.push(c));
      res.on("end", () => resolve({ status: res.statusCode ?? 0, contentType: String(res.headers["content-type"] ?? ""), text: Buffer.concat(chunks).toString("utf8") }));
      res.on("error", reject);
    });
    req.on("error", reject);
    if (payload) req.write(payload);
    req.end();
  });
}

export async function api<T = any>(method: string, path: string, body?: unknown, opts: { start?: boolean } = {}): Promise<T> {
  const info = opts.start === false ? await runningService() : await ensureService();
  if (!info) throw new Error("spoor serve is not running");
  const r = await request(info.url + path, method, info.token, body);
  if (!r.contentType.includes("application/json")) {
    if (r.status >= 400) throw new ApiError(r.status, r.text);
    return r.text as T;
  }
  const data = JSON.parse(r.text);
  if (r.status >= 400) throw new ApiError(r.status, (data as { error?: string }).error ?? `HTTP ${r.status}`);
  return data as T;
}
