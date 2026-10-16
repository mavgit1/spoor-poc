// `spoor serve`: the local service.
//
// - HTTP API on 127.0.0.1 for the agent and the CLI. Every request needs
//   `Authorization: Bearer <token>`; url and token are in serve.json.
// - /bridge: a WebSocket the native messaging host connects to. That is the
//   extension's only line to the service; the home panel and sidebar call the
//   same API through it (`api` calls), so the human and the agent see the
//   same data.
// - Spoor's Firefox starts on demand: anything that needs the browser (a run,
//   a login, a recording) launches it in the background if it isn't up.
// - Sessions: the check script's verdict is kept per site (session.json);
//   keepalive re-checks on a timer and asks the human to log in when it fails.

import { randomBytes, timingSafeEqual } from "node:crypto";
import { appendFileSync, createReadStream, existsSync, mkdirSync, readdirSync, readFileSync, rmSync, statSync } from "node:fs";
import { createServer, type IncomingMessage, type ServerResponse } from "node:http";
import { join } from "node:path";
import { WebSocketServer, type WebSocket } from "ws";

import { RpcPeer } from "../shared/rpc.ts";
import { launch } from "../cli/firefox.ts";
import type { BridgeMessage, CookieInfo, Flow, RecordingMeta, RunDone, RunEventInput, SiteBrowserState, StorageSnapshot } from "../shared/types.ts";
import { inputSchema, validateArgs } from "./actions.ts";
import { buildDossier } from "./dossier.ts";
import { authChain, endpoints, flowLine, lifetimes, matchingFlows, trace } from "./inspect.ts";
import { DEFAULT_PORT, newId, nowIso, paths } from "./paths.ts";
import { isWrite, minimize, requestFromFlow, send } from "./replay.ts";
import { RunManager } from "./runs.ts";
import * as store from "./store.ts";
import { BadRequest, NotFound } from "./store.ts";

export const VERSION = "0.8.0";
const MAX_API_FILE_BYTES = 8 * 1024 * 1024;
const BROWSER_START_MS = 90_000;

/** An action needs a login the site doesn't have right now. */
export class LoginRequired extends BadRequest {
  code = "login_required";
}

export interface ServeInfo {
  url: string;
  token: string;
  pid: number;
  version: string;
}

function log(msg: string): void {
  const line = `${nowIso()} ${msg}\n`;
  try {
    mkdirSync(paths.logs(), { recursive: true });
    appendFileSync(join(paths.logs(), "service.log"), line);
  } catch {
    // logging must never take the service down
  }
  if (process.env.SPOOR_VERBOSE) process.stderr.write(line);
}

type Ctx = { params: Record<string, string>; query: URLSearchParams; body: any; fromExtension: boolean };
type Result = unknown | { __file: string; contentType: string };
type Route = [method: string, pattern: RegExp, keys: string[], handler: (ctx: Ctx) => Promise<Result> | Result];

function compile(path: string): [RegExp, string[]] {
  const keys: string[] = [];
  const re = path.replace(/:([a-z_]+)/g, (_, k) => {
    keys.push(k);
    return "([^/]+)";
  });
  return [new RegExp(`^${re}$`), keys];
}

/** Paces requests the service itself sends (replays), per site. */
class Pacer {
  private last = new Map<string, number>();
  async wait(site: string, gapMs: number): Promise<void> {
    const prev = this.last.get(site) ?? 0;
    const now = Date.now();
    const at = Math.max(now, prev + gapMs);
    this.last.set(site, at);
    if (at > now) await new Promise(r => setTimeout(r, at - now));
  }
}

export class Service {
  private routes: Route[] = [];
  private peer: RpcPeer | null = null;
  private socket: WebSocket | null = null;
  private browserInfo: Record<string, unknown> = {};
  private recordings = new Map<string, RecordingMeta>();
  private pacer = new Pacer();
  readonly runs: RunManager;
  private shutdownHook: () => void = () => {};
  private launching: Promise<void> | null = null;
  /** The service started Firefox itself: it goes to the background once connected. */
  private launchedInBackground = false;
  private connectWaiters = new Set<() => void>();
  private lastKeepalive = new Map<string, number>();
  private logins = new Set<string>();

  constructor() {
    this.runs = new RunManager(
      { call: (m, p, t) => this.ext(m, p, t), ensure: () => this.ensureBrowser() },
      site => this.changed(site),
    );
    this.defineRoutes();
    setInterval(() => void this.keepalive(), 60_000).unref();
  }

