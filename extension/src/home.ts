// The home panel: an extension page that opens when Spoor's Firefox starts.
// Overview across sites; per site the notes, script library, run history and
// recordings. Everything goes through the same API the agent uses.

import { ago, api, bridgeBanner, connect, h, mount, parseArgs, pill, pretty, statusKind, ui, type UiState } from "./ui.ts";

type Tab = "overview" | "scripts" | "runs" | "recordings";

interface SiteDetail {
  site: { name: string; url: string; check?: string; min_gap_ms?: number };
  dir: string;
  notes: string;
  scripts: { name: string; bytes: number; modified: string; description?: string }[];
  runs: { id: string; script: string; status: string; started: string; duration_ms?: number; requests?: number; error?: string; files?: string[]; args: unknown }[];
  recordings: { id: string; started: string; stopped?: string; flows: number }[];
  dossier: string[];
  browser: { open_tabs: number; recording?: string; active_run?: string } | null;
}

const root = document.getElementById("app")!;
let state: UiState | null = null;
let sites: { name: string; url: string }[] = [];
let detail: SiteDetail | null = null;
let error = "";
let notice = "";

const view = {
  site: location.hash.slice(1) || undefined as string | undefined,
  tab: "overview" as Tab,
  script: undefined as string | undefined,
  scriptCode: "",
  scriptDirty: false,
  newScript: false,
  args: "",
  notes: "",
  notesDirty: false,
  run: undefined as string | undefined,
  runData: null as null | { meta: any; files: { name: string; bytes: number }[]; events: any[] },
  file: null as null | { name: string; text: string },
  recording: undefined as string | undefined,
  recView: "flows" as "flows" | "auth" | "lifetimes" | "endpoints",
  recData: null as unknown,
  grep: "",
  addName: "",
  addUrl: "",
};

async function act(f: () => Promise<unknown>, ok?: string): Promise<void> {
  error = "";
  notice = "";
  try {
    await f();
    if (ok) notice = ok;
  } catch (e) {
    error = (e as Error).message;
  }
  render();
}

async function loadSites(): Promise<void> {
  sites = await api("GET", "/sites");
  if (!view.site && sites[0]) view.site = sites[0].name;
}

async function loadDetail(): Promise<void> {
  if (!view.site) {
    detail = null;
    return;
  }
  detail = await api("GET", `/sites/${view.site}`);
  if (!view.notesDirty) view.notes = detail!.notes;
}

async function refresh(): Promise<void> {
  try {
    await loadSites();
    await loadDetail();
    if (view.run) await loadRun();
  } catch (e) {
    error = (e as Error).message;
  }
  render();
}

async function selectSite(name: string): Promise<void> {
  Object.assign(view, { site: name, tab: "overview", script: undefined, run: undefined, recording: undefined, runData: null, recData: null, notesDirty: false, file: null });
  location.hash = name;
  await act(loadDetail);
}

async function openScript(name: string): Promise<void> {
  const r = await api("GET", `/sites/${view.site}/scripts/${name}`);
  Object.assign(view, { script: name, scriptCode: r.code, scriptDirty: false, newScript: false });
}

async function loadRun(): Promise<void> {
  if (!view.run || !view.site) return;
  const [info, ev] = await Promise.all([
    api("GET", `/sites/${view.site}/runs/${view.run}`),
    api("GET", `/sites/${view.site}/runs/${view.run}/events?after=0`),
  ]);
  view.runData = { meta: info.meta, files: info.files, events: ev.events };
}

async function loadRecording(): Promise<void> {
  if (!view.recording) return;
  const ref = encodeURIComponent(`${view.site}:${view.recording}`);
  const path = view.recView === "flows" ? `/recordings/${ref}/flows${view.grep ? `?grep=${encodeURIComponent(view.grep)}` : ""}` : `/recordings/${ref}/${view.recView}`;
  view.recData = await api("GET", path);
}

// ------------------------------------------------------------------ views

function siteList(): HTMLElement {
  const s = state!;
  return h("nav", { class: "sites" },
    h("div", { class: "muted small label" }, "Sites"),
    sites.map(x =>
      h("button", { class: `site ${x.name === view.site ? "active" : ""}`, onclick: () => selectSite(x.name) },
        h("span", { class: "grow" }, x.name),
        s.recordings[x.name] ? pill("●", "bad") : null,
        s.runs.some(r => r.site === x.name) ? pill("run", "busy") : null,
      ),
    ),
    h("form", { class: "add", onsubmit: (e: Event) => { e.preventDefault(); void addSite(); } },
      h("div", { class: "muted small label" }, "Add site"),
      h("input", { "data-key": "addName", placeholder: "name (e.g. cas)", value: view.addName, oninput: (e: Event) => (view.addName = (e.target as HTMLInputElement).value) }),
      h("input", { "data-key": "addUrl", placeholder: "https://…", value: view.addUrl, oninput: (e: Event) => (view.addUrl = (e.target as HTMLInputElement).value) }),
      h("button", { type: "submit" }, "Add"),
    ),
  );
}

