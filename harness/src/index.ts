// loginHarness: get a usable session for a site that needs a browser login
// (SSO, MFA, device-bound sessions), then do plain HTTP with it.
//
//   const session = await loginHarness({
//     start: "https://app.example.com/",
//     profile: "mysite",                     // persistent → often silent next time
//     done: s => s.cookies.has("SESSION_ID"),
//     capture: { cookies: ["app.example.com"], storage: ["authToken"] },
//   });
//   const r = await session.fetch("https://app.example.com/api/me");
//
// Like `aws sso login`: open a window, the human does the login (or nothing,
// when the persistent profile still has a valid IdP session), capture what
// the app needs, close. With `silentFirst` (default) it first tries without
// any window and only shows one if the login doesn't complete on its own.
//
// Built on Playwright, not on Spoor, so the apps that use it don't depend on
// Spoor. `start`, `done` and `capture` are what you work out from a Spoor
// recording of the login (`spoor auth`, `spoor lifetimes`).

import { mkdirSync } from "node:fs";
import { homedir } from "node:os";
import { isAbsolute, join } from "node:path";
import type { BrowserContext, BrowserType, Cookie, Page } from "playwright";

export interface SessionView {
  /** URL of the page the login ended up on (the most recently navigated tab). */
  url: string;
  cookies: { has(name: string): boolean; get(name: string): string | undefined; all: Cookie[] };
  storage: { get(key: string): string | undefined };
}

export interface HarnessOptions {
  /** Page that starts the login (usually the app's own start page). */
  start: string;
  /** Profile name (kept under ~/.spoor-harness/) or an absolute directory. */
  profile: string;
  /** True once the login is complete. Polled while the window is open. */
  done: (s: SessionView) => boolean | Promise<boolean>;
  /** What to keep. Cookie domains match by suffix; storage keys exactly. Default: everything. */
  capture?: { cookies?: string[]; storage?: string[] };
  /** Give up after this long (default 5 minutes). */
  timeoutMs?: number;
  /** Try without a window first; show one only if needed (default true). */
  silentFirst?: boolean;
  /** How long the silent attempt may take (default 15 s). */
  silentTimeoutMs?: number;
  /** "firefox" (default) or "chromium". */
  browser?: "firefox" | "chromium";
  /** Poll interval for `done` (default 500 ms). */
  pollMs?: number;
}

export interface Session {
  /** True when no window was shown. */
  silent: boolean;
  url: string;
  cookies: Cookie[];
  /** localStorage + sessionStorage values by key (from the final page's origin). */
  storage: Record<string, string>;
  capturedAt: Date;
  /** Earliest expiry among captured non-session cookies, if any. */
  expiresAt?: Date;
  /** `Cookie:` header value for a request to `url`. */
  cookieHeader(url: string): string;
  /** fetch with the session's cookies for that URL added. */
  fetch(url: string, init?: RequestInit): Promise<Response>;
}

export class LoginTimeout extends Error {}

export function profileDir(profile: string): string {
  return isAbsolute(profile) ? profile : join(homedir(), ".spoor-harness", profile);
}

function view(url: string, cookies: Cookie[], storage: Record<string, string>): SessionView {
  return {
    url,
    cookies: {
      has: name => cookies.some(c => c.name === name),
      get: name => cookies.find(c => c.name === name)?.value,
      all: cookies,
    },
    storage: { get: key => storage[key] },
  };
}

async function readStorage(page: Page): Promise<Record<string, string>> {
  try {
    return await page.evaluate(() => {
      const out: Record<string, string> = {};
      for (const s of [window.localStorage, window.sessionStorage]) {
        for (let i = 0; i < s.length; i++) {
          const k = s.key(i)!;
          out[k] = s.getItem(k) ?? "";
        }
      }
      return out;
    });
  } catch {
    return {}; // mid-navigation or a page without storage access
  }
}

function domainMatches(cookieDomain: string, wanted: string): boolean {
  const d = cookieDomain.replace(/^\./, "");
  return d === wanted || d.endsWith(`.${wanted}`) || wanted.endsWith(`.${d}`);
}

