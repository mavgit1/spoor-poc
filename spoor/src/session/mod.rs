//! Recordings — traffic captured while a human uses a site (`spoor record`),
//! stored as plain files an agent can read.
//!
//! # Layout
//!
//! ```text
//! {spoor_cache_dir()}/sessions/{session_id}/
//!   meta.json       site, started_at, ended_at, pages, flows_capped, counts, version
//!   flows.jsonl     one CaptureRecord per line, append-only while recording
//!   flows.jsonl.gz  gzipped JSONL after stop; uncompressed file is removed
//! ```
//!
//! `session_id` is `{YYYY-MM-DDTHH-MM-SSZ}-{6 hex}` so it sorts chronologically
//! and is filesystem-safe (`:` is not used).
//!
//! # Snapshots
//!
//! While recording, new flows are appended about every 2 seconds, so a crash
//! loses at most that window. Capture only appends, so the already-written
//! count is the cursor.
//!
//! # Pruning
//!
//! Keep `SPOOR_SESSION_KEEP` newest sessions (default 20). Also drop oldest
//! sessions if the store exceeds `SPOOR_SESSION_MAX_MB` (default 512), never
//! deleting the newest one.

mod recorder;
mod store;

pub use recorder::{Recording, SNAPSHOT_INTERVAL};
pub use store::{
    BrowsingPage, LoadedSession, SessionMeta, SessionStore, SessionSummary, SessionWriter,
    format_bytes, is_safe_session_id, keep_count, load_session_dir, load_source, max_bytes,
    new_session_id, utc_now_rfc3339,
};