async function addSite(): Promise<void> {
  await act(async () => {
    const s = await api("POST", "/sites", { name: view.addName.trim(), url: view.addUrl.trim() });
    view.addName = view.addUrl = "";
    await loadSites();
    await selectSite(s.name);
  });
}

function tabs(): HTMLElement {
  const t = (id: Tab, label: string) => h("button", { class: `tab ${view.tab === id ? "active" : ""}`, onclick: () => { view.tab = id; render(); } }, label);
  return h("div", { class: "tabs" }, t("overview", "Overview"), t("scripts", `Scripts (${detail!.scripts.length})`), t("runs", "Runs"), t("recordings", `Recordings (${detail!.recordings.length})`));
}

function overview(d: SiteDetail): HTMLElement {
  const rec = state!.recordings[d.site.name];
  return h("div", { class: "stack" },
    h("div", { class: "row wrap" },
      h("button", { class: "primary", onclick: () => ui({ ui: "open", site: d.site.name, url: d.site.url }) }, "Open site"),
      rec
        ? h("button", { class: "danger", onclick: () => act(() => api("POST", `/sites/${d.site.name}/record/stop`).then(loadDetail), "Recording saved") }, `Stop recording (${rec.flows})`)
        : h("button", { onclick: () => act(() => api("POST", `/sites/${d.site.name}/record/start`)) }, "Record"),
      h("button", { onclick: () => act(async () => { const st = await api("GET", `/sites/${d.site.name}/status`); notice = st.check ? (st.logged_in ? "Logged in ✓" : `Not logged in${st.check_error ? `: ${st.check_error}` : ""}`) : "No check script set for this site"; }) }, "Check login"),
      h("button", { onclick: () => act(() => api("POST", `/sites/${d.site.name}/stop`), "Stopped") }, "Kill switch"),
    ),
    h("dl", { class: "facts" },
      h("dt", null, "URL"), h("dd", null, d.site.url),
      h("dt", null, "Folder"), h("dd", null, h("code", null, d.dir)),
      h("dt", null, "Pacing"), h("dd", null, `${d.site.min_gap_ms ?? 1000} ms between requests`),
      h("dt", null, "Check script"), h("dd", null, d.site.check ?? "—"),
      h("dt", null, "Open tabs"), h("dd", null, d.browser ? String(d.browser.open_tabs) : "—"),
      h("dt", null, "Dossier"), h("dd", null, d.dossier.length ? d.dossier.join(", ") : "—"),
    ),
    h("div", { class: "row" }, h("b", { class: "grow" }, "Notes"), h("span", { class: "muted small" }, "shared with the agent (notes.md)"),
      h("button", { disabled: !view.notesDirty, onclick: () => act(async () => { await api("PUT", `/sites/${d.site.name}/notes`, { text: view.notes }); view.notesDirty = false; }, "Notes saved") }, "Save")),
    h("textarea", { "data-key": "notes", class: "notes", value: view.notes, oninput: (e: Event) => { view.notes = (e.target as HTMLTextAreaElement).value; if (!view.notesDirty) { view.notesDirty = true; render(); } } }),
  );
}

