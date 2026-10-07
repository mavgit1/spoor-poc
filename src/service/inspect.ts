// Reading recordings. Plain functions over Flow[], no site knowledge: Spoor
// lays out the evidence, the agent draws the conclusions.
//
// - matchingFlows: which requests mention X (url, headers or either body)?
// - trace: where did value X first *appear* (a response, HTML included), and
//   which later requests *used* it?
// - authChain: the navigation/redirect sequence across hosts, with the
//   cookies set and the OAuth/SAML parameters that went by.
// - lifetimes: cookie expiry and JWT claims found anywhere in the recording.
// - endpoints: a catalog of the API-ish requests, grouped by path template.

import type { Body, CookieInfo, Flow, StorageSnapshot } from "../shared/types.ts";

const SNIPPET_RADIUS = 60;

export function bodyText(b: Body | undefined): string | undefined {
  return b?.kind === "text" ? b.text : undefined;
}

function headerValues(h: Record<string, string> | undefined): [string, string][] {
  return Object.entries(h ?? {}).sort(([a], [b]) => a.localeCompare(b));
}

export function matchingFlows(flows: Flow[], needle?: string): Flow[] {
  return flows
    .filter(f => {
      if (!needle) return true;
      if (f.url.includes(needle) || f.label?.includes(needle)) return true;
      if (bodyText(f.request_body)?.includes(needle) || bodyText(f.response_body)?.includes(needle)) return true;
      return [...headerValues(f.request_headers), ...headerValues(f.response_headers)].some(([, v]) => v.includes(needle));
    })
    .sort((a, b) => a.sequence - b.sequence);
}

/** One line per flow: `seq METHOD status type url`. */
export function flowLine(f: Flow): string {
  if (f.kind === "mark") return `${String(f.sequence).padStart(5)} ---- mark: ${f.label ?? ""} ----`;
  const status = f.status !== undefined ? String(f.status) : f.error ? "ERR" : "-";
  const type = (f.type ?? "-").replace("xmlhttprequest", "xhr");
  return `${String(f.sequence).padStart(5)} ${(f.method ?? "").padEnd(7)} ${status.padStart(3)} ${type.padEnd(10)} ${f.url}`;
}

export interface TraceHit {
  sequence: number;
  method: string;
  url: string;
  /** `response body`, `response header location`, `url`, `request body`, … */
  place: string;
  snippet: string;
}

export function trace(flows: Flow[], value: string): { appears_in: TraceHit[]; used_in: TraceHit[] } {
  const out = { appears_in: [] as TraceHit[], used_in: [] as TraceHit[] };
  if (!value) return out;
  for (const f of [...flows].sort((a, b) => a.sequence - b.sequence)) {
    if (f.kind === "mark") continue;
    const hit = (place: string, text: string): TraceHit => ({
      sequence: f.sequence,
      method: f.method ?? "",
      url: f.url,
      place,
      snippet: snippet(text, value),
    });
    const resp = bodyText(f.response_body);
    if (resp?.includes(value)) out.appears_in.push(hit("response body", resp));
    for (const [name, v] of headerValues(f.response_headers)) {
      if (v.includes(value)) out.appears_in.push(hit(`response header ${name}`, v));
    }
    if (f.url.includes(value)) out.used_in.push(hit("url", f.url));
    for (const [name, v] of headerValues(f.request_headers)) {
      if (v.includes(value)) out.used_in.push(hit(`request header ${name}`, v));
    }
    const req = bodyText(f.request_body);
    if (req?.includes(value)) out.used_in.push(hit("request body", req));
  }
  return out;
}

