use std::path::PathBuf;

use globset::{Glob, GlobSet, GlobSetBuilder};
use url::Url;

use crate::cache_dir;
use crate::ir::TrafficEntry;

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

pub struct FilterRegistry {
    path_matcher: GlobSet,
    get_matcher: GlobSet,
    allow_matcher: GlobSet,
    host_matcher: GlobSet,
}

/// Back-compat alias.
pub type IgnoreRegistry = FilterRegistry;

impl FilterRegistry {
    pub fn load() -> Self {
        let mut path_patterns: Vec<String> = DEFAULT_PATH_IGNORES
            .iter()
            .map(|s| (*s).to_string())
            .collect();
        let get_patterns: Vec<String> = DEFAULT_GET_IGNORES
            .iter()
            .map(|s| (*s).to_string())
            .collect();
        let mut allow_patterns: Vec<String> = Vec::new();
        let mut host_patterns: Vec<String> = Vec::new();

        if let Some(content) = read_user_config() {
            for line in content.lines() {
                let line = line.trim();
                if line.is_empty() || line.starts_with('#') {
                    continue;
                }
                if let Some(pat) = line.strip_prefix("allow:") {
                    allow_patterns.push(pat.trim().to_string());
                } else if let Some(pat) = line.strip_prefix("host:") {
                    host_patterns.push(pat.trim().to_string());
                } else if let Some(pat) = line.strip_prefix("ignore:") {
                    path_patterns.push(pat.trim().to_string());
                } else {
                    path_patterns.push(line.to_string());
                }
            }
        }

        Self {
            path_matcher: compile_globs(&path_patterns),
            get_matcher: compile_globs(&get_patterns),
            allow_matcher: compile_globs(&allow_patterns),
            host_matcher: compile_globs(&host_patterns),
        }
    }

    pub fn append_ignore(&self, pattern: &str) -> anyhow::Result<PathBuf> {
        let path = filters_config_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut content = if path.exists() {
            std::fs::read_to_string(&path)?
        } else {
            String::from(
                "# Spoor filters — path globs, host:example.com, allow:**/api/**\n",
            )
        };
        if !content.ends_with('\n') {
            content.push('\n');
        }
        let line = if pattern.starts_with("host:") || pattern.starts_with("allow:") {
            pattern.to_string()
        } else {
            format!("ignore:{pattern}")
        };
        content.push_str(&line);
        content.push('\n');
        std::fs::write(&path, &content)?;
        Ok(path)
    }
}

pub fn filters_config_path() -> PathBuf {
    cache_dir::filters_config_path()
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
    builder.build().unwrap_or_else(|_| {
        GlobSetBuilder::new()
            .build()
            .expect("empty glob set")
    })
}

/// Non-API content paths (i18n bundles, static assets) — shared by filters and REST heuristics.
pub fn is_non_api_path(path: &str) -> bool {
    is_static_asset_path(path) || is_locale_content_path(path)
}

fn is_locale_content_path(path: &str) -> bool {
    let p = path.to_lowercase();
    p.contains("/translations")
        || p.contains("/i18n/")
        || p.contains("/locales/")
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

pub fn should_ignore(entry: &TrafficEntry, registry: &FilterRegistry) -> bool {
    if registry.allow_matcher.is_match(&entry.path) {
        return false;
    }

    let method = entry.flow.method.to_uppercase();
    if SKIP_METHODS.contains(&method.as_str()) {
        return true;
    }

    if let Ok(url) = Url::parse(&entry.flow.url) {
        if let Some(host) = url.host_str() {
            if registry.host_matcher.is_match(host) {
                return true;
            }
        }
    }

    if is_non_api_path(&entry.path) {
        return true;
    }

    if registry.path_matcher.is_match(&entry.path) {
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

pub fn persist_ignore(pattern: &str) -> anyhow::Result<PathBuf> {
    IgnoreRegistry::load().append_ignore(pattern)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use crate::types::CapturedFlow;

    use super::*;

    fn entry(path: &str, method: &str, resource_type: Option<&str>) -> TrafficEntry {
        TrafficEntry {
            flow: CapturedFlow {
                id: "1".into(),
                url: format!("https://example.com{path}"),
                method: method.into(),
                request_headers: HashMap::new(),
                request_body: None,
                status: Some(200),
                response_headers: None,
                response_body: None,
                resource_type: resource_type.map(str::to_string),
            },
            origin: "https://example.com".into(),
            path: path.into(),
        }
    }

    #[test]
    fn default_globs_block_static_assets() {
        let reg = FilterRegistry::load();
        assert!(should_ignore(
            &entry("/app/chunk-abc.js", "GET", Some("Script")),
            &reg
        ));
        assert!(should_ignore(
            &entry("/assets/i18n/de.json", "GET", Some("Fetch")),
            &reg
        ));
        assert!(should_ignore(
            &entry("/tiles/zoom.pbf", "GET", Some("Fetch")),
            &reg
        ));
        assert!(!should_ignore(
            &entry("/api/v1/search", "GET", Some("Fetch")),
            &reg
        ));
    }
}