  /** Start Spoor's Firefox unless it's connected, and wait for its extension to connect. */
  ensureBrowser(): Promise<void> {
    if (this.peer) return Promise.resolve();
    this.launching ??= (async () => {
      if (process.env.SPOOR_NO_LAUNCH) throw new BadRequest("Spoor's Firefox is not running — start it with `spoor browser`");
      log("browser: starting Spoor's Firefox on demand");
      this.launchedInBackground = true;
      launch();
      const connected = new Promise<boolean>(resolve => {
        const t = setTimeout(() => resolve(false), BROWSER_START_MS);
        this.connectWaiters.add(() => {
          clearTimeout(t);
          resolve(true);
        });
      });
      if (!(await connected)) throw new Error(`Spoor's Firefox did not connect within ${BROWSER_START_MS / 1000}s — see ${paths.logs()}`);
    })().finally(() => {
      this.launching = null;
    });
    return this.launching;
  }

  onShutdown(f: () => void): void {
    this.shutdownHook = f;
  }

  // ------------------------------------------------------------ bridge

  /** A native messaging host connected (Spoor's Firefox started). */
  attach(ws: WebSocket): void {
    if (this.socket) {
      log("bridge: replacing previous connection");
      this.socket.close(4000, "replaced by a newer connection");
    }
    this.socket = ws;
    const peer = new RpcPeer(
      msg => ws.send(JSON.stringify(msg)),
      {
        api: (p: { method: string; path: string; body?: unknown }) => this.dispatch(p.method, p.path, p.body, true).then(r => r.body),
        "run.save": (p: { run_id: string; name: string; data: string; encoding: "utf8" | "base64" }) => {
          const meta = this.runs.get(p.run_id);
          if (!meta) throw new BadRequest(`run ${p.run_id} is not active`);
          const bytes = store.saveRunFile(meta.site, p.run_id, p.name, p.data, p.encoding);
          this.runs.event(p.run_id, { type: "saved", file: p.name, bytes });
          return { file: p.name, bytes, abs: join(paths.run(meta.site, p.run_id), p.name) };
        },
        // A spoor.download() finished in Firefox's download folder: move it into the run.
        "run.adopt": (p: { run_id: string; from: string; path: string; url: string }) => {
          const meta = this.runs.get(p.run_id);
          if (!meta) throw new BadRequest(`run ${p.run_id} is not active`);
          const out = store.adoptDownload(meta.site, p.run_id, p.from, p.path);
          this.runs.event(p.run_id, { type: "saved", file: out.file, bytes: out.bytes, source: p.url });
          return { path: out.file.slice("files/".length), file: out.file, bytes: out.bytes, abs: join(paths.run(meta.site, p.run_id), out.file) };
        },
        "run.state": (p: { run_id: string; op: "get" | "set" | "all"; key?: string; value?: unknown }) => {
          const meta = this.runs.get(p.run_id);
          if (!meta) throw new BadRequest(`run ${p.run_id} is not active`);
          if (p.op === "all") return store.readScriptState(meta.site, meta.script);
          if (typeof p.key !== "string" || !p.key) throw new BadRequest("state key must be a non-empty string");
          if (p.op === "get") return store.readScriptState(meta.site, meta.script)[p.key] ?? null;
          store.writeScriptState(meta.site, meta.script, p.key, p.value);
          return true;
        },
      },
      (name, data) => this.onEvent(name, data),
    );
    this.peer = peer;
    log("bridge: connected");
    ws.on("message", raw => {
      let msg: BridgeMessage;
      try {
        msg = JSON.parse(String(raw));
      } catch {
        return;
      }
      void peer.receive(msg);
    });
    ws.on("close", () => {
      if (this.peer !== peer) return;
      log("bridge: disconnected");
      this.peer = null;
      this.socket = null;
      this.browserInfo = {};
      peer.close("Spoor's Firefox disconnected");
      this.runs.failAll("Spoor's Firefox disconnected while the run was going");
      for (const [site, meta] of this.recordings) {
        meta.stopped = nowIso();
        store.writeRecordingMeta(meta);
        this.recordings.delete(site);
      }
    });
  }

  private ext<T = any>(method: string, params?: unknown, timeoutMs = 60_000): Promise<T> {
    if (!this.peer) return Promise.reject(new BadRequest("Spoor's Firefox is not running — start it with `spoor browser`"));
    return this.peer.call<T>(method, params, timeoutMs);
  }

  private changed(site?: string): void {
    this.peer?.emit("changed", { site });
  }

  private onEvent(name: string, data: any): void {
    switch (name) {
      case "hello":
        this.browserInfo = data ?? {};
        log(`bridge: hello ${JSON.stringify(data)}`);
        if (this.launchedInBackground) {
          this.launchedInBackground = false;
          this.peer?.emit("background", {});
        }
        for (const w of this.connectWaiters) w();
        this.connectWaiters.clear();
        break;
      case "run.event":
        this.runs.event(data.run_id, data.ev as RunEventInput);
        break;
      case "run.done":
        this.runs.finish(data as RunDone);
        break;
      case "audit": {
        const { site, ...rest } = data;
        if (store.loadSites()[site]) store.audit(site, rest);
        break;
      }
      case "record.flow": {
        const meta = this.recordings.get(data.site);
        if (!meta || meta.id !== data.rec_id) return;
        meta.flows++;
        store.appendFlow(data.site, data.rec_id, data.flow as Flow);
        break;
      }
      default:
        log(`bridge: unknown event ${name}`);
    }
  }

