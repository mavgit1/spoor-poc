// Spoor's background script. Everything Spoor does in the browser happens
// here, through ordinary extension APIs (no automation protocol):
//
// - one Firefox container per site: its own cookie store, so each site keeps
//   its own login; the human logs in by hand in a tab of that container
// - one hidden worker tab per site where runs execute, as content scripts
//   using content.fetch (requests carry the page's cookies and origin); a run
//   can open more hidden tabs in the same container for other origins
//   (spoor.tab) and download files straight to disk (spoor.download)
// - sessions: clearing a site's container, bringing a login tab forward
// - pacing and pause/stop through a gate every script request passes, an
//   audit of every request the worker tab makes (webRequest)
// - recording: every request in the site's container, with headers, cookies,
//   Set-Cookie, redirects and bodies (filterResponseData), across all hosts
// - the native messaging port to the local service, and the state the home
//   panel and sidebar render

import { ChunkAssembler, isChunk, RpcPeer } from "../../src/shared/rpc.ts";
import type { Body, BridgeMessage, CookieInfo, ExecParams, Flow, Progress, RunDone, RunEventInput, RunStatus, SiteBrowserState, StorageSnapshot } from "../../src/shared/types.ts";
import { safeRelPath } from "../../src/shared/relpath.ts";
import { buildWrapper } from "./wrapper.ts";
import { redactBody } from "./redact.ts";

const NATIVE_HOST = "spoor_bridge";
const VERSION = browser.runtime.getManifest().version;
const MAX_TEXT_BODY = 5 * 1024 * 1024;
const MAX_BINARY_BODY = 256 * 1024;
const LOG_TAIL = 300;

// ------------------------------------------------------------------ state

interface Confirm {
  id: string;
  run_id: string;
  site: string;
  message: string;
  detail?: unknown;
  resolve: (approved: boolean) => void;
}

interface Run {
  id: string;
  site: string;
  script: string;
  tabId: number;
  status: RunStatus;
  started: number;
  requests: number;
  minGap: number;
  lastRequest: number;
  gateChain: Promise<void>;
  progress?: Progress;
  log: { level: string; text: string }[];
  resumeWaiters: (() => void)[];
  timer?: ReturnType<typeof setTimeout>;
  /** Extra hidden tabs opened with spoor.tab(), same container. */
  auxTabs: number[];
  /** spoor.download() ids still in progress. */
  downloads: Set<number>;
}

interface Recording {
  rec_id: string;
  site: string;
  storeId: string;
  seq: number;
  flows: number;
  started: number;
  /** In-flight exchanges by webRequest requestId. */
  open: Map<string, PendingFlow>;
}

interface PendingFlow {
  flow: Flow;
  started: number;
  completed: boolean;
  /** True while a response filter is still collecting the body. */
  bodyPending: boolean;
}

const state = {
  bridge: "connecting" as "connecting" | "connected" | "down",
  bridgeError: undefined as string | undefined,
  runs: new Map<string, Run>(),
  confirms: new Map<string, Confirm>(),
  recordings: new Map<string, Recording>(),
  /** site → cookieStoreId */
  containers: new Map<string, string>(),
  workers: new Map<string, number>(),
  /** Finished runs, newest first, for the sidebar. */
  recent: [] as { id: string; site: string; script: string; status: RunStatus; error?: string }[],
};

// ------------------------------------------------------------------ bridge

let peer: RpcPeer | null = null;
let backoff = 1000;

function connectBridge(): void {
  state.bridge = "connecting";
  broadcast();
  let port: browser.runtime.Port;
  try {
    port = browser.runtime.connectNative(NATIVE_HOST);
  } catch (e) {
    bridgeDown(String(e));
    return;
  }
  const assembler = new ChunkAssembler();
  const p = new RpcPeer(msg => port.postMessage(msg), handlers, onServiceEvent);
  port.onMessage.addListener(async (raw: unknown) => {
    const msg = isChunk(raw) ? assembler.add(raw) : (raw as BridgeMessage);
    if (!msg) return;
    if (state.bridge !== "connected") {
      state.bridge = "connected";
      state.bridgeError = undefined;
      backoff = 1000;
      broadcast();
    }
    await p.receive(msg);
  });
  port.onDisconnect.addListener(() => {
    p.close("bridge closed");
    if (peer === p) peer = null;
    bridgeDown(port.error?.message ?? "native host exited");
  });
  peer = p;
  void browser.runtime.getBrowserInfo().then(info => p.emit("hello", { extension: VERSION, firefox: info.version }));
}

function bridgeDown(error: string): void {
  state.bridge = "down";
  state.bridgeError = error;
  // The service has lost track of these; don't leave them running unseen.
  for (const run of state.runs.values()) void finishRun(run, { ok: false, status: "error", error: "connection to the Spoor service was lost" });
  broadcast();
  setTimeout(connectBridge, backoff);
  backoff = Math.min(backoff * 2, 30_000);
}

function emit(name: string, data: unknown): void {
  try {
    peer?.emit(name, data);
  } catch {
    // port closed; onDisconnect handles it
  }
}

function runEvent(run: Run, ev: RunEventInput): void {
  emit("run.event", { run_id: run.id, ev });
}

function onServiceEvent(name: string, data: any): void {
  if (name === "changed") broadcast({ changed: data?.site ?? null });
  if (name === "background") void toBackground();
  if (name === "notify") notify(data);
  if (name === "host.error") {
    state.bridgeError = data?.error;
    broadcast();
  }
}

async function api(method: string, path: string, body?: unknown): Promise<any> {
  if (!peer) throw new Error(`not connected to the Spoor service (${state.bridgeError ?? "connecting"})`);
  return peer.call("api", { method, path, body }, 300_000);
}

// ------------------------------------------------------------------ containers & tabs

const COLORS = ["blue", "turquoise", "green", "yellow", "orange", "red", "pink", "purple"];

async function containerFor(site: string): Promise<string> {
  const known = state.containers.get(site);
  if (known) {
    try {
      await browser.contextualIdentities.get(known);
      return known;
    } catch {
      state.containers.delete(site);
    }
  }
  const existing = await browser.contextualIdentities.query({ name: site });
  const ci = existing[0] ?? (await browser.contextualIdentities.create({
    name: site,
    color: COLORS[[...site].reduce((a, c) => a + c.charCodeAt(0), 0) % COLORS.length]!,
    icon: "fingerprint",
  }));
  state.containers.set(site, ci.cookieStoreId);
  return ci.cookieStoreId;
}

