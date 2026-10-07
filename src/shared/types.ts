// Types shared by the extension, the local service and the CLI.
// Type-only module: everything here is erased at runtime.

/** One entry of the site registry (`sites.json`). */
export interface Site {
  /** Start page; the worker tab loads it before running a script. */
  url: string;
  /** Script name in the site's library that returns truthy when logged in. */
  check?: string;
  /** Minimum milliseconds between requests a run makes (default 1000). */
  min_gap_ms?: number;
}

export interface SiteEntry extends Site {
  name: string;
}

// ---------------------------------------------------------------- recordings

export type Body =
  | { kind: "text"; text: string; truncated?: boolean }
  | { kind: "bytes"; base64: string; content_type?: string }
  | { kind: "omitted"; reason: string; size?: number };

/**
 * One recorded exchange (or a marker the human set while recording).
 * Header names are lower-case; repeated headers are joined with "\n".
 */
export interface Flow {
  sequence: number;
  kind: "http" | "mark";
  timestamp_ms: number;
  url: string;
  method?: string;
  /** webRequest resource type: main_frame, sub_frame, xmlhttprequest, script, … */
  type?: string;
  tab_id?: number;
  document_url?: string;
  request_headers?: Record<string, string>;
  request_body?: Body;
  status?: number;
  response_headers?: Record<string, string>;
  /** Each Set-Cookie line, unparsed. */
  set_cookies?: string[];
  response_body?: Body;
  /** For 3xx: where the browser went next (also a separate flow). */
  redirect_url?: string;
  from_cache?: boolean;
  error?: string;
  duration_ms?: number;
  /** Markers: what the human said this step is. */
  label?: string;
}

export interface CookieInfo {
  name: string;
  value: string;
  domain: string;
  path: string;
  secure: boolean;
  httpOnly: boolean;
  sameSite: string;
  session: boolean;
  /** Unix seconds; absent for session cookies. */
  expirationDate?: number;
}

export interface StorageSnapshot {
  url: string;
  local: Record<string, string>;
  session: Record<string, string>;
}

export interface RecordingMeta {
  id: string;
  site: string;
  started: string;
  stopped?: string;
  flows: number;
  start_url?: string;
}

// ---------------------------------------------------------------- runs

export type RunStatus = "queued" | "running" | "paused" | "waiting" | "ok" | "error" | "stopped";

export interface RunMeta {
  id: string;
  site: string;
  /** Library script name, or "(inline)". */
  script: string;
  args: unknown;
  status: RunStatus;
  started: string;
  finished?: string;
  duration_ms?: number;
  requests?: number;
  error?: string;
  progress?: Progress;
  /** Files written with spoor.save(). */
  files?: string[];
}

export interface Progress {
  done: number;
  total?: number;
  label?: string;
}

/** One line of a run's `events.jsonl`. `n` orders events within the run. */
export type RunEvent = { n: number; ts: string } & (
  | { type: "status"; status: RunStatus; error?: string }
  | { type: "log"; level: string; text: string }
  | { type: "progress"; progress: Progress }
  | { type: "confirm"; confirm_id: string; message: string; detail?: unknown }
  | { type: "confirmed"; confirm_id: string; approved: boolean }
  | { type: "saved"; file: string; bytes: number }
  | { type: "result"; ok: boolean; result?: unknown; error?: string }
);

export type RunEventInput = DistributiveOmit<RunEvent, "n" | "ts">;
type DistributiveOmit<T, K extends keyof any> = T extends unknown ? Omit<T, K> : never;

// ---------------------------------------------------------------- bridge
// Messages between the service and the extension. Both sides can call the
// other (`call` → `reply`) and push `event`s.

export type BridgeMessage =
  | { t: "call"; id: number; method: string; params?: unknown }
  | { t: "reply"; id: number; ok: true; result?: unknown }
  | { t: "reply"; id: number; ok: false; error: string }
  | { t: "event"; name: string; data?: unknown };

/** Service → extension: start a run in the site's worker tab. */
export interface ExecParams {
  run_id: string;
  site: string;
  /** Library script name, or "(inline)". */
  script: string;
  site_url: string;
  /** Function body; `args` and `spoor` are in scope. */
  code: string;
  args: unknown;
  url?: string;
  timeout_ms: number;
  min_gap_ms: number;
}

/** Extension → service: a run finished (or was stopped / timed out). */
export interface RunDone {
  run_id: string;
  ok: boolean;
  status: RunStatus;
  result?: unknown;
  error?: string;
  requests: number;
}

/** What the extension reports about one site. */
export interface SiteBrowserState {
  site: string;
  container?: string;
  worker_tab?: number;
  recording?: string;
  open_tabs: number;
  active_run?: string;
}