/** `…context around the first match…`, whitespace collapsed. */
export function snippet(text: string, needle: string): string {
  const pos = text.indexOf(needle);
  if (pos < 0) return "";
  const chars = Array.from(text.slice(0, pos));
  const startIdx = Math.max(0, chars.length - SNIPPET_RADIUS);
  const before = chars.slice(startIdx).join("");
  const afterAll = Array.from(text.slice(pos + needle.length));
  const after = afterAll.slice(0, SNIPPET_RADIUS).join("");
  const body = (before + needle + after).split(/\s+/).filter(Boolean).join(" ");
  return `${startIdx > 0 ? "…" : ""}${body}${afterAll.length > SNIPPET_RADIUS ? "…" : ""}`;
}

// ---------------------------------------------------------------- auth chain

/** Parameters that identify OAuth 2 / OIDC, SAML and WS-Federation exchanges. */
const AUTH_PARAMS = [
  "client_id", "redirect_uri", "response_type", "response_mode", "scope", "state", "nonce",
  "code", "code_challenge", "id_token", "access_token", "prompt", "login_hint", "domain_hint",
  "SAMLRequest", "SAMLResponse", "RelayState", "wa", "wtrealm", "wctx", "wresult",
];
/** Values long enough to be secrets get shortened in the chain view. */
const SECRET_PARAMS = new Set(["code", "id_token", "access_token", "SAMLResponse", "SAMLRequest", "wresult", "state", "nonce"]);

export interface AuthStep {
  sequence: number;
  method: string;
  status?: number;
  host: string;
  path: string;
  type?: string;
  location?: string;
  sets_cookies: string[];
  sends_cookies: string[];
  params: Record<string, string>;
}

export interface AuthChain {
  hosts: string[];
  protocol_hints: string[];
  steps: AuthStep[];
}

function cookieNames(header: string | undefined): string[] {
  if (!header) return [];
  return header.split(/;\s*/).map(c => c.split("=", 1)[0]!.trim()).filter(Boolean);
}

export function setCookieName(line: string): string {
  return line.split("=", 1)[0]!.trim();
}

function formParams(f: Flow): URLSearchParams | undefined {
  const ct = f.request_headers?.["content-type"] ?? "";
  const body = bodyText(f.request_body);
  if (body && ct.includes("application/x-www-form-urlencoded")) return new URLSearchParams(body);
  return undefined;
}

function shorten(name: string, v: string): string {
  return SECRET_PARAMS.has(name) && v.length > 24 ? `${v.slice(0, 12)}…(${v.length} chars)` : v;
}

/**
 * Document loads and redirects in order, across every host the login touched.
 * XHRs are included only when they set cookies or carry auth parameters.
 */
export function authChain(flows: Flow[]): AuthChain {
  const steps: AuthStep[] = [];
  const hosts: string[] = [];
  const hints = new Set<string>();
  for (const f of [...flows].sort((a, b) => a.sequence - b.sequence)) {
    if (f.kind !== "http") continue;
    let u: URL;
    try {
      u = new URL(f.url);
    } catch {
      continue;
    }
    const params: Record<string, string> = {};
    const sources = [u.searchParams, formParams(f), u.hash.length > 1 ? new URLSearchParams(u.hash.slice(1)) : undefined];
    for (const src of sources) {
      if (!src) continue;
      for (const p of AUTH_PARAMS) {
        const v = src.get(p);
        if (v !== null) params[p] = shorten(p, v);
      }
    }
    const isDoc = f.type === "main_frame" || f.type === "sub_frame";
    const isRedirect = f.status !== undefined && f.status >= 300 && f.status < 400;
    const setsCookies = (f.set_cookies ?? []).map(setCookieName);
    if (!isDoc && !isRedirect && setsCookies.length === 0 && Object.keys(params).length === 0) continue;

    if (params.SAMLRequest || params.SAMLResponse) hints.add("SAML 2.0");
    if (params.wa?.startsWith("wsignin")) hints.add("WS-Federation");
    if (params.client_id && (params.response_type || params.redirect_uri)) {
      hints.add(params.response_type?.includes("id_token") || params.scope?.includes("openid") ? "OpenID Connect" : "OAuth 2.0");
    }
    if (u.hostname === "login.microsoftonline.com" || u.hostname.endsWith(".microsoftonline.com")) hints.add("Microsoft Entra ID");
    if (/\/token$/.test(u.pathname) && f.method === "POST") hints.add(`token endpoint: ${u.origin}${u.pathname}`);

    if (!hosts.includes(u.host)) hosts.push(u.host);
    steps.push({
      sequence: f.sequence,
      method: f.method ?? "",
      status: f.status,
      host: u.host,
      path: u.pathname,
      type: f.type,
      location: f.redirect_url ?? f.response_headers?.location,
      sets_cookies: setsCookies,
      sends_cookies: cookieNames(f.request_headers?.cookie),
      params,
    });
  }
  return { hosts, protocol_hints: [...hints], steps };
}