async function loadContainers(): Promise<void> {
  try {
    for (const ci of await browser.contextualIdentities.query({})) state.containers.set(ci.name, ci.cookieStoreId);
  } catch (e) {
    console.error("containers unavailable — is privacy.userContext.enabled on?", e);
  }
}

function siteForStore(storeId: string | undefined): string | undefined {
  if (!storeId) return undefined;
  for (const [site, id] of state.containers) if (id === storeId) return site;
  return undefined;
}

/**
 * Wait until the tab has committed a navigation that started after `since`
 * and finished loading. Polls instead of trusting a single onUpdated event,
 * which is easy to miss when a page loads fast.
 */
async function waitForLoad(tabId: number, since: number, timeoutMs = 60_000): Promise<void> {
  const deadline = Date.now() + timeoutMs;
  let committed = false;
  const onCommitted = (d: { tabId: number; frameId: number; timeStamp: number }) => {
    if (d.tabId === tabId && d.frameId === 0 && d.timeStamp >= since - 50) committed = true;
  };
  browser.webNavigation.onCommitted.addListener(onCommitted);
  try {
    while (Date.now() < deadline) {
      let tab: browser.tabs.Tab;
      try {
        tab = await browser.tabs.get(tabId);
      } catch {
        throw new Error("the tab was closed while loading");
      }
      if (committed && tab.status === "complete") return;
      await new Promise(r => setTimeout(r, 100));
    }
    throw new Error(`page did not finish loading within ${timeoutMs / 1000}s`);
  } finally {
    browser.webNavigation.onCommitted.removeListener(onCommitted);
  }
}

/** The site's worker tab, hidden, loaded at `url`. Reused across runs. */
async function workerTab(site: string, url: string): Promise<number> {
  const storeId = await containerFor(site);
  const id = state.workers.get(site);
  if (id !== undefined) {
    try {
      const tab = await browser.tabs.get(id);
      if (tab.cookieStoreId === storeId) {
        const since = Date.now();
        await browser.tabs.update(id, { url });
        await waitForLoad(id, since);
        return id;
      }
    } catch {
      // gone
    }
    state.workers.delete(site);
  }
  const since = Date.now();
  const tab = await hiddenTab(storeId, url);
  state.workers.set(site, tab);
  await waitForLoad(tab, since);
  broadcast();
  return tab;
}

/** A hidden tab in a container, marked so a restored session doesn't bring it back. */
const createdThisSession = new Set<number>();
/** Main-frame URLs each hidden tab committed, in order (spoor.tab(...).urls()). */
const tabHistory = new Map<number, string[]>();

browser.webNavigation.onCommitted.addListener(d => {
  if (d.frameId === 0) tabHistory.get(d.tabId)?.push(d.url);
});

async function hiddenTab(storeId: string, url: string): Promise<number> {
  const tab = await browser.tabs.create({ url, cookieStoreId: storeId, active: false });
  createdThisSession.add(tab.id!);
  tabHistory.set(tab.id!, []);
  await browser.sessions.setTabValue(tab.id!, "spoor", "worker").catch(() => {});
  try {
    await browser.tabs.hide(tab.id!);
  } catch (e) {
    console.warn("could not hide the worker tab", e);
  }
  return tab.id!;
}

/**
 * Firefox restores the last session (that keeps session cookies alive), which
 * would bring back old worker tabs as visible tabs. Close those.
 */
async function sweepRestoredWorkers(): Promise<void> {
  for (const t of await browser.tabs.query({})) {
    if (t.id === undefined || createdThisSession.has(t.id)) continue;
    const mark = await browser.sessions.getTabValue(t.id, "spoor").catch(() => undefined);
    if (mark === "worker") await browser.tabs.remove(t.id).catch(() => {});
  }
}

// ------------------------------------------------------------------ runs

async function exec(p: ExecParams): Promise<{ accepted: true }> {
  for (const r of state.runs.values()) if (r.site === p.site) throw new Error(`site ${p.site} is busy with run ${r.id}`);
  const run: Run = {
    id: p.run_id,
    site: p.site,
    script: p.script,
    tabId: -1,
    status: "running",
    started: Date.now(),
    requests: 0,
    minGap: p.min_gap_ms,
    lastRequest: 0,
    gateChain: Promise.resolve(),
    log: [],
    resumeWaiters: [],
    auxTabs: [],
    downloads: new Set(),
  };
  state.runs.set(run.id, run);
  run.timer = setTimeout(() => void stopRun(run, "error", `timed out after ${Math.round(p.timeout_ms / 1000)} s`), p.timeout_ms);
  broadcast();
  void (async () => {
    try {
      const tabId = await workerTab(p.site, p.url ?? p.site_url);
      if (!state.runs.has(run.id)) return; // stopped while loading
      run.tabId = tabId;
      await browser.tabs.executeScript(tabId, { code: buildWrapper(p.run_id, p.args, p.code), runAt: "document_idle" });
    } catch (e) {
      await finishRun(run, { ok: false, status: "error", error: errorText(e) });
    }
  })();
  return { accepted: true };
}

function errorText(e: unknown): string {
  if (e instanceof Error) return e.message;
  return String((e as { message?: string })?.message ?? e);
}

/**
 * Finished runs stay attributable for a moment: a fetch can resolve in the
 * page before webRequest reports it completed, and those requests still
 * belong in the run's audit and count.
 */
const lingering = new Map<number, Run>();
const LINGER_MS = 2000;
const SETTLE_MS = 200;

