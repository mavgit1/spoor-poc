// Replayability check: does a recorded request also work *outside* the
// browser, from plain HTTP? With which cookies and headers?
//
// Every replay is a real request to the site, so replays are paced like runs
// and only GET/HEAD replay unless the caller explicitly allows writes.

import type { Flow } from "../shared/types.ts";
import { bodyText } from "./inspect.ts";

/** Headers fetch manages itself or that can't be replayed. */
const SKIP_HEADERS = new Set(["host", "connection", "content-length", "accept-encoding", "te", "upgrade", "keep-alive", "transfer-encoding", "cookie"]);

export interface ReplayRequest {
  url: string;
  method: string;
  headers: Record<string, string>;
  cookies: [string, string][];
  body?: string;
}

export interface ReplayResult {
  status: number;
  location?: string;
  content_type?: string;
  bytes: number;
  /** Does it look like the recorded response? */
  matches: boolean;
  why?: string;
}

export function parseCookieHeader(h: string | undefined): [string, string][] {
  if (!h) return [];
  return h
    .split(/;\s*/)
    .filter(Boolean)
    .map(p => {
      const i = p.indexOf("=");
      return i < 0 ? [p, ""] as [string, string] : [p.slice(0, i), p.slice(i + 1)] as [string, string];
    });
}

export function requestFromFlow(f: Flow, liveCookies?: [string, string][]): ReplayRequest {
  const headers: Record<string, string> = {};
  for (const [k, v] of Object.entries(f.request_headers ?? {})) {
    if (!SKIP_HEADERS.has(k)) headers[k] = v;
  }
  const body = bodyText(f.request_body);
  return {
    url: f.url,
    method: f.method ?? "GET",
    headers,
    cookies: liveCookies ?? parseCookieHeader(f.request_headers?.cookie),
    body: f.method === "GET" || f.method === "HEAD" ? undefined : body,
  };
}

export function isWrite(method: string): boolean {
  return !["GET", "HEAD", "OPTIONS"].includes(method.toUpperCase());
}

export async function send(req: ReplayRequest, recorded: Flow): Promise<ReplayResult> {
  const headers = new Headers();
  for (const [k, v] of Object.entries(req.headers)) {
    try {
      headers.set(k, v);
    } catch {
      // header fetch refuses (e.g. invalid chars); leave it out
    }
  }
  if (req.cookies.length) headers.set("cookie", req.cookies.map(([k, v]) => `${k}=${v}`).join("; "));
  const res = await fetch(req.url, { method: req.method, headers, body: req.body, redirect: "manual" });
  const buf = Buffer.from(await res.arrayBuffer());
  const result: ReplayResult = {
    status: res.status,
    location: res.headers.get("location") ?? undefined,
    content_type: res.headers.get("content-type")?.split(";")[0]?.trim() || undefined,
    bytes: buf.length,
    matches: false,
  };
  const why = compare(result, recorded);
  result.matches = why === undefined;
  if (why) result.why = why;
  return result;
}

/** undefined when the replay looks like the recorded response, else the reason it doesn't. */
export function compare(r: ReplayResult, f: Flow): string | undefined {
  if (f.status !== undefined && r.status !== f.status) return `status ${r.status}, recorded ${f.status}${r.location ? ` (→ ${r.location})` : ""}`;
  const recLoc = f.response_headers?.location ?? f.redirect_url;
  if (r.location && recLoc && new URL(r.location, f.url).pathname !== new URL(recLoc, f.url).pathname) return `redirects to ${r.location}, recorded ${recLoc}`;
  const recCt = f.response_headers?.["content-type"]?.split(";")[0]?.trim();
  if (recCt && r.content_type && recCt !== r.content_type) return `content-type ${r.content_type}, recorded ${recCt}`;
  const recText = bodyText(f.response_body);
  if (recText !== undefined && recText.length > 200) {
    const ratio = r.bytes / Buffer.byteLength(recText);
    if (ratio < 0.5 || ratio > 2) return `body ${r.bytes} bytes, recorded ${Buffer.byteLength(recText)}`;
  }
  return undefined;
}

export interface MinimizeReport {
  url: string;
  method: string;
  baseline: ReplayResult;
  /** Cookies without which the replay stops matching. */
  required_cookies: string[];
  optional_cookies: string[];
  required_headers: string[];
  optional_headers: string[];
  /** With only the required cookies and headers. */
  minimal?: ReplayResult;
  requests_sent: number;
}

/**
 * Replay with everything, then drop one cookie / header at a time and see
 * which ones the site actually needs. `pace` runs before each request.
 */
export async function minimize(f: Flow, base: ReplayRequest, pace: () => Promise<void>): Promise<MinimizeReport> {
  let sent = 0;
  const go = async (req: ReplayRequest) => {
    await pace();
    sent++;
    return send(req, f);
  };
  const baseline = await go(base);
  const report: MinimizeReport = {
    url: f.url,
    method: base.method,
    baseline,
    required_cookies: [],
    optional_cookies: [],
    required_headers: [],
    optional_headers: [],
    requests_sent: 0,
  };
  if (!baseline.matches) {
    report.requests_sent = sent;
    return report;
  }
  for (const [name] of base.cookies) {
    const r = await go({ ...base, cookies: base.cookies.filter(([n]) => n !== name) });
    (r.matches ? report.optional_cookies : report.required_cookies).push(name);
  }
  for (const name of Object.keys(base.headers)) {
    const headers = { ...base.headers };
    delete headers[name];
    const r = await go({ ...base, headers });
    (r.matches ? report.optional_headers : report.required_headers).push(name);
  }
  const minimalHeaders = Object.fromEntries(Object.entries(base.headers).filter(([k]) => report.required_headers.includes(k)));
  report.minimal = await go({
    ...base,
    headers: minimalHeaders,
    cookies: base.cookies.filter(([n]) => report.required_cookies.includes(n)),
  });
  report.requests_sent = sent;
  return report;
}
