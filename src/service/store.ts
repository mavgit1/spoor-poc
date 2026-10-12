// File store. Plain files under $SPOOR_HOME so the human, the agent and the
// service all see the same thing; nothing is cached in memory that isn't also
// on disk. See paths.ts for the layout.

import { appendFileSync, copyFileSync, existsSync, mkdirSync, readFileSync, readdirSync, renameSync, rmSync, statSync, unlinkSync, writeFileSync } from "node:fs";
import { dirname, join, relative, resolve, sep } from "node:path";
import { gunzipSync } from "node:zlib";

import type { ActionSpec, Flow, RecordingMeta, RunEvent, RunEventInput, RunMeta, SessionState, Site, SiteEntry } from "../shared/types.ts";
import { parseAction } from "./actions.ts";
import { DEFAULT_MIN_GAP_MS, isValidFileName, isValidName, isValidScriptName, nowIso, paths, safeRelPath, spoorHome } from "./paths.ts";

export class NotFound extends Error {}
export class BadRequest extends Error {}

function readJson<T>(path: string, fallback: T): T {
  try {
    return JSON.parse(readFileSync(path, "utf8")) as T;
  } catch (e: any) {
    if (e?.code === "ENOENT") return fallback;
    throw new Error(`read ${path}: ${e.message}`);
  }
}

export function writeJson(path: string, value: unknown): void {
  mkdirSync(dirname(path), { recursive: true });
  writeFileSync(path, JSON.stringify(value, null, 2) + "\n");
}

function appendLine(path: string, value: unknown): void {
  mkdirSync(dirname(path), { recursive: true });
  appendFileSync(path, JSON.stringify(value) + "\n");
}

export function readJsonl<T>(path: string): T[] {
  let text: string;
  if (existsSync(path)) text = readFileSync(path, "utf8");
  else if (existsSync(path + ".gz")) text = gunzipSync(readFileSync(path + ".gz")).toString("utf8");
  else return [];
  const out: T[] = [];
  for (const line of text.split("\n")) {
    if (!line.trim()) continue;
    try {
      out.push(JSON.parse(line) as T);
    } catch {
      // a torn last line from a crash; skip it
    }
  }
  return out;
}

function listDirs(dir: string): string[] {
  try {
    return readdirSync(dir, { withFileTypes: true }).filter(d => d.isDirectory()).map(d => d.name);
  } catch {
    return [];
  }
}

// ---------------------------------------------------------------- sites

type Registry = { sites: Record<string, Site> };

export function loadSites(): Record<string, Site> {
  return readJson<Registry>(paths.sites(), { sites: {} }).sites ?? {};
}

export function listSites(): SiteEntry[] {
  return Object.entries(loadSites())
    .sort(([a], [b]) => a.localeCompare(b))
    .map(([name, s]) => ({ name, ...s }));
}

export function getSite(name: string): SiteEntry {
  if (!isValidName(name)) throw new NotFound(`invalid site name ${JSON.stringify(name)}`);
  const s = loadSites()[name];
  if (!s) throw new NotFound(`no site ${JSON.stringify(name)} — add it with \`spoor site add ${name} <url>\``);
  return { name, ...s };
}

export function minGap(site: Site): number {
  return site.min_gap_ms ?? DEFAULT_MIN_GAP_MS;
}

export function validateUrl(url: string): void {
  let u: URL;
  try {
    u = new URL(url);
  } catch {
    throw new BadRequest(`invalid url ${JSON.stringify(url)}`);
  }
  if (u.protocol !== "http:" && u.protocol !== "https:") throw new BadRequest(`site url must be http(s), got ${JSON.stringify(url)}`);
}