async function finishRun(run: Run, out: { ok: boolean; status: RunStatus; result?: unknown; error?: string }): Promise<void> {
  if (!state.runs.has(run.id)) return;
  state.runs.delete(run.id);
  if (run.tabId >= 0) {
    lingering.set(run.tabId, run);
    setTimeout(() => {
      if (lingering.get(run.tabId) === run) lingering.delete(run.tabId);
    }, LINGER_MS);
  }
  clearTimeout(run.timer);
  for (const id of run.downloads) void browser.downloads.cancel(id).catch(() => {});
  if (run.auxTabs.length) setTimeout(() => void browser.tabs.remove(run.auxTabs).catch(() => {}), LINGER_MS);
  for (const c of state.confirms.values()) {
    if (c.run_id === run.id) {
      state.confirms.delete(c.id);
      c.resolve(false);
    }
  }
  for (const w of run.resumeWaiters.splice(0)) w();
  if (out.status === "ok") await new Promise(r => setTimeout(r, SETTLE_MS));
  const done: RunDone = { run_id: run.id, requests: run.requests, ...out };
  emit("run.done", done);
  state.recent.unshift({ id: run.id, site: run.site, script: run.script, status: out.status, error: out.error });
  state.recent.length = Math.min(state.recent.length, 20);
  broadcast();
}

/** Stop a run for real: its tab goes away, so the script's context is gone. */
async function stopRun(run: Run, status: RunStatus, error: string): Promise<void> {
  const tabId = run.tabId;
  await finishRun(run, { ok: false, status, error });
  if (tabId >= 0 && state.workers.get(run.site) === tabId) {
    state.workers.delete(run.site);
    await browser.tabs.remove(tabId).catch(() => {});
  }
}

function setPaused(run: Run, paused: boolean): void {
  if (paused && run.status === "running") {
    run.status = "paused";
    runEvent(run, { type: "status", status: "paused" });
  } else if (!paused && run.status === "paused") {
    run.status = "running";
    runEvent(run, { type: "status", status: "running" });
    for (const w of run.resumeWaiters.splice(0)) w();
  }
  broadcast();
}

async function whileActive(run: Run): Promise<void> {
  while (run.status === "paused") await new Promise<void>(r => run.resumeWaiters.push(r));
  if (!state.runs.has(run.id)) throw new Error("run stopped");
}

/** Every request a script makes waits here: pause, stop, and the minimum gap. */
function gate(run: Run): Promise<void> {
  const next = run.gateChain.then(async () => {
    await whileActive(run);
    const wait = run.lastRequest + run.minGap - Date.now();
    if (wait > 0) await new Promise(r => setTimeout(r, wait));
    await whileActive(run);
    run.lastRequest = Date.now();
  });
  run.gateChain = next.catch(() => {});
  return next;
}

function runForSender(msg: { spoor_run?: string }, sender: browser.runtime.MessageSender): Run | undefined {
  const run = msg.spoor_run ? state.runs.get(msg.spoor_run) : undefined;
  return run && sender.tab?.id === run.tabId ? run : undefined;
}

/** Messages from a running script (the wrapper in the worker tab). */
async function onScriptMessage(msg: any, run: Run): Promise<unknown> {
  const d = msg.data ?? {};
  switch (msg.kind) {
    case "gate":
      await gate(run);
      return true;
    case "log": {
      const entry = { level: String(d.level), text: String(d.text).slice(0, 20_000) };
      run.log.push(entry);
      if (run.log.length > LOG_TAIL) run.log.shift();
      runEvent(run, { type: "log", ...entry });
      broadcast();
      return true;
    }
    case "progress": {
      run.progress = { done: Number(d.done) || 0, total: d.total === undefined ? undefined : Number(d.total), label: d.label === undefined ? undefined : String(d.label) };
      runEvent(run, { type: "progress", progress: run.progress });
      broadcast();
      return true;
    }
    case "sleep":
      await new Promise(r => setTimeout(r, Math.max(0, Number(d.ms) || 0)));
      await whileActive(run);
      return true;
    case "checkpoint":
      await whileActive(run);
      return true;
    case "save":
      if (!peer) throw new Error("not connected to the Spoor service");
      return peer.call("run.save", { run_id: run.id, name: d.name, data: d.data, encoding: d.encoding }, 120_000);
    case "download":
      return download(run, d);
    case "state":
      if (!peer) throw new Error("not connected to the Spoor service");
      return peer.call("run.state", { run_id: run.id, op: d.op, key: d.key, value: d.value });
    case "tab.open": {
      const storeId = await containerFor(run.site);
      const since = Date.now();
      const tab = await hiddenTab(storeId, String(d.url));
      run.auxTabs.push(tab);
      await waitForLoad(tab, since);
      return { tab };
    }
    case "tab.fetch": {
      const tab = auxTab(run, d.tab);
      await gate(run);
      const [res] = await browser.tabs.executeScript(tab, { code: TAB_FETCH(String(d.url), d.init ?? {}) });
      return JSON.parse(String(res));
    }
    case "tab.eval": {
      const tab = auxTab(run, d.tab);
      const [res] = await browser.tabs.executeScript(tab, { code: `(async (args) => {\n${String(d.code)}\n})(${JSON.stringify(d.args ?? null)}).then(r => r === undefined ? null : JSON.parse(JSON.stringify(r)))` });
      return res;
    }
    case "tab.urls":
      return tabHistory.get(auxTab(run, d.tab)) ?? [];
    case "tab.close": {
      const tab = auxTab(run, d.tab);
      run.auxTabs = run.auxTabs.filter(t => t !== tab);
      await browser.tabs.remove(tab).catch(() => {});
      return true;
    }
    case "confirm":
      return { approved: await askConfirm(run, String(d.message ?? "Continue?"), d.detail) };
    case "done":
      await finishRun(run, d.ok ? { ok: true, status: "ok", result: d.result } : { ok: false, status: "error", error: String(d.error) });
      return true;
  }
  throw new Error(`unknown message ${msg.kind}`);
}

function auxTab(run: Run, id: unknown): number {
  const tab = Number(id);
  if (!run.auxTabs.includes(tab)) throw new Error("that tab is closed or not this run's");
  return tab;
}

/** Runs in an aux tab: the page's own fetch there, the response handed back as plain data. */
const TAB_FETCH = (url: string, init: unknown) => `(async () => {
  // Relative to where the tab is now (after redirects), not where it was opened.
  const r = await content.fetch(new URL(${JSON.stringify(url)}, location.href).href, ${JSON.stringify(init)});
  // Copy first: the page's buffer can't be sliced from here.
  const u8 = new Uint8Array(structuredClone(await r.arrayBuffer()));
  let s = "";
  for (let i = 0; i < u8.length; i += 0x8000) s += String.fromCharCode(...u8.subarray(i, i + 0x8000));
  // As a JSON string: objects built here don't survive being handed back as they are.
  const headers = [];
  r.headers.forEach((v, k) => headers.push([String(k), String(v)]));
  return JSON.stringify({ status: r.status, statusText: r.statusText, url: r.url, redirected: r.redirected, headers, body: btoa(s) });
})()`;

