//! Where Spoor keeps its state. Everything lives under one directory:
//!
//! ```text
//! {spoor_cache_dir()}/
//!   sites.toml          site registry (hand-editable)
//!   serve.json          url + bearer token of the running `spoor serve`
//!   profiles/{site}/    one persistent Chrome profile per site — the session
//!   sessions/{id}/      recordings (`spoor record`)
//!   audit/{site}.jsonl  every request an `exec` made, one line each
//!   logs/               rotating process log
//! ```

use std::path::PathBuf;

/// Cross-platform Spoor directory (`…/spoor/` under OS cache, or `~/.cache/spoor`).
///
/// Override with `SPOOR_CACHE_DIR` (absolute path). Tests use this to stay off
/// the real profiles and recordings.
pub fn spoor_cache_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("SPOOR_CACHE_DIR").filter(|s| !s.is_empty()) {
        return PathBuf::from(dir);
    }
    dirs::cache_dir()
        .map(|d| d.join("spoor"))
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache").join("spoor")))
        .unwrap_or_else(|| PathBuf::from(".cache/spoor"))
}

/// Stored recordings: `{cache}/sessions/{session_id}/`.
pub fn sessions_dir() -> PathBuf {
    spoor_cache_dir().join("sessions")
}

pub fn sites_path() -> PathBuf {
    spoor_cache_dir().join("sites.toml")
}

pub fn serve_file_path() -> PathBuf {
    spoor_cache_dir().join("serve.json")
}

/// Chrome profile for one site. Callers validate `site` with
/// [`crate::site::is_valid_name`] first, so it is a single safe path segment.
pub fn profile_dir(site: &str) -> PathBuf {
    spoor_cache_dir().join("profiles").join(site)
}

pub fn audit_path(site: &str) -> PathBuf {
    spoor_cache_dir()
        .join("audit")
        .join(format!("{site}.jsonl"))
}
