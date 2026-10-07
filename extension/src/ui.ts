// Shared bits for the home panel and the sidebar (extension pages).

export interface UiState {
  bridge: "connecting" | "connected" | "down";
  bridgeError?: string;
  runs: { id: string; site: string; script: string; status: string; started: number; requests: number; progress?: { done: number; total?: number; label?: string }; log: { level: string; text: string }[] }[];
  confirms: { id: string; run_id: string; site: string; message: string; detail?: unknown }[];
  recordings: Record<string, { rec_id: string; flows: number; started: number }>;
  containers: Record<string, string>;
  workers: Record<string, number>;
  recent: { id: string; site: string; script: string; status: string; error?: string }[];
}

export type Push = { state: UiState; changed?: string | null; tabChanged?: boolean };

export function connect(onPush: (p: Push) => void): void {
  const port = browser.runtime.connect({ name: "ui" });
  port.onMessage.addListener(m => onPush(m as Push));
}

export function api<T = any>(method: string, path: string, body?: unknown): Promise<T> {
  return browser.runtime.sendMessage({ ui: "api", method, path, body }) as Promise<T>;
}

export function ui<T = any>(msg: Record<string, unknown>): Promise<T> {
  return browser.runtime.sendMessage(msg) as Promise<T>;
}

type Child = Node | string | number | null | undefined | false | Child[];

export function h<K extends keyof HTMLElementTagNameMap>(
  tag: K,
  attrs: Record<string, unknown> | null = null,
  ...children: Child[]
): HTMLElementTagNameMap[K] {
  const el = document.createElement(tag);
  for (const [k, v] of Object.entries(attrs ?? {})) {
    if (v === undefined || v === null || v === false) continue;
    if (k.startsWith("on") && typeof v === "function") el.addEventListener(k.slice(2), v as EventListener);
    else if (k === "class") el.className = String(v);
    else if (k === "value") (el as HTMLInputElement).value = String(v);
    else if (v === true) el.setAttribute(k, "");
    else el.setAttribute(k, String(v));
  }
  const add = (c: Child) => {
    if (c === null || c === undefined || c === false) return;
    if (Array.isArray(c)) c.forEach(add);
    else el.append(c instanceof Node ? c : String(c));
  };
  children.forEach(add);
  return el;
}

/**
 * Replace root's content. Inputs with a `data-key` keep focus, cursor and
 * scroll position across re-renders.
 */
export function mount(root: HTMLElement, ...children: Child[]): void {
  const active = document.activeElement as HTMLInputElement | HTMLTextAreaElement | null;
  const key = active?.dataset?.key;
  const sel = key ? [active!.selectionStart, active!.selectionEnd] : null;
  const scrolls = new Map<string, number>();
  root.querySelectorAll<HTMLElement>("[data-key]").forEach(el => scrolls.set(el.dataset.key!, el.scrollTop));
  root.replaceChildren();
  const add = (c: Child) => {
    if (c === null || c === undefined || c === false) return;
    if (Array.isArray(c)) c.forEach(add);
    else root.append(c instanceof Node ? c : String(c));
  };
  children.forEach(add);
  root.querySelectorAll<HTMLElement>("[data-key]").forEach(el => {
    const top = scrolls.get(el.dataset.key!);
    if (top) el.scrollTop = top;
  });
  if (key) {
    const el = root.querySelector<HTMLInputElement>(`[data-key="${CSS.escape(key)}"]`);
    if (el) {
      el.focus();
      if (sel && sel[0] !== null) el.setSelectionRange(sel[0], sel[1]);
    }
  }
}

export function pill(text: string, kind = ""): HTMLElement {
  return h("span", { class: `pill ${kind}` }, text);
}

export function statusKind(status: string): string {
  return ({ ok: "good", running: "busy", waiting: "warn", paused: "warn", queued: "", error: "bad", stopped: "bad" } as Record<string, string>)[status] ?? "";
}

export function ago(ts: string | number): string {
  const s = Math.round((Date.now() - (typeof ts === "number" ? ts : Date.parse(ts))) / 1000);
  if (s < 60) return `${s}s ago`;
  if (s < 3600) return `${Math.round(s / 60)}m ago`;
  if (s < 86400) return `${Math.round(s / 3600)}h ago`;
  return `${Math.round(s / 86400)}d ago`;
}

export function pretty(v: unknown): string {
  if (typeof v === "string") return v;
  try {
    return JSON.stringify(v, null, 2);
  } catch {
    return String(v);
  }
}

/** `key=value` lines (values parsed as JSON when they parse) or a JSON object. */
export function parseArgs(text: string): unknown {
  const t = text.trim();
  if (!t) return {};
  if (t.startsWith("{")) return JSON.parse(t);
  const out: Record<string, unknown> = {};
  for (const line of t.split("\n")) {
    if (!line.trim()) continue;
    const i = line.indexOf("=");
    if (i < 0) throw new Error(`expected key=value, got ${JSON.stringify(line)}`);
    const k = line.slice(0, i).trim();
    const v = line.slice(i + 1).trim();
    try {
      out[k] = JSON.parse(v);
    } catch {
      out[k] = v;
    }
  }
  return out;
}

export function bridgeBanner(s: UiState): HTMLElement | null {
  if (s.bridge === "connected") return null;
  return h("div", { class: `banner ${s.bridge === "down" ? "bad" : ""}` },
    s.bridge === "down" ? `Not connected to the Spoor service: ${s.bridgeError ?? "unknown error"}. ` : "Connecting to the Spoor service… ",
    s.bridge === "down" ? h("button", { class: "link", onclick: () => ui({ ui: "reconnect" }) }, "Retry") : null,
    s.bridge === "down" ? h("div", { class: "muted small" }, "Is the native host installed? Run `spoor install` in a terminal.") : null,
  );
}