// ---------------------------------------------------------------- lifetimes

export interface JwtInfo {
  /** Where the token showed up first … */
  where: string;
  /** … and every place it was seen. */
  seen_in: string[];
  sequence?: number;
  claims: Record<string, unknown>;
  expires?: string;
  lifetime_s?: number;
}

const JWT_RE = /eyJ[A-Za-z0-9_-]{8,}\.eyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]*/g;
const INTERESTING_CLAIMS = ["iss", "aud", "sub", "appid", "azp", "scp", "roles", "tid", "iat", "nbf", "exp"];

export function decodeJwt(token: string): Record<string, unknown> | undefined {
  const part = token.split(".")[1];
  if (!part) return undefined;
  try {
    const json = Buffer.from(part.replace(/-/g, "+").replace(/_/g, "/"), "base64").toString("utf8");
    const v = JSON.parse(json);
    return typeof v === "object" && v !== null ? v : undefined;
  } catch {
    return undefined;
  }
}

function jwtInfo(token: string, where: string, sequence?: number): JwtInfo | undefined {
  const claims = decodeJwt(token);
  if (!claims) return undefined;
  const picked: Record<string, unknown> = {};
  for (const k of INTERESTING_CLAIMS) if (k in claims) picked[k] = claims[k];
  const exp = typeof claims.exp === "number" ? claims.exp : undefined;
  const iat = typeof claims.iat === "number" ? claims.iat : typeof claims.nbf === "number" ? claims.nbf : undefined;
  return {
    where,
    seen_in: [where],
    sequence,
    claims: picked,
    expires: exp ? new Date(exp * 1000).toISOString() : undefined,
    lifetime_s: exp && iat ? exp - iat : undefined,
  };
}

export interface CookieLifetime {
  name: string;
  domain: string;
  path: string;
  session: boolean;
  httpOnly: boolean;
  secure: boolean;
  sameSite: string;
  expires?: string;
  /** Seconds from the end of the recording until expiry. */
  remaining_s?: number;
  /** Sequence numbers of responses that set it during the recording. */
  set_by: number[];
}