  // ------------------------------------------------------------ routing

  private route(method: string, path: string, handler: Route[3]): void {
    const [re, keys] = compile(path);
    this.routes.push([method, re, keys, handler]);
  }

  async dispatch(method: string, rawPath: string, body: unknown, fromExtension: boolean): Promise<{ status: number; body: unknown; file?: { path: string; contentType: string } }> {
    const url = new URL(rawPath, "http://x");
    for (const [m, re, keys, handler] of this.routes) {
      if (m !== method) continue;
      const match = re.exec(url.pathname);
      if (!match) continue;
      const params: Record<string, string> = {};
      keys.forEach((k, i) => (params[k] = decodeURIComponent(match[i + 1]!)));
      try {
        const out = await handler({ params, query: url.searchParams, body: body ?? {}, fromExtension });
        if (out && typeof out === "object" && "__file" in out) {
          const f = out as { __file: string; contentType: string };
          if (fromExtension) return { status: 200, body: fileForExtension(f.__file, f.contentType) };
          return { status: 200, body: null, file: { path: f.__file, contentType: f.contentType } };
        }
        return { status: 200, body: out ?? { ok: true } };
      } catch (e) {
        const msg = e instanceof Error ? e.message : String(e);
        const status = e instanceof NotFound ? 404 : e instanceof BadRequest ? 400 : 500;
        if (status === 500) log(`${method} ${rawPath}: ${e instanceof Error ? e.stack : msg}`);
        if (fromExtension) throw new Error(msg);
        const code = (e as { code?: unknown }).code;
        return { status, body: typeof code === "string" ? { error: msg, code } : { error: msg } };
      }
    }
    if (fromExtension) throw new Error(`no route ${method} ${url.pathname}`);
    return { status: 404, body: { error: `no route ${method} ${url.pathname}` } };
  }