/**
 * spoor.download(): Firefox's download manager fetches the file inside the
 * site's container (its cookies) and writes it straight to disk; nothing
 * passes through memory or the bridge. The service then moves it from the
 * download folder into the run's files/.
 */
async function download(run: Run, d: { url: string; path?: string; dir?: string; method?: string; headers?: Record<string, string>; body?: string; referer?: string }): Promise<unknown> {
  if (!peer) throw new Error("not connected to the Spoor service");
  const rel = d.path ? safeRelPath(d.path) : undefined;
  if (d.path && !rel) throw new Error(`invalid download path ${JSON.stringify(d.path)}`);
  await gate(run);
  const storeId = await containerFor(run.site);
  const opts: browser.downloads._DownloadOptions & { cookieStoreId?: string } = {
    url: d.url,
    saveAs: false,
    conflictAction: rel ? "overwrite" : "uniquify",
    cookieStoreId: storeId,
  };
  // Below Firefox's download folder; the service moves it out right after.
  if (rel) opts.filename = `${run.site}/${run.id}/${rel}`;
  if (d.method) opts.method = d.method as "GET" | "POST";
  if (d.headers) opts.headers = Object.entries(d.headers).map(([name, value]) => ({ name, value: String(value) }));
  if (d.body !== undefined) opts.body = d.body;
  // Firefox names the file after the URL; the server's Content-Disposition
  // name (ILIAS: every URL is ilias.php) comes from the response headers.
  const named: DownloadName = { requestIds: new Set(), referer: d.referer };
  downloadNames.set(d.url, named);
  let id: number;
  try {
    id = await browser.downloads.download(opts);
  } catch (e) {
    downloadNames.delete(d.url);
    throw e;
  }
  run.downloads.add(id);
  run.requests++;
  let item: browser.downloads.DownloadItem | undefined;
  try {
    for (;;) {
      [item] = await browser.downloads.search({ id });
      if (!item) throw new Error("the download disappeared");
      if (item.state === "complete") break;
      if (item.state === "interrupted") throw new Error(`download failed: ${item.error ?? "interrupted"} (${d.url})`);
      if (!state.runs.has(run.id)) throw new Error("run stopped");
      if (run.status === "paused" && !item.paused) await browser.downloads.pause(id).catch(() => {});
      if (run.status !== "paused" && item.paused && item.canResume) await browser.downloads.resume(id).catch(() => {});
      await new Promise(r => setTimeout(r, 250));
    }
  } catch (e) {
    await browser.downloads.cancel(id).catch(() => {});
    await browser.downloads.erase({ id }).catch(() => {});
    emit("audit", { site: run.site, run: run.id, method: d.method ?? "GET", url: d.url, type: "download", error: errorText(e) });
    throw e;
  } finally {
    run.downloads.delete(id);
    if (downloadNames.get(d.url) === named) downloadNames.delete(d.url);
  }
  // Keep the file, drop the entry from the downloads list.
  await browser.downloads.erase({ id }).catch(() => {});
  emit("audit", { site: run.site, run: run.id, method: d.method ?? "GET", url: d.url, type: "download", bytes: item.fileSize, mime: item.mime });
  const base = (named.disposition && dispositionName(named.disposition))?.replace(/[\\/]/g, "_") || item.filename.split(/[\\/]/).pop()!;
  const path = rel ?? (d.dir ? `${d.dir}/${base}` : base);
  const out = await peer.call<Record<string, unknown>>("run.adopt", { run_id: run.id, from: item.filename, path, url: d.url }, 120_000);
  return { ...out, mime: item.mime || undefined };
}

interface DownloadName {
  requestIds: Set<string>;
  disposition?: string;
  /** The download API can't set Referer; added to the request here instead. */
  referer?: string;
}

/** Downloads in flight by URL; their requests (tabId -1) carry the file name. */
const downloadNames = new Map<string, DownloadName>();

browser.webRequest.onBeforeRequest.addListener(d => {
  if (d.tabId === -1) downloadNames.get(d.url)?.requestIds.add(d.requestId);
}, { urls: ["<all_urls>"] });

browser.webRequest.onBeforeSendHeaders.addListener(d => {
  if (d.tabId !== -1) return {};
  for (const n of downloadNames.values()) {
    if (!n.referer || !n.requestIds.has(d.requestId)) continue;
    const headers = (d.requestHeaders ?? []).filter(h => h.name.toLowerCase() !== "referer");
    headers.push({ name: "Referer", value: n.referer });
    return { requestHeaders: headers };
  }
  return {};
}, { urls: ["<all_urls>"] }, ["blocking", "requestHeaders"]);

browser.webRequest.onHeadersReceived.addListener(d => {
  if (d.tabId !== -1) return;
  for (const n of downloadNames.values()) {
    if (!n.requestIds.has(d.requestId)) continue;
    const h = d.responseHeaders?.find(x => x.name.toLowerCase() === "content-disposition");
    if (h?.value) n.disposition = h.value;
  }
}, { urls: ["<all_urls>"] }, ["responseHeaders"]);

/** `attachment; filename="a b.zip"; filename*=UTF-8''a%20b.zip` → "a b.zip" (filename* wins). */
export function dispositionName(v: string): string | undefined {
  const star = /filename\*\s*=\s*([^;]+)/i.exec(v);
  if (star) {
    const raw = star[1]!.trim().replace(/^"|"$/g, "");
    const m = /^[\w-]*'[^']*'(.*)$/.exec(raw);
    try {
      return decodeURIComponent(m ? m[1]! : raw);
    } catch {
      // fall through to filename=
    }
  }
  const plain = /filename\s*=\s*(?:"([^"]*)"|([^;]+))/i.exec(v);
  const name = (plain?.[1] ?? plain?.[2])?.trim();
  return name || undefined;
}

