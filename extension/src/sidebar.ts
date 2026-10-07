// The essentials panel: browser sidebar, next to every page but outside it,
// so the site can't see it. Shows what Spoor is doing right now, asks for
// approvals, and records.

import { api, bridgeBanner, connect, h, mount, pill, pretty, statusKind, ui, type UiState } from "./ui.ts";

const root = document.getElementById("app")!;
let state: UiState | null = null;
let windowId: number | undefined;
let current: { site?: string; url?: string } = {};
let sites: { name: string; url: string }[] = [];
let markText = "";
let error = "";

async function refreshContext(): Promise<void> {
  windowId ??= (await browser.windows.getCurrent()).id;
  current = await ui({ ui: "siteForTab", windowId });
  render();
}

async function refreshSites(): Promise<void> {
  try {
    sites = await api("GET", "/sites");
  } catch {
    sites = [];
  }
  render();
}

async function act(f: () => Promise<unknown>): Promise<void> {
  error = "";
  try {
    await f();
  } catch (e) {
    error = (e as Error).message;
  }
  render();
}

function confirmCard(c: UiState["confirms"][number]): HTMLElement {
  return h("section", { class: "card confirm" },
    h("div", { class: "row" }, pill("approval", "warn"), h("b", null, c.site)),
    h("p", { class: "message" }, c.message),
    c.detail !== undefined ? h("details", { open: true }, h("summary", null, "Details"), h("pre", null, pretty(c.detail))) : null,
    h("div", { class: "row" },
      h("button", { class: "primary", onclick: () => ui({ ui: "confirm", id: c.id, approved: true }) }, "Approve"),
      h("button", { onclick: () => ui({ ui: "confirm", id: c.id, approved: false }) }, "Reject"),
    ),
  );
}

function runCard(r: UiState["runs"][number]): HTMLElement {
  const p = r.progress;
  const pct = p?.total ? Math.min(100, Math.round((p.done / p.total) * 100)) : undefined;
  const control = (action: string) => () => act(() => ui({ ui: "run.control", run_id: r.id, site: r.site, action }));
  return h("section", { class: "card" },
    h("div", { class: "row" }, pill(r.status, statusKind(r.status)), h("b", null, r.site), h("span", { class: "grow ellipsis", title: r.id }, r.script), h("span", { class: "muted small" }, `${r.requests} req`)),
    p ? h("div", { class: "progress" },
      h("div", { class: "bar" }, h("div", { class: "fill", style: `width:${pct ?? 100}%` + (pct === undefined ? ";opacity:.3" : "") })),
      h("div", { class: "small" }, `${p.done}${p.total ? ` / ${p.total}` : ""}${p.label ? ` · ${p.label}` : ""}`),
    ) : null,
    r.log.length ? h("pre", { class: "log" }, r.log.slice(-12).map(l => `${l.level === "log" ? "" : `[${l.level}] `}${l.text}`).join("\n")) : null,
    h("div", { class: "row" },
      r.status === "paused" ? h("button", { onclick: control("resume") }, "Resume") : h("button", { onclick: control("pause") }, "Pause"),
      h("button", { class: "danger", onclick: control("stop") }, "Stop"),
    ),
  );
}

function recordingCard(site: string, s: UiState): HTMLElement {
  const rec = s.recordings[site];
  return h("section", { class: "card" },
    h("div", { class: "row" }, h("b", { class: "grow" }, "Recording"), rec ? pill(`● ${rec.flows} requests`, "bad") : pill("off")),
    rec
      ? [
          h("div", { class: "row" },
            h("input", { "data-key": "mark", class: "grow", placeholder: "Mark this step, e.g. “add DNS record”", value: markText, oninput: (e: Event) => (markText = (e.target as HTMLInputElement).value), onkeydown: (e: KeyboardEvent) => { if (e.key === "Enter") void mark(site); } }),
            h("button", { onclick: () => mark(site) }, "Mark"),
          ),
          h("button", { class: "danger wide", onclick: () => act(() => api("POST", `/sites/${site}/record/stop`)) }, "Stop recording"),
        ]
      : h("button", { class: "wide", onclick: () => act(() => api("POST", `/sites/${site}/record/start`, { open: false })) }, "Start recording"),
  );
}

async function mark(site: string): Promise<void> {
  const label = markText.trim();
  if (!label) return;
  markText = "";
  await act(() => api("POST", `/sites/${site}/record/mark`, { label }));
}

function render(): void {
  if (!state) return mount(root, h("p", { class: "muted" }, "Loading…"));
  const s = state;
  const site = current.site;
  const confirms = s.confirms;
  const runs = s.runs;
  mount(root,
    h("header", { class: "row" },
      h("b", { class: "grow" }, "Spoor"),
      s.bridge === "connected" ? pill("connected", "good") : pill(s.bridge, s.bridge === "down" ? "bad" : "warn"),
      h("button", { class: "link", onclick: () => ui({ ui: "home" }) }, "Home"),
    ),
    bridgeBanner(s),
    error ? h("div", { class: "banner bad" }, error) : null,
    confirms.map(confirmCard),
    h("section", { class: "card" },
      site
        ? h("div", null, h("div", { class: "muted small" }, "This tab belongs to"), h("div", { class: "site-name" }, site))
        : h("div", null,
            h("div", { class: "muted small" }, "This tab is not in a site container."),
            sites.length ? h("div", { class: "row wrap" }, sites.map(x => h("button", { onclick: () => ui({ ui: "open", site: x.name, url: x.url }) }, `Open ${x.name}`))) : null,
          ),
    ),
    runs.length ? runs.map(runCard) : h("p", { class: "muted small" }, "Nothing running."),
    site ? recordingCard(site, s) : null,
    Object.keys(s.recordings).filter(x => x !== site).map(x => h("p", { class: "small" }, pill("● rec", "bad"), ` recording ${x} (${s.recordings[x]!.flows})`)),
    s.recent.length ? h("section", { class: "card" },
      h("b", null, "Recent runs"),
      h("ul", { class: "plain" }, s.recent.slice(0, 8).map(r => h("li", { class: "row" }, pill(r.status, statusKind(r.status)), h("span", { class: "grow ellipsis", title: r.error ?? "" }, `${r.site} · ${r.script}${r.error ? ` — ${r.error}` : ""}`)))),
    ) : null,
  );
}

connect(p => {
  const firstConnect = state?.bridge !== "connected" && p.state.bridge === "connected";
  state = p.state;
  if (p.tabChanged) void refreshContext();
  if (firstConnect || p.changed !== undefined) void refreshSites();
  render();
});
void refreshContext();