  private defineRoutes(): void {
    const r = this.route.bind(this);
    const site = (ctx: Ctx) => store.getSite(ctx.params.site!);

    r("GET", "/health", () => ({ ok: true, version: VERSION }));
    r("GET", "/state", () => ({
      version: VERSION,
      browser: this.peer ? { connected: true, ...this.browserInfo } : { connected: false },
      active_runs: this.runs.activeRuns(),
      recordings: [...this.recordings.values()],
    }));
    r("POST", "/shutdown", () => {
      setTimeout(() => this.shutdownHook(), 50);
      return { shutting_down: true };
    });
    r("POST", "/stop", async () => {
      if (this.peer) await this.ext("stop", {});
      return { stopped: true };
    });

    // sites
    r("GET", "/sites", () => store.listSites());
    r("POST", "/sites", ctx => {
      const { name, url, check, min_gap_ms, keepalive_mins } = ctx.body;
      const s = store.addSite(String(name ?? ""), { url, check, min_gap_ms, keepalive_mins });
      this.changed(s.name);
      return s;
    });
    r("GET", "/sites/:site", async ctx => {
      const s = site(ctx);
      const browser = this.peer ? await this.ext<SiteBrowserState>("status", { site: s.name }).then(b => this.withCheck(b)).catch(() => null) : null;
      let dossier: string[] = [];
      try {
        dossier = readdirSync(paths.dossier(s.name));
      } catch {}
      return {
        site: s,
        dir: paths.site(s.name),
        session: store.readSession(s.name),
        notes: store.readNotes(s.name),
        scripts: store.listScripts(s.name),
        runs: store.listRuns(s.name, 20),
        recordings: store.listRecordings(s.name).slice(0, 20).map(x => x.meta),
        dossier,
        browser,
      };
    });
    r("DELETE", "/sites/:site", ctx => {
      const removed = store.removeSite(ctx.params.site!);
      this.changed(ctx.params.site);
      return { removed };
    });
    r("GET", "/sites/:site/notes", ctx => ({ text: store.readNotes(site(ctx).name) }));
    r("PUT", "/sites/:site/notes", ctx => {
      store.writeNotes(site(ctx).name, String(ctx.body.text ?? ""));
      this.changed(ctx.params.site);
      return { saved: true };
    });
    r("GET", "/sites/:site/status", async ctx => this.status(site(ctx).name));

    // sessions: deliberate, audited actions on the site's container
    r("GET", "/sites/:site/session", async ctx => {
      const s = site(ctx);
      const live = ctx.query.get("check") !== "0" && this.peer ? await this.status(s.name) : null;
      const cookies = this.peer ? await this.ext<CookieInfo[]>("cookies", { site: s.name }).catch(() => null) : null;
      return {
        site: s.name,
        browser_running: !!this.peer,
        ...store.readSession(s.name),
        check: s.check,
        check_error: live?.check_error,
        keepalive_mins: s.keepalive_mins,
        cookies: cookies && cookieSummary(cookies),
      };
    });
    r("POST", "/sites/:site/login", async ctx => this.login(site(ctx).name, Number(ctx.body.timeout_secs ?? 600), ctx.body.wait !== false));
    r("POST", "/sites/:site/session/clear", async ctx => {
      const s = site(ctx);
      await this.ensureBrowser();
      if (this.runs.activeRuns().some(m => m.site === s.name)) await this.ext("stop", { site: s.name });
      const out = await this.ext<{ cookies_removed: number; cache: boolean }>("session.clear", { site: s.name, cache: !!ctx.body.cache });
      store.writeSession(s.name, { state: "logged_out" });
      store.audit(s.name, { event: "session.clear", cache: !!ctx.body.cache, cookies_removed: out.cookies_removed });
      log(`session: cleared ${s.name} (${out.cookies_removed} cookies${out.cache ? ", cache" : ""})`);
      this.changed(s.name);
      return { site: s.name, cleared: true, ...out };
    });
    r("GET", "/sites/:site/session/cookies", async ctx => {
      const s = site(ctx);
      await this.ensureBrowser();
      const cookies = await this.ext<CookieInfo[]>("cookies", { site: s.name });
      store.audit(s.name, { event: "session.export", cookies: cookies.length });
      return cookies;
    });

    // actions: scripts with a spoor-action block, called by name with validated args
    r("GET", "/actions", () => store.listSites().flatMap(s => this.actions(s.name)));
    r("GET", "/sites/:site/actions", ctx => this.actions(site(ctx).name));
    r("POST", "/sites/:site/actions/:name", async ctx => {
      const s = site(ctx);
      const info = store.listScripts(s.name).find(x => x.name === ctx.params.name);
      if (!info) throw new NotFound(`no action ${ctx.params.name} for site ${s.name}`);
      if (info.action_error) throw new BadRequest(`${info.name}: ${info.action_error}`);
      if (!info.action) throw new BadRequest(`${info.name} is a script, not an action (no spoor-action block) — run it with POST /sites/${s.name}/runs`);
      const v = validateArgs(info.action, ctx.body.args);
      if ("errors" in v) throw new BadRequest(`invalid arguments for ${s.name}/${info.name}: ${v.errors.join("; ")}`);
      if (info.action.login !== false && s.check) {
        await this.ensureBrowser();
        let st: { logged_in?: boolean; check_error?: string } = await this.status(s.name);
        if (!st.logged_in && ctx.body.login === "wait") st = await this.login(s.name, Number(ctx.body.login_timeout_secs ?? 600), true);
        if (!st.logged_in) {
          if (ctx.body.login !== "wait") this.notifyLogin(s.name, `${info.name} needs you to log in`);
          throw new LoginRequired(`${s.name} is not logged in${st.check_error ? ` (${st.check_error})` : ""} — log in (POST /sites/${s.name}/login, \`spoor login ${s.name}\`) and try again`);
        }
      }
      const meta = this.runs.start({ site: s.name, script: info.name, args: v.args, timeout_secs: ctx.body.timeout_secs ?? info.action.timeout_secs, action: true, writes: !!info.action.writes });
      const waitMs = ctx.body.wait === true ? 0 : Number(ctx.body.wait ?? -1) * 1000;
      if (waitMs < 0) return meta;
      const done = await this.runs.wait(meta.id, waitMs || undefined);
      return done ?? { ...this.runs.get(meta.id), still_running: true };
    });

    r("POST", "/sites/:site/open", async ctx => {
      const s = site(ctx);
      await this.ensureBrowser();
      await this.ext("open", { site: s.name, url: ctx.body.url ?? s.url });
      if (!ctx.body.wait) return { site: s.name, opened: true };
      const deadline = Date.now() + (ctx.body.timeout_secs ?? 600) * 1000;
      while (Date.now() < deadline) {
        const st = await this.status(s.name);
        if (st.logged_in) return { site: s.name, opened: true, logged_in: true };
        await new Promise(res => setTimeout(res, 3000));
      }
      return { site: s.name, opened: true, logged_in: false };
    });
    r("POST", "/sites/:site/stop", async ctx => {
      await this.ext("stop", { site: site(ctx).name });
      return { stopped: true };
    });
    r("GET", "/sites/:site/audit", ctx => store.readAudit(site(ctx).name, Number(ctx.query.get("tail") ?? 100)));

    // scripts
    r("GET", "/sites/:site/scripts", ctx => store.listScripts(site(ctx).name));
    r("GET", "/sites/:site/scripts/:name", ctx => ({ name: ctx.params.name, code: store.readScript(site(ctx).name, ctx.params.name!) }));
    r("PUT", "/sites/:site/scripts/:name", ctx => {
      if (typeof ctx.body.code !== "string") throw new BadRequest("body needs {code}");
      const info = store.writeScript(site(ctx).name, ctx.params.name!, ctx.body.code);
      this.changed(ctx.params.site);
      return info;
    });
    r("DELETE", "/sites/:site/scripts/:name", ctx => {
      const deleted = store.deleteScript(site(ctx).name, ctx.params.name!);
      this.changed(ctx.params.site);
      return { deleted };
    });

    // runs
    const startRun = (ctx: Ctx) =>
      this.runs.start({
        site: site(ctx).name,
        script: ctx.body.script,
        code: ctx.body.code,
        args: ctx.body.args,
        url: ctx.body.url,
        timeout_secs: ctx.body.timeout_secs,
      });
    r("POST", "/sites/:site/runs", async ctx => {
      const meta = startRun(ctx);
      if (!ctx.body.wait) return meta;
      return (await this.runs.wait(meta.id)) ?? meta;
    });
    // exec = start a run and wait for it, returning the result and console output.
    r("POST", "/sites/:site/exec", async ctx => {
      const meta = startRun(ctx);
      const done = (await this.runs.wait(meta.id))!;
      const { events } = await this.runs.events(meta.site, meta.id, 0, 0);
      const logs = events.flatMap(e => (e.type === "log" ? [`${e.level === "log" ? "" : `[${e.level}] `}${e.text}`] : []));
      return { run_id: meta.id, ok: done.status === "ok", status: done.status, result: done.result, error: done.error, logs, requests: done.requests, duration_ms: done.duration_ms };
    });
    r("GET", "/sites/:site/runs", ctx => {
      const s = site(ctx).name;
      const live = new Map(this.runs.activeRuns().filter(m => m.site === s).map(m => [m.id, m]));
      return store.listRuns(s, Number(ctx.query.get("limit") ?? 50)).map(m => live.get(m.id) ?? m);
    });
    r("GET", "/sites/:site/runs/:id", ctx => {
      const s = site(ctx).name;
      const meta = this.runs.get(ctx.params.id!) ?? store.readRunMeta(s, ctx.params.id!);
      return { meta, files: store.listRunFiles(s, ctx.params.id!) };
    });
    r("GET", "/sites/:site/runs/:id/events", ctx =>
      this.runs.events(site(ctx).name, ctx.params.id!, Number(ctx.query.get("after") ?? 0), Math.min(Number(ctx.query.get("wait_ms") ?? 0), 60_000)),
    );
    r("GET", "/sites/:site/runs/:id/files/:name", ctx => {
      const path = store.runFilePath(site(ctx).name, ctx.params.id!, ctx.params.name!);
      return { __file: path, contentType: contentType(ctx.params.name!) };
    });
    for (const action of ["pause", "resume", "stop"] as const) {
      r("POST", `/sites/:site/runs/:id/${action}`, async ctx => {
        site(ctx);
        await this.runs.control(ctx.params.id!, action);
        return { ok: true };
      });
    }

    // recording
    r("POST", "/sites/:site/record/start", async ctx => {
      const s = site(ctx);
      if (this.recordings.has(s.name)) throw new BadRequest(`already recording ${s.name} (${this.recordings.get(s.name)!.id})`);
      const meta: RecordingMeta = { id: newId(), site: s.name, started: nowIso(), flows: 0, start_url: ctx.body.url };
      store.writeRecordingMeta(meta);
      this.recordings.set(s.name, meta);
      try {
        await this.ensureBrowser();
        await this.ext("record.start", { site: s.name, rec_id: meta.id, url: ctx.body.url, open: ctx.body.open !== false, site_url: s.url });
      } catch (e) {
        this.recordings.delete(s.name);
        throw e;
      }
      this.changed(s.name);
      return meta;
    });
    r("POST", "/sites/:site/record/stop", async ctx => {
      const s = site(ctx);
      const meta = this.recordings.get(s.name);
      if (!meta) throw new BadRequest(`not recording ${s.name}`);
      const snap = await this.ext<{ cookies: CookieInfo[]; storage: StorageSnapshot[] }>("record.stop", { site: s.name, rec_id: meta.id }, 120_000).catch(e => {
        log(`record.stop ${s.name}: ${e.message}`);
        return { cookies: [], storage: [] };
      });
      // Flow events can still be in flight right behind the reply.
      await new Promise(res => setTimeout(res, 300));
      this.recordings.delete(s.name);
      meta.stopped = nowIso();
      store.writeRecordingMeta(meta);
      store.writeRecordingFile(s.name, meta.id, "cookies.json", snap.cookies);
      store.writeRecordingFile(s.name, meta.id, "storage.json", snap.storage);
      this.changed(s.name);
      return { ...meta, dir: paths.recording(s.name, meta.id), cookies: snap.cookies.length, storage: snap.storage.length };
    });
    r("POST", "/sites/:site/record/mark", async ctx => {
      const s = site(ctx);
      if (!this.recordings.has(s.name)) throw new BadRequest(`not recording ${s.name}`);
      await this.ext("record.mark", { site: s.name, label: String(ctx.body.label ?? "") });
      return { marked: true };
    });
    r("GET", "/sites/:site/recordings", ctx => store.listRecordings(site(ctx).name).map(x => x.meta));
    r("GET", "/recordings", () => store.listRecordings().map(x => x.meta));
    r("GET", "/recordings/:ref/flows", ctx => {
      const rec = store.findRecording(ctx.params.ref!);
      const flows = matchingFlows(store.readFlows(rec), ctx.query.get("grep") ?? undefined);
      return ctx.query.get("full") ? flows : flows.map(flowLine);
    });
    r("GET", "/recordings/:ref/flows/:seq", ctx => {
      const flow = store.readFlows(store.findRecording(ctx.params.ref!)).find(f => f.sequence === Number(ctx.params.seq));
      if (!flow) throw new NotFound(`no flow ${ctx.params.seq}`);
      return flow;
    });
    r("GET", "/recordings/:ref/trace", ctx => trace(store.readFlows(store.findRecording(ctx.params.ref!)), ctx.query.get("value") ?? ""));
    r("GET", "/recordings/:ref/auth", ctx => authChain(store.readFlows(store.findRecording(ctx.params.ref!))));
    r("GET", "/recordings/:ref/endpoints", ctx => endpoints(store.readFlows(store.findRecording(ctx.params.ref!))));
    r("GET", "/recordings/:ref/lifetimes", ctx => {
      const rec = store.findRecording(ctx.params.ref!);
      return lifetimes(store.readFlows(rec), store.readRecordingFile(rec, "cookies.json", []), store.readRecordingFile(rec, "storage.json", []));
    });
    r("POST", "/recordings/:ref/replay", async ctx => {
      const rec = store.findRecording(ctx.params.ref!);
      const flow = store.readFlows(rec).find(f => f.sequence === Number(ctx.body.seq));
      if (!flow || flow.kind !== "http") throw new NotFound(`no request ${ctx.body.seq} in ${rec.id}`);
      if (isWrite(flow.method ?? "GET") && !ctx.body.allow_write) throw new BadRequest(`${flow.method} may change data on the site; pass allow_write to replay it`);
      const s = store.getSite(rec.site);
      let live: [string, string][] | undefined;
      if (ctx.body.live) {
        const cookies = await this.ext<CookieInfo[]>("cookies", { site: s.name, url: flow.url });
        live = cookies.map(c => [c.name, c.value]);
      }
      const req = requestFromFlow(flow, live);
      const pace = () => this.pacer.wait(s.name, store.minGap(s));
      const report = ctx.body.minimize
        ? await minimize(flow, req, pace)
        : await pace().then(() => send(req, flow)).then(r => ({ url: flow.url, method: req.method, baseline: r, requests_sent: 1 }));
      store.audit(s.name, { event: "replay", recording: rec.id, seq: flow.sequence, method: req.method, url: flow.url, requests: report.requests_sent, status: report.baseline.status });
      const out = { recording: rec.id, seq: flow.sequence, cookies_from: live ? "browser (live)" : "recording", ...report, at: nowIso() };
      store.writeJson(join(paths.dossier(s.name), "replay", `${rec.id}-${flow.sequence}.json`), out);
      return out;
    });
    r("POST", "/sites/:site/dossier", ctx => {
      const s = site(ctx);
      const ref = ctx.body.recording ? store.findRecording(ctx.body.recording) : store.listRecordings(s.name)[0];
      if (!ref) throw new BadRequest(`no recordings for ${s.name} — record one first`);
      return { recording: ref.id, files: buildDossier(s.name, ref) };
    });
  }