function askConfirm(run: Run, message: string, detail: unknown): Promise<boolean> {
  const id = `${run.id}-${Math.random().toString(16).slice(2, 8)}`;
  return new Promise<boolean>(resolve => {
    state.confirms.set(id, {
      id,
      run_id: run.id,
      site: run.site,
      message,
      detail,
      resolve: approved => {
        runEvent(run, { type: "confirmed", confirm_id: id, approved });
        resolve(approved);
      },
    });
    runEvent(run, { type: "confirm", confirm_id: id, message, detail });
    void browser.notifications.create(id, { type: "basic", title: `Spoor · ${run.site}: approval needed`, message });
    void openSidebar();
    broadcast();
  });
}

function answerConfirm(id: string, approved: boolean): void {
  const c = state.confirms.get(id);
  if (!c) return;
  state.confirms.delete(id);
  void browser.notifications.clear(id);
  c.resolve(approved);
  broadcast();
}

async function openSidebar(): Promise<void> {
  try {
    if (!(await browser.sidebarAction.isOpen({}))) await browser.sidebarAction.open();
  } catch {
    // only allowed from a user action; the notification has to do
  }
}

// ------------------------------------------------------------------ audit (worker tabs)

/** The run whose script lives in this tab (its worker tab). */
function runForTab(tabId: number): Run | undefined {
  for (const r of state.runs.values()) if (r.tabId === tabId) return r;
  return undefined;
}

/** The run using this tab, as worker or spoor.tab(). */
function runUsingTab(tabId: number): Run | undefined {
  for (const r of state.runs.values()) if (r.tabId === tabId || r.auxTabs.includes(tabId)) return r;
  return undefined;
}

/** For the audit: the running run on this tab, or one that just finished there. */
function auditRunForTab(tabId: number): Run | undefined {
  return runUsingTab(tabId) ?? lingering.get(tabId);
}

browser.webRequest.onCompleted.addListener(d => {
  const run = auditRunForTab(d.tabId);
  if (!run) return;
  run.requests++;
  emit("audit", { site: run.site, run: run.id, method: d.method, url: d.url, status: d.statusCode, type: d.type });
}, { urls: ["<all_urls>"] });

browser.webRequest.onErrorOccurred.addListener(d => {
  const run = auditRunForTab(d.tabId);
  if (!run) return;
  run.requests++;
  emit("audit", { site: run.site, run: run.id, method: d.method, url: d.url, error: d.error, type: d.type });
}, { urls: ["<all_urls>"] });

// A run's page navigating away destroys the script's context.
browser.webNavigation.onCommitted.addListener(d => {
  if (d.frameId !== 0) return;
  const run = runForTab(d.tabId);
  if (run) void finishRun(run, { ok: false, status: "error", error: `the worker tab navigated to ${d.url}; don't navigate in scripts, use fetch` });
});

browser.tabs.onRemoved.addListener(tabId => {
  tabHistory.delete(tabId);
  const run = runForTab(tabId);
  if (run) void finishRun(run, { ok: false, status: "error", error: "the worker tab was closed" });
  for (const [site, id] of state.workers) if (id === tabId) state.workers.delete(site);
  broadcast();
});

// ------------------------------------------------------------------ recording

const BODY_TYPES = new Set(["main_frame", "sub_frame", "xmlhttprequest", "other", "object", "xslt", "web_manifest"]);
const SKIP_TYPES = new Set(["image", "imageset", "media", "font", "stylesheet", "script", "beacon", "ping", "csp_report", "speculative"]);

function recordingFor(storeId: string | undefined): Recording | undefined {
  const site = siteForStore(storeId);
  return site ? state.recordings.get(site) : undefined;
}

function headerMap(headers: browser.webRequest.HttpHeaders | undefined): Record<string, string> {
  const out: Record<string, string> = {};
  for (const h of headers ?? []) {
    const k = h.name.toLowerCase();
    const v = h.value ?? (h.binaryValue ? String.fromCharCode(...h.binaryValue) : "");
    out[k] = out[k] === undefined ? v : `${out[k]}\n${v}`;
  }
  return out;
}

function requestBody(d: browser.webRequest._OnBeforeRequestDetails): Body | undefined {
  const rb = d.requestBody;
  if (!rb) return undefined;
  if (rb.error) return { kind: "omitted", reason: rb.error };
  if (rb.formData) {
    const params = new URLSearchParams();
    for (const [k, vs] of Object.entries(rb.formData)) for (const v of vs as string[]) params.append(k, v);
    return { kind: "text", text: params.toString() };
  }
  if (rb.raw) {
    const parts = rb.raw.filter(p => p.bytes).map(p => new Uint8Array(p.bytes as ArrayBuffer));
    if (rb.raw.some(p => p.file)) return { kind: "omitted", reason: "file_upload" };
    return decodeBody(concat(parts), undefined);
  }
  return undefined;
}

function concat(parts: Uint8Array[]): Uint8Array {
  const len = parts.reduce((a, p) => a + p.length, 0);
  const out = new Uint8Array(len);
  let o = 0;
  for (const p of parts) {
    out.set(p, o);
    o += p.length;
  }
  return out;
}

function isTextual(ct: string | undefined): boolean {
  if (!ct) return true;
  return /^(text\/|application\/(json|.*\+json|xml|.*\+xml|javascript|x-www-form-urlencoded|graphql))/i.test(ct);
}

function decodeBody(bytes: Uint8Array, contentType: string | undefined, total?: number): Body {
  const size = total ?? bytes.length;
  if (isTextual(contentType)) {
    const text = new TextDecoder("utf-8", { fatal: false }).decode(bytes);
    return size > bytes.length ? { kind: "text", text, truncated: true } : { kind: "text", text };
  }
  if (size > MAX_BINARY_BODY) return { kind: "omitted", reason: "too_large", size };
  let bin = "";
  for (let i = 0; i < bytes.length; i += 0x8000) bin += String.fromCharCode(...bytes.subarray(i, i + 0x8000));
  return { kind: "bytes", base64: btoa(bin), content_type: contentType };
}

function finalizeFlow(rec: Recording, requestId: string, pf: PendingFlow): void {
  if (rec.open.get(requestId) === pf) rec.open.delete(requestId);
  pf.flow.duration_ms = Date.now() - pf.started;
  if (pf.flow.request_body) pf.flow.request_body = redactBody(pf.flow.request_body, pf.flow.request_headers?.["content-type"]);
  rec.flows++;
  emit("record.flow", { site: rec.site, rec_id: rec.rec_id, flow: pf.flow });
  broadcastSoon();
}