export function addSite(name: string, site: Site): SiteEntry {
  if (!isValidName(name)) throw new BadRequest(`site name must be lowercase letters, digits, '-' or '_' (got ${JSON.stringify(name)})`);
  validateUrl(site.url);
  if (site.check !== undefined && !isValidScriptName(site.check)) throw new BadRequest(`invalid check script name ${JSON.stringify(site.check)}`);
  if (site.min_gap_ms !== undefined && (!Number.isFinite(site.min_gap_ms) || site.min_gap_ms < 0)) throw new BadRequest("min_gap_ms must be >= 0");
  if (site.keepalive_mins !== undefined && (!Number.isFinite(site.keepalive_mins) || site.keepalive_mins < 1)) throw new BadRequest("keepalive_mins must be >= 1");
  const reg = readJson<Registry>(paths.sites(), { sites: {} });
  // Adding an existing site updates it: fields not given keep their value.
  const clean: Site = { ...reg.sites?.[name], url: site.url };
  if (site.check) clean.check = site.check;
  if (site.min_gap_ms !== undefined) clean.min_gap_ms = site.min_gap_ms;
  if (site.keepalive_mins !== undefined) clean.keepalive_mins = site.keepalive_mins;
  reg.sites = { ...reg.sites, [name]: clean };
  writeJson(paths.sites(), reg);
  mkdirSync(paths.scripts(name), { recursive: true });
  if (!existsSync(paths.notes(name))) writeFileSync(paths.notes(name), `# ${name}\n\n${site.url}\n`);
  return { name, ...clean };
}

/** Removes the registry entry only; the site's folder (scripts, runs, recordings) stays. */
export function removeSite(name: string): boolean {
  const reg = readJson<Registry>(paths.sites(), { sites: {} });
  if (!reg.sites?.[name]) return false;
  delete reg.sites[name];
  writeJson(paths.sites(), reg);
  return true;
}

// ---------------------------------------------------------------- notes

export function readNotes(site: string): string {
  try {
    return readFileSync(paths.notes(site), "utf8");
  } catch {
    return "";
  }
}

export function writeNotes(site: string, text: string): void {
  mkdirSync(paths.site(site), { recursive: true });
  writeFileSync(paths.notes(site), text);
}

// ---------------------------------------------------------------- scripts

export interface ScriptInfo {
  name: string;
  bytes: number;
  modified: string;
  /** First `//` comment line, or the action's description. */
  description?: string;
  /** Present when the script declares a spoor-action block. */
  action?: ActionSpec;
  action_error?: string;
}

function scriptPath(site: string, name: string): string {
  const bare = name.replace(/\.js$/, "");
  if (!isValidScriptName(bare)) throw new BadRequest(`invalid script name ${JSON.stringify(name)}`);
  return join(paths.scripts(site), `${bare}.js`);
}

export function listScripts(site: string): ScriptInfo[] {
  let names: string[];
  try {
    names = readdirSync(paths.scripts(site)).filter(f => f.endsWith(".js"));
  } catch {
    return [];
  }
  return names.sort().map(f => {
    const path = join(paths.scripts(site), f);
    const st = statSync(path);
    const code = readFileSync(path, "utf8");
    const first = code.split("\n", 1)[0] ?? "";
    const m = first.match(/^\s*\/\/\s*(.+)$/);
    const info: ScriptInfo = { name: f.slice(0, -3), bytes: st.size, modified: st.mtime.toISOString(), description: m?.[1] };
    const action = parseAction(code);
    if (action && "error" in action) info.action_error = action.error;
    else if (action) {
      info.action = action;
      info.description = action.description ?? info.description;
    }
    return info;
  });
}

export function readScript(site: string, name: string): string {
  try {
    return readFileSync(scriptPath(site, name), "utf8");
  } catch (e: any) {
    if (e?.code === "ENOENT") throw new NotFound(`no script ${JSON.stringify(name)} for site ${site}`);
    throw e;
  }
}

export function writeScript(site: string, name: string, code: string): ScriptInfo {
  const path = scriptPath(site, name);
  mkdirSync(dirname(path), { recursive: true });
  // CRLF from a Windows editor breaks nothing in JS, but keep files uniform.
  writeFileSync(path, code.replace(/\r\n/g, "\n"));
  return listScripts(site).find(s => s.name === name.replace(/\.js$/, ""))!;
}

export function deleteScript(site: string, name: string): boolean {
  const path = scriptPath(site, name);
  if (!existsSync(path)) return false;
  rmSync(path);
  return true;
}