  private async status(name: string, retried = false): Promise<{ site: string; browser: SiteBrowserState | null; logged_in?: boolean; check?: string; check_error?: string }> {
    const s = store.getSite(name);
    if (!this.peer) return { site: name, browser: null };
    const browser = this.withCheck(await this.ext<SiteBrowserState>("status", { site: name }));
    if (!s.check) return { site: name, browser };
    // The check runs in the worker tab like any run; don't queue it behind a long batch.
    if (this.runs.activeRuns().some(m => m.site === name)) {
      const known = store.readSession(name);
      return { site: name, browser, check: s.check, logged_in: known.state === "logged_in" ? true : undefined, check_error: "skipped: a run is in progress" };
    }
    // On a page that never redirects: loading the start page while logged out
    // would set off an SSO round trip in the worker tab (next to the human's).
    const meta = this.runs.start({ site: name, script: s.check, ephemeral: true, timeout_secs: 60, url: `${new URL(s.url).origin}/robots.txt` });
    const done = (await this.runs.wait(meta.id))!;
    // The browser going away says nothing about the login. Right after a
    // start Firefox may reinstall a rebuilt add-on, which drops the first
    // connection: reconnect and ask once more.
    if (done.error && /Firefox (is not running|disconnected|did not connect)|connection to the Spoor service/.test(done.error)) {
      if (!retried) {
        await this.ensureBrowser().catch(() => {});
        if (this.peer) return this.status(name, true);
      }
      return { site: name, browser, check: s.check, check_error: done.error };
    }
    // A check that failed (threw, timed out) doesn't know either: only a
    // false result means logged out.
    const state = done.status !== "ok" ? "unknown" : done.result ? "logged_in" : "logged_out";
    const prev = store.writeSession(name, { state, error: done.error });
    if (prev.state !== state) {
      log(`session: ${name} ${prev.state} → ${state}${done.error ? ` (${done.error})` : ""}`);
      store.audit(name, { event: "session.state", state, error: done.error });
      this.changed(name);
    }
    return { site: name, browser, check: s.check, logged_in: state === "unknown" ? undefined : state === "logged_in", check_error: done.error };
  }

