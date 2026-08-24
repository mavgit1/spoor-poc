//! Capture session store — persist recorded traffic so a crash or restart
//! does not throw away a human's browsing.
//!
//! Not to be confused with `crate::export::session`, which infers id-handoff
//! edges for the agent pack.
//!
//! # Layout
//!
//! ```text
//! {spoor_cache_dir()}/sessions/{session_id}/
//!   meta.json       started_at, ended_at, pages, flows_capped, counts, version
//!   flows.jsonl     one CaptureRecord per line, append-only while recording
//!   flows.jsonl.gz  gzipped JSONL after Stop; uncompressed file is removed
//! ```
//!
//! `session_id` is `{YYYY-MM-DDTHH-MM-SSZ}-{6 hex}` so it sorts chronologically
//! and is filesystem-safe (`:` is not used).
//!
//! # Snapshot cursor
//!
//! A background task copies `AppState.flows[written..]` about every 2 seconds
//! and on Stop. Capture only appends (or the vec is cleared), so the already-
//! written count is the cursor — we do not diff contents. A crash loses at most
//! the last interval of flows. Persistence errors are logged and never surface
//! into the capture path.
//!
//! # Pruning
//!
//! Keep `SPOOR_SESSION_KEEP` newest sessions (default 20). Also drop oldest
//! sessions if the store exceeds `SPOOR_SESSION_MAX_MB` (default 512), never
//! deleting the newest one.

mod persist;
mod store;

pub use persist::{PersistHandle, SNAPSHOT_INTERVAL};
pub use store::{
    LoadedSession, SessionMeta, SessionStore, SessionSummary, SessionWriter, format_bytes,
    is_safe_session_id, keep_count, load_session_dir, load_source, max_bytes, new_session_id,
};
