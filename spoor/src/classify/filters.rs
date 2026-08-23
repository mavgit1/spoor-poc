use std::path::PathBuf;

use globset::{Glob, GlobSet, GlobSetBuilder};

use crate::cache_dir;
use crate::ir::TrafficEntry;
use crate::types::Candidate;

const SKIP_METHODS: &[&str] = &["HEAD", "OPTIONS"];

const DEFAULT_PATH_IGNORES: &[&str] = &[
    "**/*.ico",
    "**/*.png",
    "**/*.jpeg",
    "**/*.jpg",
    "**/*.gif",
    "**/*.pbf",
    "**/*.woff",
    "**/*.woff2",
    "**/*.ttf",
    "**/*.svg",
    "**/xjs/**",
    "**/favicon.ico",
    "**/translations/**",
    "**/i18n/**",
    "**/locales/**",
];

const DEFAULT_GET_IGNORES: &[&str] = &["**/*.css", "**/*.js", "**/*.map"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilterAction {
    /// Append an ignore line to the user's filters file.
    Ignore,
    /// Remove matching ignore lines (undo Ignore from the panel).
    Allow,
}

pub struct FilterRegistry {
    hard_path_matcher: GlobSet,
    get_matcher: GlobSet,
    user_ignore_path_matcher: GlobSet,
    user_ignore_host_matcher: GlobSet,
}

/// Back-compat alias.
pub type IgnoreRegistry = FilterRegistry;

impl FilterRegistry {
    pub fn load() -> Self {
        let hard_path_patterns: Vec<String> = DEFAULT_PATH_IGNORES
            .iter()
            .map(|s| (*s).to_string())
            .collect();
        let get_patterns: Vec<String> = DEFAULT_GET_IGNORES
            .iter()
            .map(|s| (*s).to_string())
            .collect();
        let mut user_ignore_paths = Vec::new();
        let mut user_ignore_hosts = Vec::new();

        if let Some(content) = read_user_config() {
            for line in content.lines() {
                let line = line.trim();
                if line.is_empty() || line.starts_with('#') {
                    continue;
                }
                // Legacy allow: lines are ignored (no separate allow-list).
                if line.starts_with("allow:") {
                    continue;
                }
                if let Some(pat) = line.strip_prefix("host:") {
                    user_ignore_hosts.push(pat.trim().to_string());
                } else if let Some(pat) = line.strip_prefix("ignore:") {
                    user_ignore_paths.push(pat.trim().to_string());
                } else {
                    user_ignore_paths.push(line.to_string());
                }
            }
        }

        Self {
            hard_path_matcher: compile_globs(&hard_path_patterns),
            get_matcher: compile_globs(&get_patterns),
            user_ignore_path_matcher: compile_globs(&user_ignore_paths),
            user_ignore_host_matcher: compile_globs(&user_ignore_hosts),
        }
    }

    fn append_ignore_line(&self, pattern: &str) -> anyhow::Result<PathBuf> {
        let path = filters_config_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut content = if path.exists() {
            std::fs::read_to_string(&path)?
        } else {
            String::from(
                "# Spoor filters — panel Ignore saves here (host: / ignore:)\n\
                 # User ignore: still captured & listed, unchecked by default.\n\
                 # Hard classify rules (static assets) are built into Spoor, not this file.\n",
            )
        };
        if !content.ends_with('\n') {
            content.push('\n');
        }
        content.push_str(&format_ignore_line(pattern));
        content.push('\n');
        std::fs::write(&path, &content)?;
        Ok(path)
    }
}

pub fn filters_config_path() -> PathBuf {
    cache_dir::filters_config_path()
}

fn format_ignore_line(pattern: &str) -> String {
    let trimmed = pattern.trim();
    if trimmed.starts_with("ignore:") || trimmed.starts_with("host:") {
        trimmed.to_string()
    } else {
        format!("ignore:{trimmed}")
    }
}

/// Lines removed when the user clicks Allow (undo) for a panel pattern.
fn lines_to_remove_for_pattern(pattern: &str) -> Vec<String> {
    let ignore_line = format_ignore_line(pattern);
    let trimmed = pattern.trim();
    let mut out = vec![ignore_line.clone()];
    if let Some(host) = trimmed.strip_prefix("host:") {
        out.push(format!("ignore:host:{host}"));
    }
    if trimmed.starts_with("ignore:") {
        out.push(trimmed.strip_prefix("ignore:").unwrap_or("").to_string());
    }
    out
}

pub fn drop_ignore_lines(content: &str, pattern: &str) -> String {
    let remove: std::collections::HashSet<String> = lines_to_remove_for_pattern(pattern)
        .into_iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();

    let mut kept = Vec::new();
    for line in content.lines() {
        let t = line.trim();
        if t.is_empty() || t.starts_with('#') {
            kept.push(line);
            continue;
        }
        if remove.contains(t) {
            continue;
        }
        // Drop legacy allow: lines tied to the same host/path when undoing.
        if t.starts_with("allow:host:") {
            if let Some(host) = pattern.trim().strip_prefix("host:") {
                if t == format!("allow:host:{host}") {
                    continue;
                }
            }
        }
        kept.push(line);
    }
    let mut out = kept.join("\n");
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    out
}

fn read_user_config() -> Option<String> {
    let filters = filters_config_path();
    if filters.exists() {
        return std::fs::read_to_string(filters).ok();
    }
    let legacy = cache_dir::legacy_ignore_config_path();
    if legacy.exists() {
        let content = std::fs::read_to_string(&legacy).ok()?;
        if let Some(parent) = filters.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(&filters, &content);
        return Some(content);
    }
    None
}

fn compile_globs(patterns: &[String]) -> GlobSet {
    let mut builder = GlobSetBuilder::new();
    for pat in patterns {
        if let Ok(glob) = Glob::new(pat) {
            builder.add(glob);
        }
    }
    builder
        .build()
        .unwrap_or_else(|_| GlobSetBuilder::new().build().expect("empty glob set"))
}

/// Non-API content paths (i18n bundles, static assets) — shared by filters and REST heuristics.
pub fn is_non_api_path(path: &str) -> bool {
    is_static_asset_path(path) || is_locale_content_path(path)
}

fn is_locale_content_path(path: &str) -> bool {
    let p = path.to_lowercase();
    p.contains("/translations") || p.contains("/i18n/") || p.contains("/locales/")
}

/// Path suffix / prefix rules for static assets.
pub fn is_static_asset_path(path: &str) -> bool {
    let p = path.to_lowercase();
    p.starts_with("/_next/static/")
        || p.ends_with(".css")
        || p.ends_with(".js")
        || p.ends_with(".map")
        || p.ends_with(".woff2")
        || p.ends_with(".woff")
        || p.ends_with(".svg")
        || p.ends_with(".png")
        || p.ends_with(".ico")
        || p.contains("/icons/")
        || p.contains("/fonts/")
        || p.contains("/logos/")
        || p.contains("/scripttemplates/")
}

/// **Hard classify** — built into Spoor, not the user's filters file.
/// Drops traffic before it becomes candidates (never shown in the panel).
pub fn should_ignore(entry: &TrafficEntry, registry: &FilterRegistry) -> bool {
    let method = entry.http_method().to_uppercase();
    if SKIP_METHODS.contains(&method.as_str()) {
        return true;
    }

    if is_non_api_path(&entry.path) {
        return true;
    }

    if registry.hard_path_matcher.is_match(&entry.path) {
        return true;
    }
    if method == "GET" && registry.get_matcher.is_match(&entry.path) {
        return true;
    }

    let rt = entry.flow.resource_type.as_deref().unwrap_or("");
    if matches!(rt, "Image" | "Font" | "Stylesheet" | "Script" | "Media") {
        return true;
    }

    false
}

/// **User ignore** — from filters.yaml; still classified, listed in panel, unchecked by default.
pub fn preference_ignored(candidate: &Candidate, registry: &FilterRegistry) -> bool {
    matches_user_ignore(candidate, registry)
}

pub fn default_selected(_candidate: &Candidate, _registry: &FilterRegistry) -> bool {
    // Product law: patterns are pre-filled, not pre-selected.
    false
}

pub fn matches_user_ignore(candidate: &Candidate, registry: &FilterRegistry) -> bool {
    if registry
        .user_ignore_path_matcher
        .is_match(&candidate.guessed_pattern)
    {
        return true;
    }
    registry.user_ignore_host_matcher.is_match(&candidate.host)
}

pub fn persist_preference(pattern: &str, action: FilterAction) -> anyhow::Result<PathBuf> {
    match action {
        FilterAction::Ignore => FilterRegistry::load().append_ignore_line(pattern),
        FilterAction::Allow => remove_user_ignore(pattern),
    }
}

pub fn remove_user_ignore(pattern: &str) -> anyhow::Result<PathBuf> {
    let path = filters_config_path();
    if !path.exists() {
        return Ok(path);
    }
    let content = std::fs::read_to_string(&path)?;
    let updated = drop_ignore_lines(&content, pattern);
    std::fs::write(&path, updated)?;
    Ok(path)
}

/// Back-compat wrapper.
pub fn persist_ignore(pattern: &str) -> anyhow::Result<PathBuf> {
    persist_preference(pattern, FilterAction::Ignore)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use crate::capture::{CaptureRecord, Transport};

    use super::*;

    fn entry(path: &str, method: &str, resource_type: Option<&str>) -> TrafficEntry {
        TrafficEntry {
            flow: CaptureRecord {
                id: "1".into(),
                transport: Transport::Http,
                url: format!("https://example.com{path}"),
                method: Some(method.into()),
                request_headers: HashMap::new(),
                request_body: None,
                status: Some(200),
                response_headers: None,
                response_body: None,
                resource_type: resource_type.map(str::to_string),
                sequence: 0,
                timestamp_ms: None,
                ws_request_id: None,
                ws_opcode: None,
                direction: None,
            },
            origin: "https://example.com".into(),
            path: path.into(),
        }
    }

    fn candidate(origin: &str, host: &str, pattern: &str) -> Candidate {
        Candidate {
            id: format!("rest|{origin}|GET|{pattern}"),
            label: format!("GET {pattern}"),
            protocol: "rest".into(),
            guessed_pattern: pattern.into(),
            example: format!("GET {origin}{pattern}"),
            host: host.into(),
            methods: vec!["GET".into()],
            confidence: "parser".into(),
            origin: origin.into(),
            request_count: 1,
            default_selected: false,
            preference_ignored: false,
        }
    }

    #[test]
    fn default_globs_block_static_assets() {
        let reg = FilterRegistry::load();
        assert!(should_ignore(
            &entry("/app/chunk-abc.js", "GET", Some("Script")),
            &reg
        ));
        assert!(!should_ignore(
            &entry("/api/v1/search", "GET", Some("Fetch")),
            &reg
        ));
    }

    #[cfg(test)]
    impl FilterRegistry {
        fn test_with(user_ignore_paths: &[&str], user_ignore_hosts: &[&str]) -> Self {
            Self {
                hard_path_matcher: compile_globs(&[]),
                get_matcher: compile_globs(&[]),
                user_ignore_path_matcher: compile_globs(
                    &user_ignore_paths
                        .iter()
                        .map(|s| (*s).to_string())
                        .collect::<Vec<_>>(),
                ),
                user_ignore_host_matcher: compile_globs(
                    &user_ignore_hosts
                        .iter()
                        .map(|s| (*s).to_string())
                        .collect::<Vec<_>>(),
                ),
            }
        }
    }

    #[test]
    fn user_ignore_still_classifies_but_defaults_unchecked() {
        let reg = FilterRegistry::test_with(&["**/statistics**"], &[]);
        let traffic = entry("/web/statistics", "POST", Some("Fetch"));
        assert!(!should_ignore(&traffic, &reg));
        let cand = candidate(
            "https://api.example.test",
            "api.example.test",
            "/web/statistics",
        );
        assert!(preference_ignored(&cand, &reg));
        assert!(!default_selected(&cand, &reg));
    }

    #[test]
    fn drop_ignore_lines_removes_host_entry() {
        let content = "# filters\nhost:noise.example.test\nignore:**/other**\n";
        let out = drop_ignore_lines(content, "host:noise.example.test");
        assert!(!out.contains("noise.example.test"));
        assert!(out.contains("**/other**"));
    }

    #[test]
    fn test_format_ignore_line() {
        assert_eq!(format_ignore_line("**/stats**"), "ignore:**/stats**");
        assert_eq!(
            format_ignore_line("host:noise.example"),
            "host:noise.example"
        );
    }
}