// ---------------------------------------------------------------- runs

export function writeRunMeta(meta: RunMeta): void {
  writeJson(join(paths.run(meta.site, meta.id), "run.json"), meta);
}

export function readRunMeta(site: string, id: string): RunMeta {
  if (!isValidRunId(id)) throw new NotFound(`invalid run id ${JSON.stringify(id)}`);
  const meta = readJson<RunMeta | null>(join(paths.run(site, id), "run.json"), null);
  if (!meta) throw new NotFound(`no run ${id} for site ${site}`);
  return meta;
}

export function isValidRunId(id: string): boolean {
  return /^[0-9]{8}-[0-9]{6}-[0-9a-f]{4}$/.test(id);
}

export function listRuns(site: string, limit = 50): RunMeta[] {
  return listDirs(paths.runs(site))
    .filter(isValidRunId)
    .sort()
    .reverse()
    .slice(0, limit)
    .map(id => readJson<RunMeta | null>(join(paths.run(site, id), "run.json"), null))
    .filter((m): m is RunMeta => m !== null);
}

export function appendRunEvent(site: string, id: string, n: number, ev: RunEventInput): RunEvent {
  const full = { n, ts: nowIso(), ...ev } as RunEvent;
  appendLine(join(paths.run(site, id), "events.jsonl"), full);
  return full;
}

export function readRunEvents(site: string, id: string, after = 0): RunEvent[] {
  return readJsonl<RunEvent>(join(paths.run(site, id), "events.jsonl")).filter(e => e.n > after);
}

export function saveRunFile(site: string, id: string, name: string, data: string, encoding: "utf8" | "base64"): number {
  if (!isValidFileName(name) || ["run.json", "events.jsonl", "script.js", "result.json"].includes(name)) {
    throw new BadRequest(`invalid file name ${JSON.stringify(name)}`);
  }
  const buf = Buffer.from(data, encoding);
  const path = join(paths.run(site, id), name);
  mkdirSync(dirname(path), { recursive: true });
  writeFileSync(path, buf);
  return buf.length;
}

/** A run file by name: a top-level file, or a path below files/ (downloads). */
export function runFilePath(site: string, id: string, name: string): string {
  if (!isValidRunId(id)) throw new NotFound("invalid path");
  const rel = name.startsWith("files/") ? `files/${safeRelPath(name.slice(6))}` : name;
  if (rel !== name || (!name.startsWith("files/") && !isValidFileName(name))) throw new NotFound("invalid path");
  const path = join(paths.run(site, id), rel);
  if (!existsSync(path)) throw new NotFound(`no file ${name} in run ${id}`);
  return path;
}

/** Every file in a run folder, downloads as files/<path>. */
export function listRunFiles(site: string, id: string): { name: string; bytes: number }[] {
  const root = paths.run(site, id);
  const walk = (dir: string): { name: string; bytes: number }[] => {
    try {
      return readdirSync(dir, { withFileTypes: true }).flatMap(d => {
        const p = join(dir, d.name);
        if (d.isDirectory()) return walk(p);
        return d.isFile() ? [{ name: relative(root, p).split(sep).join("/"), bytes: statSync(p).size }] : [];
      });
    } catch {
      return [];
    }
  };
  return walk(root).sort((a, b) => a.name.localeCompare(b.name));
}

/**
 * Move a file Firefox downloaded (into the downloads folder) to the run's
 * files/<rel>. Returns the run-relative name and size.
 */
export function adoptDownload(site: string, id: string, from: string, rel: string): { file: string; bytes: number } {
  const src = resolve(from);
  if (!src.startsWith(resolve(paths.downloads()) + sep)) throw new BadRequest(`refusing to adopt ${from}: not in ${paths.downloads()}`);
  const clean = safeRelPath(rel);
  if (!clean) throw new BadRequest(`invalid download path ${JSON.stringify(rel)}`);
  const dest = join(paths.run(site, id), "files", clean);
  mkdirSync(dirname(dest), { recursive: true });
  try {
    renameSync(src, dest);
  } catch (e: any) {
    if (e?.code !== "EXDEV") throw e;
    copyFileSync(src, dest);
    unlinkSync(src);
  }
  return { file: `files/${clean}`, bytes: statSync(dest).size };
}

