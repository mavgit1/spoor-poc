// spoor — command line. Commands that need the browser talk to the local
// service (started in the background on first use). Commands that only read
// recordings (flows, trace, auth, …) read the files directly.

import { readFileSync } from "node:fs";
import { parseArgs, type ParseArgsConfig } from "node:util";

import type { RunEvent, RunMeta } from "../shared/types.ts";
import { api, ApiError, runningService } from "../service/client.ts";
import { authMarkdown, duration, endpointsMarkdown, lifetimesMarkdown } from "../service/dossier.ts";
import { authChain, endpoints, flowLine, lifetimes, matchingFlows, trace } from "../service/inspect.ts";
import { DEFAULT_PORT, paths, spoorHome } from "../service/paths.ts";
import { serve } from "../service/server.ts";
import * as store from "../service/store.ts";
import { launch, sign } from "./firefox.ts";
import { install } from "./install.ts";

const HELP = `spoor — workbench for sites with no API

Setup
  spoor install                         register the native messaging host with Firefox
  spoor browser [--dev] [--headless]    start Spoor's Firefox (home panel + sidebar)
  spoor sign                            sign the extension (unlisted, needs AMO_JWT_ISSUER/SECRET)
  spoor serve [--port 7517]             run the local service in the foreground
  spoor shutdown | state

Sites
  spoor site add <name> <url> [--check <script>] [--min-gap-ms <n>]
  spoor site list | remove <name> | info <name>
  spoor notes <site> [--set <file>|-]
  spoor open <site> [--url <u>] [--wait] [--timeout <s>]
  spoor status <site>
  spoor stop [site]                     kill switch: stop runs, close worker tabs

Scripts and runs
  spoor script list <site> | show <site> <name> | add <site> <name> <file> | rm <site> <name>
  spoor run <site> <script> [--arg k=v]... [--args <json|@file>] [--url <u>] [--timeout <s>] [--detach]
  spoor exec <site> [<file> | -e <code>] [--arg k=v]... [--args …] [--url <u>] [--timeout <s>] [--full]
  spoor runs <site> [<id>] [--pause|--resume|--stop]
  spoor audit <site> [--tail <n>]

Recording and discovery     (<rec> = id, <site>:<id>, latest or <site>:latest)
  spoor record start <site> [--url <u>] [--no-open]
  spoor record stop <site> | mark <site> <label>
  spoor recordings [--site <site>]
  spoor flows <rec> [--grep <text>] [--full]
  spoor flow <rec> <seq>
  spoor trace <rec> <value>
  spoor auth <rec> | lifetimes <rec> | endpoints <rec>   [--json]
  spoor replay <rec> <seq> [--live] [--minimize] [--allow-write]
  spoor dossier <site> [--recording <rec>]

State lives in ${spoorHome()} (SPOOR_HOME).`;

type Opts = NonNullable<ParseArgsConfig["options"]>;

function parse(argv: string[], options: Opts): { values: Record<string, any>; positionals: string[] } {
  return parseArgs({ args: argv, options, allowPositionals: true, strict: true });
}

function out(v: unknown): void {
  console.log(typeof v === "string" ? v : JSON.stringify(v, null, 2));
}

function need<T>(v: T | undefined, what: string): T {
  if (v === undefined || v === "") throw new Usage(`missing ${what}`);
  return v;
}

class Usage extends Error {}

/** --args '{"a":1}' / --args @file.json, plus repeated --arg k=v (values parsed as JSON when they parse). */
function collectArgs(values: { args?: string; arg?: string[] }): Record<string, unknown> {
  let base: Record<string, unknown> = {};
  if (values.args) {
    const text = values.args.startsWith("@") ? readFileSync(values.args.slice(1), "utf8") : values.args;
    base = JSON.parse(text);
  }
  for (const kv of values.arg ?? []) {
    const i = kv.indexOf("=");
    if (i < 0) throw new Usage(`--arg wants key=value, got ${JSON.stringify(kv)}`);
    const v = kv.slice(i + 1);
    try {
      base[kv.slice(0, i)] = JSON.parse(v);
    } catch {
      base[kv.slice(0, i)] = v;
    }
  }
  return base;
}

const ARG_OPTS: Opts = { arg: { type: "string", multiple: true }, args: { type: "string" }, url: { type: "string" }, timeout: { type: "string" } };