export function lifetimes(
  flows: Flow[],
  cookies: CookieInfo[],
  storage: StorageSnapshot[],
  now = Date.now(),
): { cookies: CookieLifetime[]; jwts: JwtInfo[] } {
  const setBy = new Map<string, number[]>();
  for (const f of flows) {
    for (const line of f.set_cookies ?? []) {
      const key = setCookieName(line);
      setBy.set(key, [...(setBy.get(key) ?? []), f.sequence]);
    }
  }
  const cookieOut = cookies
    .map(c => ({
      name: c.name,
      domain: c.domain,
      path: c.path,
      session: c.session,
      httpOnly: c.httpOnly,
      secure: c.secure,
      sameSite: c.sameSite,
      expires: c.expirationDate ? new Date(c.expirationDate * 1000).toISOString() : undefined,
      remaining_s: c.expirationDate ? Math.round(c.expirationDate - now / 1000) : undefined,
      set_by: setBy.get(c.name) ?? [],
    }))
    .sort((a, b) => a.domain.localeCompare(b.domain) || a.name.localeCompare(b.name));

  const jwts: JwtInfo[] = [];
  const seen = new Map<string, JwtInfo>();
  const scan = (text: string | undefined, where: string, seq?: number) => {
    if (!text) return;
    for (const m of text.matchAll(JWT_RE)) {
      const known = seen.get(m[0]);
      if (known) {
        const place = seq !== undefined ? `${where} (seq ${seq})` : where;
        if (!known.seen_in.includes(place) && known.seen_in.length < 20) known.seen_in.push(place);
        continue;
      }
      const info = jwtInfo(m[0], seq !== undefined ? `${where} (seq ${seq})` : where, seq);
      if (!info) continue;
      seen.set(m[0], info);
      jwts.push(info);
    }
  };
  for (const f of [...flows].sort((a, b) => a.sequence - b.sequence)) {
    scan(f.request_headers?.authorization, "request header authorization", f.sequence);
    scan(bodyText(f.response_body), "response body", f.sequence);
    scan(f.url, "url", f.sequence);
    scan(bodyText(f.request_body), "request body", f.sequence);
  }
  for (const s of storage) {
    for (const [k, v] of Object.entries(s.local)) scan(v, `localStorage ${k} (${new URL(s.url).host})`);
    for (const [k, v] of Object.entries(s.session)) scan(v, `sessionStorage ${k} (${new URL(s.url).host})`);
  }
  for (const c of cookies) scan(c.value, `cookie ${c.name} (${c.domain})`);
  return { cookies: cookieOut, jwts };
}

// ---------------------------------------------------------------- endpoints

export interface Endpoint {
  method: string;
  origin: string;
  path: string;
  count: number;
  statuses: number[];
  content_types: string[];
  query_params: string[];
  request_body?: string;
  examples: number[];
}

const STATIC_TYPES = new Set(["image", "font", "media", "stylesheet", "script", "imageset", "beacon", "ping", "csp_report"]);

/** `/zones/4242/records/ab12…` → `/zones/{id}/records/{id}` */
export function pathTemplate(path: string): string {
  return path
    .split("/")
    .map(seg =>
      /^\d+$/.test(seg) || /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i.test(seg) || /^[0-9a-f]{16,}$/i.test(seg)
        ? "{id}"
        : seg,
    )
    .join("/");
}

function bodyKind(f: Flow): string | undefined {
  if (!f.request_body) return undefined;
  const ct = (f.request_headers?.["content-type"] ?? "").split(";")[0]!.trim();
  return ct || f.request_body.kind;
}

export function endpoints(flows: Flow[]): Endpoint[] {
  const map = new Map<string, Endpoint>();
  for (const f of flows) {
    if (f.kind !== "http" || STATIC_TYPES.has(f.type ?? "")) continue;
    let u: URL;
    try {
      u = new URL(f.url);
    } catch {
      continue;
    }
    if (u.protocol !== "http:" && u.protocol !== "https:") continue;
    const method = f.method ?? "GET";
    const path = pathTemplate(u.pathname);
    const key = `${method} ${u.origin}${path}`;
    let e = map.get(key);
    if (!e) {
      e = { method, origin: u.origin, path, count: 0, statuses: [], content_types: [], query_params: [], examples: [] };
      map.set(key, e);
    }
    e.count++;
    if (f.status !== undefined && !e.statuses.includes(f.status)) e.statuses.push(f.status);
    const ct = (f.response_headers?.["content-type"] ?? "").split(";")[0]!.trim();
    if (ct && !e.content_types.includes(ct)) e.content_types.push(ct);
    for (const k of u.searchParams.keys()) if (!e.query_params.includes(k)) e.query_params.push(k);
    e.request_body ??= bodyKind(f);
    if (e.examples.length < 3) e.examples.push(f.sequence);
  }
  return [...map.values()].sort((a, b) => a.origin.localeCompare(b.origin) || a.path.localeCompare(b.path) || a.method.localeCompare(b.method));
}
