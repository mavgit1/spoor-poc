// Finding (and if needed starting) the local service. Used by the CLI and by
// the native messaging host.

import { spawn } from "node:child_process";
import { mkdirSync, openSync } from "node:fs";
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

export async function api<T = any>(method: string, path: string, body?: unknown, opts: { start?: boolean } = {}): Promise<T> {
  const info = opts.start === false ? await runningService() : await ensureService();
  if (!info) throw new Error("spoor serve is not running");
  const r = await fetch(info.url + path, {
    method,
    headers: { authorization: `Bearer ${info.token}`, "content-type": "application/json" },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  const ct = r.headers.get("content-type") ?? "";
  if (!ct.includes("application/json")) {
    if (!r.ok) throw new ApiError(r.status, await r.text());
    return (await r.text()) as T;
  }
  const data = await r.json();
  if (!r.ok) throw new ApiError(r.status, (data as { error?: string }).error ?? r.statusText);
  return data as T;
}