function eventLine(e: RunEvent): string | null {
  switch (e.type) {
    case "log": return `${e.level === "log" ? "" : `[${e.level}] `}${e.text}`;
    case "progress": return `· progress ${e.progress.done}${e.progress.total !== undefined ? `/${e.progress.total}` : ""}${e.progress.label ? ` ${e.progress.label}` : ""}`;
    case "confirm": return `· waiting for approval in the sidebar: ${e.message}`;
    case "confirmed": return `· ${e.approved ? "approved" : "rejected"}`;
    case "saved": return `· saved ${e.file} (${e.bytes} bytes)`;
    case "status": return e.status === "paused" || e.status === "running" ? `· ${e.status}` : null;
    default: return null;
  }
}

/** Follow a run's events until it finishes; console output goes to stderr, the result to stdout. */
async function follow(site: string, id: string): Promise<number> {
  let after = 0;
  for (;;) {
    const r = await api<{ events: RunEvent[]; status: string }>("GET", `/sites/${site}/runs/${id}/events?after=${after}&wait_ms=25000`);
    for (const e of r.events) {
      after = Math.max(after, e.n);
      if (e.type === "result") {
        if (e.ok) out(e.result ?? null);
        else console.error(`error: ${e.error}`);
      } else {
        const line = eventLine(e);
        if (line !== null) console.error(line);
      }
    }
    if (["ok", "error", "stopped"].includes(r.status) && r.events.length === 0) {
      const meta = await api<{ meta: RunMeta; files: { name: string }[] }>("GET", `/sites/${site}/runs/${id}`);
      console.error(`· ${meta.meta.status} · run ${id} · ${meta.meta.requests ?? 0} requests · ${paths.run(site, id)}`);
      return meta.meta.status === "ok" ? 0 : 1;
    }
  }
}