  /** A login check in progress shows as active_check: it has no run folder to look up. */
  private withCheck(b: SiteBrowserState): SiteBrowserState {
    if (b.active_run && this.runs.isEphemeral(b.active_run)) {
      b.active_check = b.active_run;
      delete b.active_run;
    }
    return b;
  }

  private actions(site: string) {
    return store.listScripts(site).filter(x => x.action || x.action_error).map(x => ({
      site,
      name: x.name,
      description: x.description,
      ...(x.action ? { params: x.action.params ?? {}, required: x.action.required ?? [], writes: !!x.action.writes, login: x.action.login !== false, input_schema: inputSchema(x.action) } : { error: x.action_error }),
    }));
  }

  private notifyLogin(site: string, message: string): void {
    this.peer?.emit("notify", { id: `login-${site}`, title: `Spoor · ${site}: please log in`, message, site });
  }

  /**
   * Ask the human to log in: bring a tab of the site's container to the
   * front, notify, and (with `wait`) poll until the check script passes.
   *
   * The check only runs while a visible tab of the site sits on the site's
   * own origin and has finished loading: checking in the middle of an SSO
   * round trip would start a second login flow next to the human's.
   */
  async login(name: string, timeoutSecs: number, wait: boolean): Promise<{ site: string; logged_in?: boolean; check_error?: string; opened?: boolean; already?: boolean }> {
    const s = store.getSite(name);
    await this.ensureBrowser();
    if (s.check) {
      const st = await this.status(name);
      if (st.logged_in) return { site: name, logged_in: true, already: true };
    }
    await this.ext("login.prompt", { site: name, url: s.url });
    this.notifyLogin(name, "Log in in the tab Spoor opened; it notices when you're done.");
    store.audit(name, { event: "login.requested" });
    if (!s.check) return { site: name, opened: true, check_error: "no check script: Spoor can't tell when you are logged in" };
    if (!wait) return { site: name, opened: true };
    if (this.logins.has(name)) throw new BadRequest(`already waiting for a login on ${name}`);
    this.logins.add(name);
    try {
      const origin = new URL(s.url).origin;
      const deadline = Date.now() + timeoutSecs * 1000;
      let last: { logged_in?: boolean; check_error?: string } = {};
      // Check when a tab lands somewhere new on the site (the login just
      // redirected back), otherwise at most every 30 s: sites often have
      // their own login page on the same origin, and the human may sit there.
      let checkedAt = 0;
      let checkedUrls = "";
      while (Date.now() < deadline) {
        await new Promise(res => setTimeout(res, 3000));
        if (!this.peer) continue;
        const tabs = await this.ext<{ url?: string; status?: string }[]>("login.tabs", { site: name }).catch(() => []);
        const onSite = tabs.filter(t => t.status === "complete" && t.url && new URL(t.url).origin === origin).map(t => t.url).sort().join(" ");
        if (!onSite) continue;
        if (onSite === checkedUrls && Date.now() - checkedAt < 30_000) continue;
        checkedUrls = onSite;
        checkedAt = Date.now();
        last = await this.status(name);
        if (last.logged_in) {
          store.audit(name, { event: "login.done" });
          this.peer?.emit("notify", { id: `login-${name}`, title: `Spoor · ${name}: logged in`, message: "Thanks — Spoor carries on in the background.", clear_after_ms: 5000 });
          return { site: name, logged_in: true };
        }
      }
      return { site: name, logged_in: false, check_error: last.check_error ?? "timed out waiting for the login" };
    } finally {
      this.logins.delete(name);
    }
  }

