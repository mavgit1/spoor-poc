// Runs: one execution of a script in a site's worker tab. The service owns the
// run folder and the event log; the extension executes and streams events.
//
// One run at a time per site (there is one worker tab per site); later runs
// queue in order.

import type { ExecParams, RunDone, RunEvent, RunEventInput, RunMeta, RunStatus } from "../shared/types.ts";
import { DEFAULT_TIMEOUT_SECS, newId, nowIso, paths } from "./paths.ts";
import * as store from "./store.ts";
import { writeFileSync, mkdirSync } from "node:fs";
import { join } from "node:path";

export interface StartRun {
  site: string;
  /** Library script name; mutually exclusive with `code`. */
  script?: string;
  code?: string;
  args?: unknown;
  url?: string;
  timeout_secs?: number;
  /** Check runs are not written to disk. */
  ephemeral?: boolean;
}

interface ActiveRun {
  meta: RunMeta;
  params: ExecParams;
  ephemeral: boolean;
  n: number;
  events: RunEvent[];
  done: Promise<RunMeta & { result?: unknown }>;
  resolve: (m: RunMeta & { result?: unknown }) => void;
  listeners: Set<() => void>;
  timer?: ReturnType<typeof setTimeout>;
}

export interface ExtensionLink {
  call<T = any>(method: string, params?: unknown, timeoutMs?: number): Promise<T>;
  connected(): boolean;
}

const FINAL: RunStatus[] = ["ok", "error", "stopped"];
/** Grace on top of the run's own timeout before the service gives up on the extension. */
const SERVICE_GRACE_MS = 30_000;

export class RunManager {
  private active = new Map<string, ActiveRun>();
  private queues = new Map<string, Promise<unknown>>();
  private ext: ExtensionLink;
  private onChange: (site: string) => void;

  constructor(ext: ExtensionLink, onChange: (site: string) => void) {
    this.ext = ext;
    this.onChange = onChange;
  }

  start(req: StartRun): RunMeta {
    const site = store.getSite(req.site);
    if (!req.script && !req.code) throw new store.BadRequest("give a script name or code");
    const code = req.code ?? store.readScript(site.name, req.script!);
    const id = newId();
    const timeoutMs = (req.timeout_secs ?? DEFAULT_TIMEOUT_SECS) * 1000;
    const meta: RunMeta = {
      id,
      site: site.name,
      script: req.script ?? "(inline)",
      args: req.args ?? {},
      status: "queued",
      started: nowIso(),
    };
    const params: ExecParams = {
      run_id: id,
      site: site.name,
      script: meta.script,
      site_url: site.url,
      code,
      args: req.args ?? {},
      url: req.url,
      timeout_ms: timeoutMs,
      min_gap_ms: store.minGap(site),
    };
    let resolve!: ActiveRun["resolve"];
    const done = new Promise<RunMeta & { result?: unknown }>(r => (resolve = r));
    const run: ActiveRun = { meta, params, ephemeral: !!req.ephemeral, n: 0, events: [], done, resolve, listeners: new Set() };
    this.active.set(id, run);
    if (!run.ephemeral) {
      store.writeRunMeta(meta);
      mkdirSync(paths.run(site.name, id), { recursive: true });
      writeFileSync(join(paths.run(site.name, id), "script.js"), code);
    }
    this.event(id, { type: "status", status: "queued" });

    // Serialize per site: chain onto whatever is queued there.
    const prev = this.queues.get(site.name) ?? Promise.resolve();
    const mine = prev.then(() => this.launch(run)).then(() => run.done).catch(() => undefined);
    this.queues.set(site.name, mine);
    void mine.then(() => {
      if (this.queues.get(site.name) === mine) this.queues.delete(site.name);
    });
    return meta;
  }

  private async launch(run: ActiveRun): Promise<void> {
    if (FINAL.includes(run.meta.status)) return; // stopped while queued
    if (!this.ext.connected()) {
      this.finish({ run_id: run.meta.id, ok: false, status: "error", error: "Spoor's Firefox is not connected — start it with `spoor browser`", requests: 0 });
      return;
    }
    run.meta.status = "running";
    this.event(run.meta.id, { type: "status", status: "running" });
    run.timer = setTimeout(() => {
      this.finish({ run_id: run.meta.id, ok: false, status: "error", error: `no result from the browser within the timeout`, requests: 0 });
    }, run.params.timeout_ms + SERVICE_GRACE_MS);
    try {
      await this.ext.call("exec", run.params, 30_000);
    } catch (e) {
      this.finish({ run_id: run.meta.id, ok: false, status: "error", error: (e as Error).message, requests: 0 });
    }
  }