// ---------------------------------------------------------------- script state (spoor.state)

function stateFile(site: string, script: string): string {
  const name = script === "(inline)" ? "inline" : script.replace(/\.js$/, "");
  if (!isValidScriptName(name)) throw new BadRequest(`invalid script name ${JSON.stringify(script)}`);
  return paths.state(site, name);
}

export function readScriptState(site: string, script: string): Record<string, unknown> {
  return readJson<Record<string, unknown>>(stateFile(site, script), {});
}

export function writeScriptState(site: string, script: string, key: string, value: unknown): void {
  const all = readScriptState(site, script);
  if (value === undefined || value === null) delete all[key];
  else all[key] = value;
  writeJson(stateFile(site, script), all);
}

// ---------------------------------------------------------------- session state

export function readSession(site: string): SessionState {
  return readJson<SessionState>(paths.session(site), { state: "unknown" });
}

/** Record a check result; returns the previous state. */
export function writeSession(site: string, next: Omit<SessionState, "changed_at" | "checked_at">): SessionState {
  const prev = readSession(site);
  const now = nowIso();
  const out: SessionState = { ...next, checked_at: now, changed_at: prev.state === next.state ? prev.changed_at ?? now : now };
  if (!out.error) delete out.error;
  writeJson(paths.session(site), out);
  return prev;
}

// ---------------------------------------------------------------- recordings

export function writeRecordingMeta(meta: RecordingMeta): void {
  writeJson(join(paths.recording(meta.site, meta.id), "meta.json"), meta);
}

export function appendFlow(site: string, id: string, flow: Flow): void {
  appendLine(join(paths.recording(site, id), "flows.jsonl"), flow);
}

export function writeRecordingFile(site: string, id: string, name: string, value: unknown): void {
  writeJson(join(paths.recording(site, id), name), value);
}

export interface RecordingRef {
  site: string;
  id: string;
  dir: string;
  meta: RecordingMeta;
}

export function listRecordings(site?: string): RecordingRef[] {
  const sites = site ? [site] : listDirs(join(spoorHome(), "sites"));
  const out: RecordingRef[] = [];
  for (const s of sites) {
    for (const id of listDirs(paths.recordings(s))) {
      const dir = paths.recording(s, id);
      const meta = readJson<RecordingMeta | null>(join(dir, "meta.json"), null);
      if (meta) out.push({ site: s, id, dir, meta });
    }
  }
  return out.sort((a, b) => b.meta.started.localeCompare(a.meta.started));
}

/** Resolve a recording by id (any site), "latest", or "<site>:latest". */
export function findRecording(ref: string): RecordingRef {
  const [maybeSite, rest] = ref.includes(":") ? ref.split(":", 2) as [string, string] : [undefined, ref];
  const all = listRecordings(maybeSite);
  const hit = rest === "latest" ? all[0] : all.find(r => r.id === rest || r.id.startsWith(rest));
  if (!hit) throw new NotFound(`no recording ${JSON.stringify(ref)} — see \`spoor recordings\``);
  return hit;
}

export function readFlows(rec: RecordingRef): Flow[] {
  return readJsonl<Flow>(join(rec.dir, "flows.jsonl"));
}

export function readRecordingFile<T>(rec: RecordingRef, name: string, fallback: T): T {
  return readJson<T>(join(rec.dir, name), fallback);
}

// ---------------------------------------------------------------- audit

export function audit(site: string, entry: Record<string, unknown>): void {
  try {
    appendLine(paths.audit(site), { ts: nowIso(), ...entry });
  } catch (e) {
    // best effort: an audit write failure is logged, never fatal to a run
    console.error(`audit write failed for ${site}:`, e);
  }
}

export function readAudit(site: string, tail = 100): unknown[] {
  return readJsonl<unknown>(paths.audit(site)).slice(-tail);
}