  /** Sites with keepalive: re-check on schedule; tell the human when a login lapsed. */
  private async keepalive(): Promise<void> {
    if (!this.peer) return;
    for (const s of store.listSites()) {
      if (!s.keepalive_mins || !s.check) continue;
      const last = this.lastKeepalive.get(s.name) ?? 0;
      if (Date.now() - last < s.keepalive_mins * 60_000) continue;
      if (this.runs.activeRuns().some(m => m.site === s.name) || this.logins.has(s.name)) continue;
      this.lastKeepalive.set(s.name, Date.now());
      const before = store.readSession(s.name).state;
      const st = await this.status(s.name).catch(e => ({ logged_in: false, check_error: (e as Error).message }));
      if (before === "logged_in" && !st.logged_in) this.notifyLogin(s.name, "The session expired. Log in again when you have a moment.");
    }
  }
}

function cookieSummary(cookies: CookieInfo[]) {
  const persistent = cookies.filter(c => !c.session && c.expirationDate);
  const earliest = persistent.reduce<number | undefined>((a, c) => (a === undefined || c.expirationDate! < a ? c.expirationDate : a), undefined);
  return {
    count: cookies.length,
    session_cookies: cookies.filter(c => c.session).length,
    hosts: [...new Set(cookies.map(c => c.domain.replace(/^\./, "")))].sort(),
    earliest_expiry: earliest ? new Date(earliest * 1000).toISOString() : undefined,
  };
}