  /** Record an event: in memory for live listeners, on disk for everyone else. */
  event(runId: string, ev: RunEventInput): void {
    const run = this.active.get(runId);
    if (!run) return;
    run.n++;
    let full: RunEvent;
    if (run.ephemeral) full = { n: run.n, ts: nowIso(), ...ev } as RunEvent;
    else full = store.appendRunEvent(run.meta.site, runId, run.n, ev);
    run.events.push(full);
    if (run.events.length > 5000) run.events.splice(0, run.events.length - 5000);

    if (ev.type === "status" && !FINAL.includes(ev.status)) run.meta.status = ev.status;
    if (ev.type === "progress") run.meta.progress = ev.progress;
    if (ev.type === "confirm") run.meta.status = "waiting";
    if (ev.type === "confirmed" && run.meta.status === "waiting") run.meta.status = "running";
    if (ev.type === "saved") run.meta.files = [...(run.meta.files ?? []), ev.file];
    if (!run.ephemeral && ev.type !== "log") store.writeRunMeta(run.meta);
    for (const l of run.listeners) l();
    if (ev.type !== "log" && !run.ephemeral) this.onChange(run.meta.site);
  }

  finish(done: RunDone): void {
    const run = this.active.get(done.run_id);
    if (!run || FINAL.includes(run.meta.status)) return;
    clearTimeout(run.timer);
    const m = run.meta;
    m.status = done.status;
    m.finished = nowIso();
    m.duration_ms = Date.parse(m.finished) - Date.parse(m.started);
    m.requests = done.requests;
    if (done.error) m.error = done.error;
    this.event(m.id, { type: "result", ok: done.ok, result: done.result, error: done.error });
    this.event(m.id, { type: "status", status: done.status, error: done.error });
    if (!run.ephemeral) {
      store.writeRunMeta(m);
      if (done.ok) writeFileSync(join(paths.run(m.site, m.id), "result.json"), JSON.stringify(done.result ?? null, null, 2) + "\n");
    }
    run.resolve({ ...m, result: done.result });
    for (const l of run.listeners) l();
    // Keep finished runs briefly so late pollers still get their last events.
    setTimeout(() => this.active.delete(m.id), 60_000).unref?.();
    if (!run.ephemeral) this.onChange(m.site);
  }

  /** The browser went away: every running run on it is lost. */
  failAll(reason: string): void {
    for (const run of this.active.values()) {
      if (!FINAL.includes(run.meta.status) && run.meta.status !== "queued") {
        this.finish({ run_id: run.meta.id, ok: false, status: "error", error: reason, requests: 0 });
      }
    }
  }

  async wait(runId: string, timeoutMs?: number): Promise<(RunMeta & { result?: unknown }) | undefined> {
    const run = this.active.get(runId);
    if (!run) return undefined;
    if (!timeoutMs) return run.done;
    return Promise.race([run.done, new Promise<undefined>(r => setTimeout(() => r(undefined), timeoutMs))]);
  }

  get(runId: string): RunMeta | undefined {
    return this.active.get(runId)?.meta;
  }

  activeRuns(): RunMeta[] {
    return [...this.active.values()].map(r => r.meta).filter(m => !FINAL.includes(m.status));
  }

  /**
   * Events after `after`. If there are none yet and the run is still going,
   * wait up to `waitMs` for the next one (long poll).
   */
  async events(site: string, runId: string, after: number, waitMs: number): Promise<{ events: RunEvent[]; status: RunStatus }> {
    const run = this.active.get(runId);
    if (!run) {
      const meta = store.readRunMeta(site, runId);
      return { events: store.readRunEvents(site, runId, after), status: meta.status };
    }
    let evs = run.events.filter(e => e.n > after);
    if (evs.length === 0 && !FINAL.includes(run.meta.status) && waitMs > 0) {
      await new Promise<void>(resolve => {
        const l = () => { clearTimeout(t); run.listeners.delete(l); resolve(); };
        const t = setTimeout(l, waitMs);
        run.listeners.add(l);
      });
      evs = run.events.filter(e => e.n > after);
    }
    // Events trimmed from memory are still on disk.
    if (evs.length > 0 && evs[0]!.n > after + 1 && !run.ephemeral) evs = store.readRunEvents(site, runId, after);
    return { events: evs, status: run.meta.status };
  }

  async control(runId: string, action: "pause" | "resume" | "stop"): Promise<void> {
    const run = this.active.get(runId);
    if (!run || FINAL.includes(run.meta.status)) throw new store.BadRequest(`run ${runId} is not active`);
    if (run.meta.status === "queued") {
      if (action !== "stop") throw new store.BadRequest("run is still queued");
      this.finish({ run_id: runId, ok: false, status: "stopped", error: "stopped before it started", requests: 0 });
      return;
    }
    await this.ext.call("run.control", { run_id: runId, action });
  }
}
