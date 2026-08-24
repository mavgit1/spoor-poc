use std::path::PathBuf;

/// Cross-platform Spoor config/cache directory (`…/spoor/` under OS cache, or `~/.cache/spoor`).
///
/// Override with `SPOOR_CACHE_DIR` (absolute path). Used by tests and by
/// `spoor sessions` / `spoor replay` when pointing at a throwaway cache.
pub fn spoor_cache_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("SPOOR_CACHE_DIR").filter(|s| !s.is_empty()) {
        return PathBuf::from(dir);
    }
    dirs::cache_dir()
        .map(|d| d.join("spoor"))
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache").join("spoor")))
        .unwrap_or_else(|| PathBuf::from(".cache/spoor"))
}

/// Stored capture sessions: `{cache}/sessions/{session_id}/`.
pub fn sessions_dir() -> PathBuf {
    spoor_cache_dir().join("sessions")
}

pub fn filters_config_path() -> PathBuf {
    spoor_cache_dir().join("filters.yaml")
}

/// Legacy path; read on migrate only.
pub fn legacy_ignore_config_path() -> PathBuf {
    spoor_cache_dir().join("ignore-rules.yaml")
}