function scriptsView(d: SiteDetail): HTMLElement {
  const list = h("ul", { class: "list" },
    d.scripts.map(s => h("li", { class: view.script === s.name ? "active" : "", onclick: () => act(() => openScript(s.name)) },
      h("b", null, s.name), h("div", { class: "muted small" }, s.description ?? "", " · ", ago(s.modified)))),
    h("li", { onclick: () => { Object.assign(view, { script: "", scriptCode: "// what this script does\n\nreturn document.title;\n", newScript: true, scriptDirty: true }); render(); } }, h("span", { class: "muted" }, "+ New script")),
  );
  if (view.script === undefined) return h("div", { class: "split" }, list, h("p", { class: "muted" }, "Pick a script. Scripts are the body of an async function: `args`, `fetch` (the page's) and `spoor` are in scope."));
  const editor = h("div", { class: "stack" },
    view.newScript
      ? h("input", { "data-key": "scriptName", placeholder: "script name", value: view.script, oninput: (e: Event) => (view.script = (e.target as HTMLInputElement).value) })
      : h("div", { class: "row" }, h("b", { class: "grow" }, view.script), h("button", { class: "link danger", onclick: () => act(async () => { if (!confirm(`Delete ${view.script}?`)) return; await api("DELETE", `/sites/${d.site.name}/scripts/${view.script}`); view.script = undefined; await loadDetail(); }) }, "Delete")),
    h("textarea", { "data-key": "code", class: "code", spellcheck: "false", value: view.scriptCode, oninput: (e: Event) => { view.scriptCode = (e.target as HTMLTextAreaElement).value; if (!view.scriptDirty) { view.scriptDirty = true; render(); } } }),
    h("div", { class: "row" },
      h("button", { disabled: !view.scriptDirty, onclick: () => act(async () => { await api("PUT", `/sites/${d.site.name}/scripts/${view.script}`, { code: view.scriptCode }); view.scriptDirty = false; view.newScript = false; await loadDetail(); }, "Saved") }, "Save"),
    ),
    h("label", { class: "muted small" }, "Arguments: one key=value per line, or a JSON object"),
    h("textarea", { "data-key": "args", class: "args", placeholder: "domain=example.ch\ndryRun=true", value: view.args, oninput: (e: Event) => (view.args = (e.target as HTMLTextAreaElement).value) }),
    h("button", { class: "primary", disabled: view.scriptDirty || view.newScript, onclick: () => act(async () => {
      const meta = await api("POST", `/sites/${d.site.name}/runs`, { script: view.script, args: parseArgs(view.args) });
      Object.assign(view, { tab: "runs", run: meta.id, file: null });
      await loadDetail();
      await loadRun();
    }) }, "Run"),
  );
  return h("div", { class: "split" }, list, editor);
}

function eventLine(e: any): string {
  const t = new Date(e.ts).toLocaleTimeString();
  switch (e.type) {
    case "log": return `${t} ${e.level === "log" ? "" : `[${e.level}] `}${e.text}`;
    case "status": return `${t} ── ${e.status}${e.error ? `: ${e.error}` : ""}`;
    case "progress": return `${t} ── progress ${e.progress.done}${e.progress.total ? `/${e.progress.total}` : ""} ${e.progress.label ?? ""}`;
    case "confirm": return `${t} ── asks: ${e.message}`;
    case "confirmed": return `${t} ── ${e.approved ? "approved" : "rejected"}`;
    case "saved": return `${t} ── saved ${e.file} (${e.bytes} bytes)`;
    case "result": return `${t} ── result: ${e.ok ? pretty(e.result).slice(0, 2000) : e.error}`;
  }
  return `${t} ${JSON.stringify(e)}`;
}

function runsView(d: SiteDetail): HTMLElement {
  const list = h("ul", { class: "list" }, d.runs.map(r =>
    h("li", { class: view.run === r.id ? "active" : "", onclick: () => act(async () => { view.run = r.id; view.file = null; await loadRun(); }) },
      h("div", { class: "row" }, pill(r.status, statusKind(r.status)), h("b", { class: "grow" }, r.script), h("span", { class: "muted small" }, ago(r.started))),
      r.error ? h("div", { class: "small bad-text ellipsis" }, r.error) : null,
    )));
  if (!view.run || !view.runData) return h("div", { class: "split" }, list, h("p", { class: "muted" }, d.runs.length ? "Pick a run." : "No runs yet."));
  const { meta, files, events } = view.runData;
  const active = ["queued", "running", "paused", "waiting"].includes(meta.status);
  const detailEl = h("div", { class: "stack" },
    h("div", { class: "row" }, pill(meta.status, statusKind(meta.status)), h("b", { class: "grow" }, `${meta.script} · ${meta.id}`),
      active ? h("button", { class: "danger", onclick: () => act(() => api("POST", `/sites/${d.site.name}/runs/${meta.id}/stop`)) }, "Stop") : null,
      !active && meta.script !== "(inline)" ? h("button", { onclick: () => act(async () => { const m = await api("POST", `/sites/${d.site.name}/runs`, { script: meta.script, args: meta.args }); view.run = m.id; await loadDetail(); await loadRun(); }) }, "Run again") : null,
    ),
    h("div", { class: "muted small" }, `args ${JSON.stringify(meta.args)} · ${meta.requests ?? 0} requests${meta.duration_ms ? ` · ${(meta.duration_ms / 1000).toFixed(1)} s` : ""}`),
    h("pre", { class: "log tall", "data-key": "runlog" }, events.map(eventLine).join("\n")),
    h("div", { class: "row wrap" }, h("span", { class: "muted small" }, "Files:"),
      files.map(f => h("button", { class: "link", onclick: () => act(async () => { const r = await api("GET", `/sites/${d.site.name}/runs/${meta.id}/files/${f.name}`); view.file = { name: f.name, text: r.text ?? (r.too_large ? `(${r.bytes} bytes — too large to show here)` : `(binary, ${r.bytes} bytes)`) }; }) }, `${f.name} (${f.bytes})`))),
    view.file ? h("div", { class: "stack" }, h("b", null, view.file.name), h("pre", { class: "log tall" }, view.file.text)) : null,
  );
  return h("div", { class: "split" }, list, detailEl);
}