function maybeFinish(rec: Recording, requestId: string, pf: PendingFlow): void {
  if (pf.completed && !pf.bodyPending) finalizeFlow(rec, requestId, pf);
}

browser.webRequest.onBeforeRequest.addListener(
  d => {
    const rec = recordingFor(d.cookieStoreId);
    if (!rec) return {};
    if (d.url.startsWith("moz-extension:")) return {};
    const flow: Flow = {
      sequence: ++rec.seq,
      kind: "http",
      timestamp_ms: Math.round(d.timeStamp),
      url: d.url,
      method: d.method,
      type: d.type,
      tab_id: d.tabId,
      document_url: d.documentUrl ?? d.originUrl,
      request_body: requestBody(d),
    };
    const pf: PendingFlow = { flow, started: Date.now(), completed: false, bodyPending: false };
    rec.open.set(d.requestId, pf);
    if (SKIP_TYPES.has(d.type)) {
      flow.response_body = { kind: "omitted", reason: d.type };
    } else if (BODY_TYPES.has(d.type)) {
      try {
        const filter = browser.webRequest.filterResponseData(d.requestId);
        const chunks: Uint8Array[] = [];
        let kept = 0;
        let total = 0;
        pf.bodyPending = true;
        filter.ondata = e => {
          const chunk = new Uint8Array(e.data);
          total += chunk.length;
          if (kept < MAX_TEXT_BODY) {
            const take = chunk.subarray(0, MAX_TEXT_BODY - kept);
            chunks.push(take.slice());
            kept += take.length;
          }
          filter.write(e.data);
        };
        const end = () => {
          if (!pf.bodyPending) return;
          pf.bodyPending = false;
          const disposition = pf.flow.response_headers?.["content-disposition"] ?? "";
          if (total === 0 && /^\s*attachment/i.test(disposition)) {
            // The body went to the download manager, not through the filter.
            const length = Number(pf.flow.response_headers?.["content-length"]);
            pf.flow.response_body = { kind: "omitted", reason: "download", ...(Number.isFinite(length) && length > 0 ? { size: length } : {}) };
          } else if (total > 0 || !pf.flow.response_body) {
            pf.flow.response_body = decodeBody(concat(chunks), pf.flow.response_headers?.["content-type"]?.split(";")[0], total);
          }
          maybeFinish(rec, d.requestId, pf);
        };
        filter.onstop = () => {
          try { filter.close(); } catch {}
          end();
        };
        filter.onerror = () => end();
      } catch (e) {
        flow.response_body = { kind: "omitted", reason: `filter: ${errorText(e)}` };
      }
    }
    return {};
  },
  { urls: ["<all_urls>"] },
  ["blocking", "requestBody"],
);

browser.webRequest.onSendHeaders.addListener(d => {
  const pf = recordingFor(d.cookieStoreId)?.open.get(d.requestId);
  if (pf) pf.flow.request_headers = headerMap(d.requestHeaders);
}, { urls: ["<all_urls>"] }, ["requestHeaders"]);

browser.webRequest.onHeadersReceived.addListener(d => {
  const pf = recordingFor(d.cookieStoreId)?.open.get(d.requestId);
  if (!pf) return;
  pf.flow.status = d.statusCode;
  const headers = headerMap(d.responseHeaders);
  pf.flow.response_headers = headers;
  if (headers["set-cookie"]) pf.flow.set_cookies = headers["set-cookie"].split("\n");
}, { urls: ["<all_urls>"] }, ["responseHeaders"]);

browser.webRequest.onBeforeRedirect.addListener(d => {
  const rec = recordingFor(d.cookieStoreId);
  const pf = rec?.open.get(d.requestId);
  if (!rec || !pf) return;
  pf.flow.status = d.statusCode;
  pf.flow.redirect_url = d.redirectUrl;
  pf.bodyPending = false;
  pf.completed = true;
  // The redirected request reuses the requestId; it becomes its own flow.
  finalizeFlow(rec, d.requestId, pf);
}, { urls: ["<all_urls>"] });

browser.webRequest.onCompleted.addListener(d => {
  const rec = recordingFor(d.cookieStoreId);
  const pf = rec?.open.get(d.requestId);
  if (!rec || !pf) return;
  pf.flow.status = d.statusCode;
  pf.flow.from_cache = d.fromCache;
  pf.completed = true;
  maybeFinish(rec, d.requestId, pf);
  // A filter that never fires onstop (e.g. 304, empty bodies) mustn't hold the flow forever.
  if (pf.bodyPending) setTimeout(() => {
    if (pf.bodyPending) {
      pf.bodyPending = false;
      maybeFinish(rec, d.requestId, pf);
    }
  }, 5000);
}, { urls: ["<all_urls>"] });

browser.webRequest.onErrorOccurred.addListener(d => {
  const rec = recordingFor(d.cookieStoreId);
  const pf = rec?.open.get(d.requestId);
  if (!rec || !pf) return;
  pf.flow.error = d.error;
  pf.completed = true;
  pf.bodyPending = false;
  finalizeFlow(rec, d.requestId, pf);
}, { urls: ["<all_urls>"] });

async function siteTabs(site: string): Promise<browser.tabs.Tab[]> {
  const storeId = await containerFor(site);
  return browser.tabs.query({ cookieStoreId: storeId });
}

async function openSite(site: string, url: string): Promise<number> {
  const storeId = await containerFor(site);
  const tab = await browser.tabs.create({ url, cookieStoreId: storeId, active: true });
  if (tab.windowId !== undefined) await browser.windows.update(tab.windowId, { focused: true });
  return tab.id!;
}

async function recordStart(p: { site: string; rec_id: string; url?: string; open: boolean; site_url: string }): Promise<{ ok: true }> {
  if (state.recordings.has(p.site)) throw new Error(`already recording ${p.site}`);
  const storeId = await containerFor(p.site);
  state.recordings.set(p.site, { rec_id: p.rec_id, site: p.site, storeId, seq: 0, flows: 0, started: Date.now(), open: new Map() });
  broadcast();
  if (p.open) {
    const visible = (await siteTabs(p.site)).filter(t => !t.hidden);
    if (p.url || visible.length === 0) await openSite(p.site, p.url ?? p.site_url);
    else await browser.tabs.update(visible[0]!.id!, { active: true });
  }
  return { ok: true };
}