async function main(argv: string[]): Promise<number> {
  const [cmd, ...rest] = argv;
  if (!cmd || cmd === "help" || cmd === "--help" || cmd === "-h") {
    out(HELP);
    return 0;
  }
  switch (cmd) {
    case "serve": {
      const { values } = parse(rest, { port: { type: "string" }, token: { type: "string" } });
      await serve(Number(values.port ?? process.env.SPOOR_PORT ?? DEFAULT_PORT), values.token as string | undefined);
      console.error(`spoor serve on 127.0.0.1:${values.port ?? DEFAULT_PORT} (state in ${spoorHome()})`);
      return new Promise<number>(() => {});
    }
    case "shutdown": {
      if (!(await runningService())) return out("not running"), 0;
      out(await api("POST", "/shutdown", undefined, { start: false }));
      return 0;
    }
    case "state":
      out(await api("GET", "/state"));
      return 0;
    case "install": {
      const r = install();
      out(`native host registered\n  manifest  ${r.manifest}\n  launcher  ${r.launcher}${r.registry ? `\n  registry  ${r.registry}` : ""}`);
      return 0;
    }
    case "sign":
      out(`signed: ${sign()}`);
      return 0;
    case "browser": {
      const { values, positionals } = parse(rest, { dev: { type: "boolean" }, headless: { type: "boolean" } });
      const code = await launch({ dev: !!values.dev, headless: !!values.headless, urls: positionals });
      if (code === null) out(`Spoor's Firefox started (profile ${paths.profile()})`);
      return code ?? 0;
    }

    case "site": {
      const [sub, ...r] = rest;
      if (sub === "add") {
        const { values, positionals } = parse(r, { check: { type: "string" }, "min-gap-ms": { type: "string" } });
        const [name, url] = positionals;
        const s = store.addSite(need(name, "name"), {
          url: need(url, "url"),
          check: values.check as string | undefined,
          min_gap_ms: values["min-gap-ms"] !== undefined ? Number(values["min-gap-ms"]) : undefined,
        });
        out(s);
        out(`folder: ${paths.site(s.name)}`);
        return 0;
      }
      if (sub === "list") {
        for (const s of store.listSites()) out(`${s.name.padEnd(16)} ${s.url}${s.check ? `  check=${s.check}` : ""}${s.min_gap_ms !== undefined ? `  gap=${s.min_gap_ms}ms` : ""}`);
        return 0;
      }
      if (sub === "remove") {
        out({ removed: store.removeSite(need(r[0], "name")) });
        return 0;
      }
      if (sub === "info") {
        const name = need(r[0], "name");
        if (await runningService()) out(await api("GET", `/sites/${name}`));
        else out({ site: store.getSite(name), dir: paths.site(name), notes: store.readNotes(name), scripts: store.listScripts(name), runs: store.listRuns(name, 10), recordings: store.listRecordings(name).slice(0, 10).map(x => x.meta) });
        return 0;
      }
      throw new Usage("spoor site add|list|remove|info");
    }
    case "notes": {
      const { values, positionals } = parse(rest, { set: { type: "string" } });
      const site = store.getSite(need(positionals[0], "site")).name;
      if (values.set !== undefined) {
        const text = values.set === "-" ? readFileSync(0, "utf8") : readFileSync(values.set as string, "utf8");
        store.writeNotes(site, text);
        return 0;
      }
      out(store.readNotes(site));
      return 0;
    }
    case "open": {
      const { values, positionals } = parse(rest, { url: { type: "string" }, wait: { type: "boolean" }, timeout: { type: "string" } });
      out(await api("POST", `/sites/${need(positionals[0], "site")}/open`, { url: values.url, wait: values.wait, timeout_secs: values.timeout ? Number(values.timeout) : undefined }));
      return 0;
    }
    case "status":
      out(await api("GET", `/sites/${need(rest[0], "site")}/status`));
      return 0;
    case "stop":
      out(await api("POST", rest[0] ? `/sites/${rest[0]}/stop` : "/stop"));
      return 0;

    case "script": {
      const [sub, site, name, file] = rest;
      need(site, "site");
      store.getSite(site!);
      if (sub === "list") {
        for (const s of store.listScripts(site!)) out(`${s.name.padEnd(24)} ${s.description ?? ""}`);
        return 0;
      }
      if (sub === "show") return out(store.readScript(site!, need(name, "name"))), 0;
      if (sub === "add") {
        const code = need(file, "file") === "-" ? readFileSync(0, "utf8") : readFileSync(file!, "utf8");
        out(store.writeScript(site!, need(name, "name"), code));
        return 0;
      }
      if (sub === "rm") return out({ deleted: store.deleteScript(site!, need(name, "name")) }), 0;
      throw new Usage("spoor script list|show|add|rm");
    }
    case "run": {
      const { values, positionals } = parse(rest, { ...ARG_OPTS, detach: { type: "boolean" } });
      const [site, script] = positionals;
      const meta = await api<RunMeta>("POST", `/sites/${need(site, "site")}/runs`, {
        script: need(script, "script"),
        args: collectArgs(values as { args?: string; arg?: string[] }),
        url: values.url,
        timeout_secs: values.timeout ? Number(values.timeout) : undefined,
      });
      if (values.detach) return out(meta), 0;
      console.error(`· run ${meta.id} (${site}/${script}) — progress and approvals also in the sidebar`);
      return follow(site!, meta.id);
    }
    case "exec": {
      const { values, positionals } = parse(rest, { ...ARG_OPTS, eval: { type: "string", short: "e" }, full: { type: "boolean" } });
      const [site, file] = positionals;
      const code = (values.eval as string | undefined) ?? (file ? readFileSync(file === "-" ? 0 : file, "utf8") : undefined);
      const r = await api("POST", `/sites/${need(site, "site")}/exec`, {
        code: need(code, "script file or -e code"),
        args: collectArgs(values as { args?: string; arg?: string[] }),
        url: values.url,
        timeout_secs: values.timeout ? Number(values.timeout) : undefined,
      });
      if (values.full) return out(r), r.ok ? 0 : 1;
      for (const l of r.logs ?? []) console.error(l);
      if (r.ok) out(r.result ?? null);
      else console.error(`error: ${r.error}`);
      return r.ok ? 0 : 1;
    }
    case "runs": {
      const { values, positionals } = parse(rest, { pause: { type: "boolean" }, resume: { type: "boolean" }, stop: { type: "boolean" }, limit: { type: "string" } });
      const [site, id] = positionals;
      need(site, "site");
      if (!id) {
        for (const m of store.listRuns(site!, Number(values.limit ?? 30))) {
          out(`${m.id}  ${m.status.padEnd(8)} ${m.script.padEnd(20)} ${m.duration_ms !== undefined ? duration(Math.round(m.duration_ms / 1000)).padEnd(8) : "".padEnd(8)} ${m.error ?? ""}`);
        }
        return 0;
      }
      const action = values.pause ? "pause" : values.resume ? "resume" : values.stop ? "stop" : null;
      if (action) return out(await api("POST", `/sites/${site}/runs/${id}/${action}`)), 0;
      out({ meta: store.readRunMeta(site!, id), files: store.listRunFiles(site!, id), dir: paths.run(site!, id) });
      for (const e of store.readRunEvents(site!, id)) {
        const line = e.type === "result" ? `· result ${e.ok ? JSON.stringify(e.result).slice(0, 500) : e.error}` : eventLine(e);
        if (line) console.log(line);
      }
      return 0;
    }
    case "audit": {
      const { values, positionals } = parse(rest, { tail: { type: "string" } });
      for (const e of store.readAudit(need(positionals[0], "site"), Number(values.tail ?? 50))) out(JSON.stringify(e));
      return 0;
    }

    case "record": {
      const [sub, ...r] = rest;
      const { values, positionals } = parse(r, { url: { type: "string" }, "no-open": { type: "boolean" } });
      const site = need(positionals[0], "site");
      if (sub === "start") return out(await api("POST", `/sites/${site}/record/start`, { url: values.url, open: !values["no-open"] })), 0;
      if (sub === "stop") return out(await api("POST", `/sites/${site}/record/stop`)), 0;
      if (sub === "mark") return out(await api("POST", `/sites/${site}/record/mark`, { label: positionals.slice(1).join(" ") })), 0;
      throw new Usage("spoor record start|stop|mark");
    }
    case "recordings": {
      const { values } = parse(rest, { site: { type: "string" } });
      for (const r of store.listRecordings(values.site as string | undefined)) out(`${r.site}:${r.id}  ${String(r.meta.flows).padStart(5)} flows  ${r.meta.started}${r.meta.stopped ? "" : "  (recording)"}`);
      return 0;
    }
    case "flows": {
      const { values, positionals } = parse(rest, { grep: { type: "string" }, full: { type: "boolean" } });
      const flows = matchingFlows(store.readFlows(store.findRecording(need(positionals[0], "recording"))), values.grep as string | undefined);
      for (const f of flows) out(values.full ? JSON.stringify(f) : flowLine(f));
      return 0;
    }
    case "flow": {
      const [ref, seq] = rest;
      const f = store.readFlows(store.findRecording(need(ref, "recording"))).find(x => x.sequence === Number(need(seq, "seq")));
      if (!f) throw new Usage(`no flow ${seq}`);
      out(f);
      return 0;
    }
    case "trace": {
      const [ref, value] = rest;
      const t = trace(store.readFlows(store.findRecording(need(ref, "recording"))), need(value, "value"));
      out("appears in (server → browser):");
      for (const h of t.appears_in) out(`  ${String(h.sequence).padStart(5)} ${h.method} ${h.url}\n        ${h.place}: ${h.snippet}`);
      out("used in (browser → server):");
      for (const h of t.used_in) out(`  ${String(h.sequence).padStart(5)} ${h.method} ${h.url}\n        ${h.place}: ${h.snippet}`);
      return 0;
    }
    case "auth":
    case "lifetimes":
    case "endpoints": {
      const { values, positionals } = parse(rest, { json: { type: "boolean" } });
      const rec = store.findRecording(need(positionals[0], "recording"));
      const flows = store.readFlows(rec);
      if (cmd === "auth") {
        const c = authChain(flows);
        out(values.json ? c : authMarkdown(c));
      } else if (cmd === "endpoints") {
        const e = endpoints(flows);
        out(values.json ? e : endpointsMarkdown(e));
      } else {
        const l = lifetimes(flows, store.readRecordingFile(rec, "cookies.json", []), store.readRecordingFile(rec, "storage.json", []));
        out(values.json ? l : lifetimesMarkdown(l));
      }
      return 0;
    }
    case "replay": {
      const { values, positionals } = parse(rest, { live: { type: "boolean" }, minimize: { type: "boolean" }, "allow-write": { type: "boolean" } });
      const [ref, seq] = positionals;
      out(await api("POST", `/recordings/${encodeURIComponent(need(ref, "recording"))}/replay`, {
        seq: Number(need(seq, "seq")),
        live: values.live,
        minimize: values.minimize,
        allow_write: values["allow-write"],
      }));
      return 0;
    }
    case "dossier": {
      const { values, positionals } = parse(rest, { recording: { type: "string" } });
      out(await api("POST", `/sites/${need(positionals[0], "site")}/dossier`, { recording: values.recording }));
      return 0;
    }
  }
  throw new Usage(`unknown command ${cmd}`);
}

main(process.argv.slice(2)).then(
  code => {
    if (code !== undefined) process.exitCode = code;
  },
  e => {
    if (e instanceof Usage) console.error(`${e.message}\n\nspoor help — list commands`);
    else if (e instanceof ApiError) console.error(`error (${e.status}): ${e.message}`);
    else console.error(`error: ${e instanceof Error ? e.message : e}`);
    process.exitCode = e instanceof Usage ? 2 : 1;
  },
);