function recordingsView(d: SiteDetail): HTMLElement {
  const list = h("ul", { class: "list" }, d.recordings.map(r =>
    h("li", { class: view.recording === r.id ? "active" : "", onclick: () => act(async () => { view.recording = r.id; view.recView = "flows"; await loadRecording(); }) },
      h("b", null, r.id), h("div", { class: "muted small" }, `${r.flows} flows · ${ago(r.started)}${r.stopped ? "" : " · recording"}`))));
  if (!view.recording) return h("div", { class: "split" }, list, h("p", { class: "muted" }, d.recordings.length ? "Pick a recording." : "No recordings yet. Press Record on the overview, use the site, then stop."));
  const t = (id: typeof view.recView, label: string) => h("button", { class: `tab ${view.recView === id ? "active" : ""}`, onclick: () => act(async () => { view.recView = id; await loadRecording(); }) }, label);
  let body: HTMLElement;
  const data = view.recData as any;
  if (view.recView === "flows") {
    body = h("div", { class: "stack" },
      h("form", { class: "row", onsubmit: (e: Event) => { e.preventDefault(); void act(loadRecording); } },
        h("input", { "data-key": "grep", class: "grow", placeholder: "grep: url, headers or bodies containing…", value: view.grep, oninput: (e: Event) => (view.grep = (e.target as HTMLInputElement).value) }),
        h("button", { type: "submit" }, "Filter")),
      h("pre", { class: "log tall" }, Array.isArray(data) ? data.join("\n") : ""));
  } else {
    body = h("pre", { class: "log tall" }, pretty(data));
  }
  return h("div", { class: "split" }, list, h("div", { class: "stack" },
    h("div", { class: "row" }, h("div", { class: "tabs grow" }, t("flows", "Flows"), t("auth", "Auth chain"), t("lifetimes", "Lifetimes"), t("endpoints", "Endpoints")),
      h("button", { onclick: () => act(async () => { await api("POST", `/sites/${d.site.name}/dossier`, { recording: `${d.site.name}:${view.recording}` }); await loadDetail(); }, "Dossier written") }, "Write dossier")),
    body,
    h("p", { class: "muted small" }, `CLI: spoor flows ${d.site.name}:${view.recording} · spoor trace … · spoor replay … <seq>`),
  ));
}

function render(): void {
  if (!state) return mount(root, h("p", { class: "muted pad" }, "Loading…"));
  const d = detail;
  mount(root,
    h("header", { class: "top row" },
      h("b", { class: "brand grow" }, "Spoor"),
      state.bridge === "connected" ? pill("service connected", "good") : pill(state.bridge, state.bridge === "down" ? "bad" : "warn"),
    ),
    bridgeBanner(state),
    state.confirms.length ? h("div", { class: "banner warn" }, `${state.confirms.length} approval(s) waiting — see the sidebar (View → Sidebar → Spoor).`) : null,
    h("div", { class: "layout" },
      siteList(),
      h("main", null,
        error ? h("div", { class: "banner bad" }, error) : null,
        notice ? h("div", { class: "banner good" }, notice) : null,
        !d
          ? h("div", { class: "empty" }, h("h2", null, "Welcome"), h("p", null, "Add a site on the left. Then open it, log in by hand, and Spoor keeps that session in the site's own container."))
          : h("div", { class: "stack" },
              h("div", { class: "row" }, h("h2", { class: "grow" }, d.site.name), h("a", { href: d.site.url, class: "muted small", target: "_blank" }, d.site.url)),
              tabs(),
              view.tab === "overview" ? overview(d) : view.tab === "scripts" ? scriptsView(d) : view.tab === "runs" ? runsView(d) : recordingsView(d),
            ),
      ),
    ),
  );
}

let polling = false;
async function pollRun(): Promise<void> {
  if (polling) return;
  polling = true;
  try {
    while (view.run && view.runData && ["queued", "running", "paused", "waiting"].includes(view.runData.meta.status) && view.tab === "runs") {
      await new Promise(r => setTimeout(r, 1000));
      await loadRun();
      render();
    }
  } finally {
    polling = false;
  }
}

connect(p => {
  const wasConnected = state?.bridge === "connected";
  state = p.state;
  if ((!wasConnected && state.bridge === "connected") || (p.changed !== undefined && (p.changed === null || p.changed === view.site))) {
    void refresh().then(() => void pollRun());
  } else if (!p.tabChanged) {
    render();
  }
  void pollRun();
});
