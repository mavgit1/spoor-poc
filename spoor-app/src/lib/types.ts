/** Mirrors `spoor::runtime::SiteInfo`. */
export interface SiteInfo {
  name: string;
  url: string;
  check: string | null;
  min_gap_ms: number;
  running: boolean;
  recording: string | null;
}

/** Mirrors `spoor::runtime::SiteStatus`. */
export interface SiteStatus {
  site: string;
  running: boolean;
  recording: string | null;
  logged_in: boolean | null;
  error: string | null;
}

/** Mirrors `spoor::runtime::RecordInfo`. */
export interface RecordInfo {
  site: string;
  session_id: string;
  dir: string;
  flow_count: number;
}

export interface SessionRow {
  id: string;
  site: string | null;
  started_at: string;
  flows: number;
  size: string;
  path: string;
}
