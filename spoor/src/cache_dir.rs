use std::path::PathBuf;

/// Cross-platform Spoor config/cache directory (`…/spoor/` under OS cache, or `~/.cache/spoor`).
pub fn spoor_cache_dir() -> PathBuf {
    dirs::cache_dir()
        .map(|d| d.join("spoor"))
        .or_else(|| {
            std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache").join("spoor"))
        })
        .unwrap_or_else(|| PathBuf::from(".cache/spoor"))
}

pub fn filters_config_path() -> PathBuf {
    spoor_cache_dir().join("filters.yaml")
}

/// Legacy path; read on migrate only.
pub fn legacy_ignore_config_path() -> PathBuf {
    spoor_cache_dir().join("ignore-rules.yaml")
}
