use std::path::PathBuf;

use anyhow::Context;
use globset::{Glob, GlobSet, GlobSetBuilder};

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
];

const DEFAULT_GET_IGNORES: &[&str] = &["**/*.css", "**/*.js", "**/*.map"];

pub struct IgnoreRegistry {
    path_matcher: GlobSet,
    get_matcher: GlobSet,
}

impl IgnoreRegistry {
    pub fn load() -> Self {
        let mut path_patterns: Vec<String> = DEFAULT_PATH_IGNORES
            .iter()
            .map(|s| (*s).to_string())
            .collect();
        let get_patterns: Vec<String> = DEFAULT_GET_IGNORES
            .iter()
            .map(|s| (*s).to_string())
            .collect();

        if let Some(user) = user_ignore_file().filter(|p| p.exists()) {
            if let Ok(content) = std::fs::read_to_string(user) {
                for line in content.lines() {
                    let line = line.trim();
                    if line.is_empty() || line.starts_with('#') {
                        continue;
                    }
                    path_patterns.push(line.to_string());
                }
            }
        }

        Self {
            path_matcher: compile_globs(&path_patterns),
            get_matcher: compile_globs(&get_patterns),
        }
    }

    pub fn append_ignore(&self, pattern: &str) -> anyhow::Result<()> {
        let path = user_ignore_file().context("HOME not set")?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut content = if path.exists() {
            std::fs::read_to_string(&path)?
        } else {
            String::from("# x-spoor-ignore patterns (one glob per line)\n")
        };
        if !content.ends_with('\n') {
            content.push('\n');
        }
        content.push_str(pattern);
        content.push('\n');
        std::fs::write(&path, content)?;
        Ok(())
    }
}

fn user_ignore_file() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache/spoor/ignore-rules.yaml"))
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

pub fn should_ignore(entry: &TrafficEntry, registry: &IgnoreRegistry) -> bool {
    let method = entry.flow.method.to_uppercase();
    if SKIP_METHODS.contains(&method.as_str()) {
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

pub fn persist_ignore(pattern: &str) -> anyhow::Result<()> {
    IgnoreRegistry::load().append_ignore(pattern)
}