/** Cookies that would be sent to `url` (domain, path, secure). */
export function cookiesFor(cookies: Cookie[], url: string): Cookie[] {
  const u = new URL(url);
  return cookies.filter(c => {
    const d = c.domain.replace(/^\./, "");
    const hostOk = c.domain.startsWith(".") ? u.hostname === d || u.hostname.endsWith(`.${d}`) : u.hostname === d;
    const pathOk = u.pathname === c.path || u.pathname.startsWith(c.path.endsWith("/") ? c.path : `${c.path}/`) || c.path === "/";
    const secureOk = !c.secure || u.protocol === "https:" || u.hostname === "localhost" || u.hostname === "127.0.0.1";
    const notExpired = c.expires === -1 || c.expires * 1000 > Date.now();
    return hostOk && pathOk && secureOk && notExpired;
  });
}

export function makeSession(silent: boolean, url: string, cookies: Cookie[], storage: Record<string, string>): Session {
  const expiries = cookies.filter(c => c.expires > 0).map(c => c.expires * 1000);
  const cookieHeader = (target: string) => cookiesFor(cookies, target).map(c => `${c.name}=${c.value}`).join("; ");
  return {
    silent,
    url,
    cookies,
    storage,
    capturedAt: new Date(),
    expiresAt: expiries.length ? new Date(Math.min(...expiries)) : undefined,
    cookieHeader,
    fetch: (target, init = {}) => {
      const headers = new Headers(init.headers);
      const c = cookieHeader(target);
      if (c && !headers.has("cookie")) headers.set("cookie", c);
      return fetch(target, { ...init, headers });
    },
  };
}

async function attempt(type: BrowserType, opts: HarnessOptions, headless: boolean, timeoutMs: number): Promise<Session | null> {
  const dir = profileDir(opts.profile);
  mkdirSync(dir, { recursive: true });
  const context: BrowserContext = await type.launchPersistentContext(dir, { headless, viewport: null });
  try {
    const page = context.pages()[0] ?? (await context.newPage());
    let latest: Page = page;
    context.on("page", p => (latest = p));
    page.on("framenavigated", f => {
      if (f === page.mainFrame()) latest = page;
    });
    await page.goto(opts.start, { waitUntil: "domcontentloaded" }).catch(() => {
      // a redirect chain can abort the first navigation; the poll below is what counts
    });
    const deadline = Date.now() + timeoutMs;
    while (Date.now() < deadline) {
      if (latest.isClosed()) latest = context.pages().at(-1) ?? page;
      if (context.pages().length === 0) throw new LoginTimeout("the login window was closed");
      const cookies = await context.cookies();
      const storage = await readStorage(latest);
      const url = latest.url();
      if (await opts.done(view(url, cookies, storage))) {
        const keepCookies = opts.capture?.cookies ? cookies.filter(c => opts.capture!.cookies!.some(d => domainMatches(c.domain, d))) : cookies;
        const keepStorage = opts.capture?.storage ? Object.fromEntries(Object.entries(storage).filter(([k]) => opts.capture!.storage!.includes(k))) : storage;
        return makeSession(headless, url, keepCookies, keepStorage);
      }
      await new Promise(r => setTimeout(r, opts.pollMs ?? 500));
    }
    return null;
  } finally {
    await context.close().catch(() => {});
  }
}

export async function loginHarness(opts: HarnessOptions): Promise<Session> {
  const pw = await import("playwright");
  const type = opts.browser === "chromium" ? pw.chromium : pw.firefox;
  const total = opts.timeoutMs ?? 5 * 60_000;
  if (opts.silentFirst ?? true) {
    const s = await attempt(type, opts, true, Math.min(opts.silentTimeoutMs ?? 15_000, total));
    if (s) return s;
  }
  const s = await attempt(type, opts, false, total);
  if (!s) throw new LoginTimeout(`login did not complete within ${Math.round(total / 1000)} s`);
  return s;
}