async function recordStop(p: { site: string; rec_id: string }): Promise<{ cookies: CookieInfo[]; storage: StorageSnapshot[] }> {
  const rec = state.recordings.get(p.site);
  if (!rec || rec.rec_id !== p.rec_id) throw new Error(`not recording ${p.site}`);
  // Let in-flight exchanges land (bounded), then flush the rest as they are.
  const deadline = Date.now() + 3000;
  while (rec.open.size > 0 && Date.now() < deadline) await new Promise(r => setTimeout(r, 100));
  for (const [id, pf] of rec.open) {
    pf.flow.error ??= "still in flight when recording stopped";
    finalizeFlow(rec, id, pf);
  }
  state.recordings.delete(p.site);
  broadcast();
  return { cookies: await cookiesFor(p.site), storage: await storageFor(p.site) };
}

function recordMark(p: { site: string; label: string }): { ok: true } {
  const rec = state.recordings.get(p.site);
  if (!rec) throw new Error(`not recording ${p.site}`);
  const flow: Flow = { sequence: ++rec.seq, kind: "mark", timestamp_ms: Date.now(), url: "", label: p.label };
  rec.flows++;
  emit("record.flow", { site: rec.site, rec_id: rec.rec_id, flow });
  broadcast();
  return { ok: true };
}

async function cookiesFor(site: string, url?: string): Promise<CookieInfo[]> {
  const storeId = await containerFor(site);
  let cookies: browser.cookies.Cookie[];
  try {
    // partitionKey {} also returns cookies partitioned by Total Cookie Protection.
    cookies = await browser.cookies.getAll({ storeId, url, partitionKey: {} } as browser.cookies._GetAllDetails);
  } catch {
    cookies = await browser.cookies.getAll({ storeId, url });
  }
  return cookies.map(c => ({
    name: c.name,
    value: c.value,
    domain: c.domain,
    path: c.path,
    secure: c.secure,
    httpOnly: c.httpOnly,
    sameSite: c.sameSite,
    session: c.session,
    expirationDate: c.expirationDate,
  }));
}

const STORAGE_DUMP = `(() => {
  const dump = s => { const o = {}; try { for (let i = 0; i < s.length; i++) { const k = s.key(i); o[k] = s.getItem(k); } } catch (e) {} return o; };
  return { url: location.href, local: dump(window.localStorage), session: dump(window.sessionStorage) };
})()`;

async function storageFor(site: string): Promise<StorageSnapshot[]> {
  const out: StorageSnapshot[] = [];
  const seen = new Set<string>();
  for (const tab of await siteTabs(site)) {
    try {
      const results = (await browser.tabs.executeScript(tab.id!, { code: STORAGE_DUMP, allFrames: true })) as StorageSnapshot[];
      for (const r of results) {
        if (!r || !/^https?:/.test(r.url)) continue;
        const origin = new URL(r.url).origin;
        if (seen.has(origin)) continue;
        seen.add(origin);
        out.push(r);
      }
    } catch {
      // privileged or unloaded page
    }
  }
  return out;
}

// ------------------------------------------------------------------ handlers the service calls

const handlers = {
  exec: (p: ExecParams) => exec(p),
  "run.control": async (p: { run_id: string; action: "pause" | "resume" | "stop" }) => {
    const run = state.runs.get(p.run_id);
    if (!run) throw new Error(`run ${p.run_id} is not running in the browser`);
    if (p.action === "stop") await stopRun(run, "stopped", "stopped by request");
    else setPaused(run, p.action === "pause");
    return { ok: true };
  },
  open: async (p: { site: string; url: string }) => ({ tab: await openSite(p.site, p.url) }),
  status: async (p: { site: string }): Promise<SiteBrowserState> => {
    const tabs = await siteTabs(p.site);
    const run = [...state.runs.values()].find(r => r.site === p.site);
    return {
      site: p.site,
      container: state.containers.get(p.site),
      worker_tab: state.workers.get(p.site),
      recording: state.recordings.get(p.site)?.rec_id,
      open_tabs: tabs.filter(t => !t.hidden).length,
      active_run: run?.id,
    };
  },
  stop: async (p: { site?: string }) => {
    for (const run of [...state.runs.values()]) if (!p.site || run.site === p.site) await stopRun(run, "stopped", "kill switch");
    for (const [site, tabId] of [...state.workers]) {
      if (p.site && site !== p.site) continue;
      state.workers.delete(site);
      await browser.tabs.remove(tabId).catch(() => {});
    }
    broadcast();
    return { ok: true };
  },
  "record.start": recordStart,
  "record.stop": recordStop,
  "record.mark": recordMark,
  cookies: (p: { site: string; url?: string }) => cookiesFor(p.site, p.url),
  "session.clear": clearSession,
  "login.prompt": loginPrompt,
  "login.tabs": async (p: { site: string }) => (await siteTabs(p.site)).filter(t => !t.hidden).map(t => ({ url: t.url, status: t.status })),
};

/**
 * Forget everything the site's container holds: stop its runs, close its
 * tabs, remove its cookies and storage, and remove the container itself (the
 * next use creates a fresh one). `cache` also clears Firefox's HTTP cache,
 * which is shared by all sites.
 */
async function clearSession(p: { site: string; cache?: boolean }): Promise<{ cookies_removed: number; cache: boolean }> {
  if (state.recordings.has(p.site)) throw new Error(`${p.site} is being recorded; stop the recording first`);
  for (const run of [...state.runs.values()]) if (run.site === p.site) await stopRun(run, "stopped", "session cleared");
  const storeId = await containerFor(p.site);
  const cookies = await cookiesFor(p.site);
  state.workers.delete(p.site);
  const tabs = await siteTabs(p.site);
  await browser.tabs.remove(tabs.map(t => t.id!)).catch(() => {});
  try {
    await browser.browsingData.remove({ cookieStoreId: storeId } as browser.browsingData.RemovalOptions, { cookies: true, localStorage: true, indexedDB: true });
  } catch (e) {
    console.warn("browsingData.remove by container failed; removing the container still clears it", e);
  }
  await browser.contextualIdentities.remove(storeId);
  state.containers.delete(p.site);
  if (p.cache) await browser.browsingData.removeCache({});
  broadcast();
  return { cookies_removed: cookies.length, cache: !!p.cache };
}