function fileForExtension(path: string, ct: string): unknown {
  const size = statSync(path).size;
  if (size > MAX_API_FILE_BYTES) return { too_large: true, bytes: size };
  const buf = readFileSync(path);
  const text = /^(text\/|application\/json)/.test(ct);
  return text ? { text: buf.toString("utf8"), bytes: size, contentType: ct } : { base64: buf.toString("base64"), bytes: size, contentType: ct };
}

function contentType(name: string): string {
  if (name.endsWith(".json") || name.endsWith(".jsonl")) return "application/json";
  if (/\.(txt|md|csv|log|js|html|xml)$/.test(name)) return "text/plain; charset=utf-8";
  return "application/octet-stream";
}

function readBody(req: IncomingMessage): Promise<unknown> {
  return new Promise((resolve, reject) => {
    const chunks: Buffer[] = [];
    req.on("data", c => chunks.push(c));
    req.on("end", () => {
      const text = Buffer.concat(chunks).toString("utf8");
      if (!text.trim()) return resolve({});
      try {
        resolve(JSON.parse(text));
      } catch (e) {
        reject(new BadRequest(`invalid JSON body: ${(e as Error).message}`));
      }
    });
    req.on("error", reject);
  });
}

function tokenOk(got: string | undefined, token: string): boolean {
  if (!got) return false;
  const a = Buffer.from(got);
  const b = Buffer.from(token);
  return a.length === b.length && timingSafeEqual(a, b);
}

export function readServeInfo(): ServeInfo | null {
  try {
    return JSON.parse(readFileSync(paths.serve(), "utf8")) as ServeInfo;
  } catch {
    return null;
  }
}

export async function serve(port = DEFAULT_PORT, token = randomBytes(32).toString("hex")): Promise<void> {
  const service = new Service();
  const wss = new WebSocketServer({ noServer: true, maxPayload: 256 * 1024 * 1024 });

  const server = createServer(async (req: IncomingMessage, res: ServerResponse) => {
    const json = (status: number, body: unknown) => {
      res.writeHead(status, { "content-type": "application/json" });
      res.end(JSON.stringify(body));
    };
    const auth = req.headers.authorization?.replace(/^Bearer /, "");
    if (!tokenOk(auth, token)) return json(401, { error: "missing or wrong bearer token (see serve.json)" });
    let body: unknown;
    try {
      body = await readBody(req);
    } catch (e) {
      return json(400, { error: (e as Error).message });
    }
    const out = await service.dispatch(req.method ?? "GET", req.url ?? "/", body, false);
    if (out.file) {
      res.writeHead(200, { "content-type": out.file.contentType });
      createReadStream(out.file.path).pipe(res);
      return;
    }
    json(out.status, out.body);
  });
  server.requestTimeout = 0; // exec and long polls hold requests open
  server.headersTimeout = 60_000;

  server.on("upgrade", (req, socket, head) => {
    const url = new URL(req.url ?? "/", "http://x");
    const auth = req.headers.authorization?.replace(/^Bearer /, "");
    if (url.pathname !== "/bridge" || !tokenOk(auth, token)) {
      socket.write("HTTP/1.1 401 Unauthorized\r\n\r\n");
      socket.destroy();
      return;
    }
    wss.handleUpgrade(req, socket, head, ws => service.attach(ws));
  });

  await new Promise<void>((resolve, reject) => {
    server.once("error", reject);
    server.listen(port, "127.0.0.1", () => resolve());
  });
  const info: ServeInfo = { url: `http://127.0.0.1:${port}`, token, pid: process.pid, version: VERSION };
  store.writeJson(paths.serve(), info);
  log(`serve: listening on ${info.url} (pid ${process.pid})`);

  const exit = () => {
    try {
      const current = readServeInfo();
      if (current?.pid === process.pid && existsSync(paths.serve())) rmSync(paths.serve());
    } catch {}
    log("serve: shutting down");
    process.exit(0);
  };
  service.onShutdown(exit);
  process.on("SIGINT", exit);
  process.on("SIGTERM", exit);
}
