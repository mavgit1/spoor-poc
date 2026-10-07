// Where Spoor keeps its state. Everything lives under one directory:
//
//   $SPOOR_HOME/                 (default ~/.spoor)
//     sites.json                 site registry (hand-editable)
//     serve.json                 url + bearer token of the running service
//     firefox/                   Spoor's Firefox profile (one container per site)
//     spoor.xpi                  signed extension, installed into the profile
//     logs/service.log
//     sites/<name>/
//       notes.md                 shared notes: human and agent both edit
//       scripts/<script>.js      script library
//       runs/<id>/               run.json, events.jsonl, script.js, saved files
//       recordings/<id>/         meta.json, flows.jsonl, cookies.json, storage.json
//       dossier/                 what `spoor dossier` and `spoor replay` write
//       audit.jsonl              every request a run made

import { homedir } from "node:os";
import { join } from "node:path";

export function spoorHome(): string {
  const env = process.env.SPOOR_HOME;
  return env && env.length > 0 ? env : join(homedir(), ".spoor");
}

export const paths = {
  sites: () => join(spoorHome(), "sites.json"),
  serve: () => join(spoorHome(), "serve.json"),
  profile: () => join(spoorHome(), "firefox"),
  xpi: () => join(spoorHome(), "spoor.xpi"),
  logs: () => join(spoorHome(), "logs"),
  site: (site: string) => join(spoorHome(), "sites", site),
  notes: (site: string) => join(spoorHome(), "sites", site, "notes.md"),
  scripts: (site: string) => join(spoorHome(), "sites", site, "scripts"),
  runs: (site: string) => join(spoorHome(), "sites", site, "runs"),
  run: (site: string, id: string) => join(spoorHome(), "sites", site, "runs", id),
  recordings: (site: string) => join(spoorHome(), "sites", site, "recordings"),
  recording: (site: string, id: string) => join(spoorHome(), "sites", site, "recordings", id),
  dossier: (site: string) => join(spoorHome(), "sites", site, "dossier"),
  audit: (site: string) => join(spoorHome(), "sites", site, "audit.jsonl"),
};

export const DEFAULT_PORT = 7517;
export const DEFAULT_MIN_GAP_MS = 1000;
export const DEFAULT_TIMEOUT_SECS = 300;

/** Site, script and file names become path segments; keep them to one safe segment. */
export function isValidName(name: string): boolean {
  return /^[a-z0-9][a-z0-9_-]{0,63}$/.test(name);
}

/** Script names may carry a `.js` suffix on disk; the name itself is a safe segment. */
export function isValidScriptName(name: string): boolean {
  return /^[A-Za-z0-9][A-Za-z0-9_.-]{0,99}$/.test(name) && !name.includes("..");
}

/** Files a run saves. Same rule as scripts. */
export const isValidFileName = isValidScriptName;

export function nowIso(): string {
  return new Date().toISOString();
}

/** Sortable id: 20261002-143012-ab12 */
export function newId(): string {
  const d = new Date();
  const p = (n: number, w = 2) => String(n).padStart(w, "0");
  const stamp = `${d.getFullYear()}${p(d.getMonth() + 1)}${p(d.getDate())}-${p(d.getHours())}${p(d.getMinutes())}${p(d.getSeconds())}`;
  return `${stamp}-${Math.random().toString(16).slice(2, 6)}`;
}