/** Bring a visible tab of the site to the front at its start page, for the human to log in. */
async function loginPrompt(p: { site: string; url: string }): Promise<{ tab: number }> {
  const storeId = await containerFor(p.site);
  const [existing] = (await siteTabs(p.site)).filter(t => !t.hidden);
  const tab = existing
    ? await browser.tabs.update(existing.id!, { url: p.url, active: true })
    : await browser.tabs.create({ url: p.url, cookieStoreId: storeId, active: true });
  if (tab.windowId !== undefined) await browser.windows.update(tab.windowId, { state: "normal", focused: true }).catch(() => {});
  return { tab: tab.id! };
}

async function toBackground(): Promise<void> {
  for (const w of await browser.windows.getAll({ windowTypes: ["normal"] })) {
    if (w.id !== undefined) await browser.windows.update(w.id, { state: "minimized" }).catch(() => {});
  }
}

const loginNotices = new Map<string, string>();

function notify(p: { id: string; title: string; message: string; site?: string; clear_after_ms?: number }): void {
  if (p.site) loginNotices.set(p.id, p.site);
  void browser.notifications.create(p.id, { type: "basic", title: p.title, message: p.message });
  if (p.clear_after_ms) setTimeout(() => void browser.notifications.clear(p.id), p.clear_after_ms);
}

// ------------------------------------------------------------------ messages from scripts and UI pages

browser.runtime.onMessage.addListener((msg: any, sender) => {
  if (msg?.spoor_run) {
    const run = runForSender(msg, sender);
    if (!run) return Promise.reject(new Error("run stopped"));
    return onScriptMessage(msg, run);
  }
  // UI pages are extension pages: no tab url on a web origin.
  if (msg?.ui && sender.id === browser.runtime.id && (!sender.url || sender.url.startsWith(browser.runtime.getURL("")))) {
    return onUiMessage(msg);
  }
  return undefined;
});

async function onUiMessage(msg: any): Promise<unknown> {
  switch (msg.ui) {
    case "api":
      return api(msg.method, msg.path, msg.body);
    case "confirm":
      answerConfirm(msg.id, !!msg.approved);
      return true;
    case "run.control": {
      const run = state.runs.get(msg.run_id);
      if (!run) return api("POST", `/sites/${msg.site}/runs/${msg.run_id}/${msg.action}`);
      if (msg.action === "stop") await stopRun(run, "stopped", "stopped from the sidebar");
      else setPaused(run, msg.action === "pause");
      return true;
    }
    case "siteForTab": {
      const [tab] = await browser.tabs.query({ active: true, windowId: msg.windowId });
      return { site: siteForStore(tab?.cookieStoreId), url: tab?.url };
    }
    case "open":
      return openSite(msg.site, msg.url);
    case "home":
      return openHome();
    case "reconnect":
      if (state.bridge === "down") connectBridge();
      return true;
  }
  throw new Error(`unknown ui message ${msg.ui}`);
}

// ------------------------------------------------------------------ UI state push

const uiPorts = new Set<browser.runtime.Port>();
let broadcastTimer: ReturnType<typeof setTimeout> | undefined;

function snapshot() {
  return {
    bridge: state.bridge,
    bridgeError: state.bridgeError,
    runs: [...state.runs.values()].map(r => ({
      id: r.id, site: r.site, script: r.script, status: r.status, started: r.started, requests: r.requests, progress: r.progress, log: r.log.slice(-50),
    })),
    confirms: [...state.confirms.values()].map(c => ({ id: c.id, run_id: c.run_id, site: c.site, message: c.message, detail: c.detail })),
    recordings: Object.fromEntries([...state.recordings].map(([s, r]) => [s, { rec_id: r.rec_id, flows: r.flows, started: r.started }])),
    containers: Object.fromEntries(state.containers),
    workers: Object.fromEntries(state.workers),
    recent: state.recent,
  };
}

function broadcast(extra?: Record<string, unknown>): void {
  const msg = { state: snapshot(), ...extra };
  for (const p of uiPorts) {
    try {
      p.postMessage(msg);
    } catch {
      uiPorts.delete(p);
    }
  }
}

function broadcastSoon(): void {
  if (broadcastTimer) return;
  broadcastTimer = setTimeout(() => {
    broadcastTimer = undefined;
    broadcast();
  }, 250);
}

browser.runtime.onConnect.addListener(port => {
  if (port.name !== "ui") return;
  uiPorts.add(port);
  port.onDisconnect.addListener(() => uiPorts.delete(port));
  port.postMessage({ state: snapshot() });
});

// ------------------------------------------------------------------ home panel

const HOME = browser.runtime.getURL("home.html");

async function openHome(): Promise<void> {
  const [existing] = await browser.tabs.query({ url: HOME });
  if (existing?.id !== undefined) {
    await browser.tabs.update(existing.id, { active: true });
    if (existing.windowId !== undefined) await browser.windows.update(existing.windowId, { focused: true });
  } else {
    await browser.tabs.create({ url: HOME, active: true });
  }
}

browser.browserAction.onClicked.addListener(() => void openHome());
browser.notifications.onClicked.addListener(id => {
  const site = loginNotices.get(id);
  if (!site) return void openSidebar();
  void (async () => {
    const [tab] = (await siteTabs(site)).filter(t => !t.hidden);
    if (tab?.id !== undefined) {
      await browser.tabs.update(tab.id, { active: true });
      if (tab.windowId !== undefined) await browser.windows.update(tab.windowId, { state: "normal", focused: true });
    }
  })();
});
browser.tabs.onActivated.addListener(() => broadcast({ tabChanged: true }));
browser.tabs.onUpdated.addListener((_id, info) => {
  if (info.url) broadcast({ tabChanged: true });
}, { properties: ["url"] });

// ------------------------------------------------------------------ start

void (async () => {
  await loadContainers();
  connectBridge();
  // Restored tabs can arrive a little after start.
  await sweepRestoredWorkers();
  setTimeout(() => void sweepRestoredWorkers(), 5000);
  const { openHomeAtStart = true } = await browser.storage.local.get("openHomeAtStart");
  if (openHomeAtStart) await openHome();
})();
