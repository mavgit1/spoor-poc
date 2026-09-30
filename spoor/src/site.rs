//! Site registry: `sites.toml`, read fresh on every use so hand edits apply
//! without restarting `spoor serve`.
//!
//! ```toml
//! [sites.cas]
//! url = "https://cas.example.ch/"     # page an exec tab opens before running
//! check = "C:/work/cas/logged-in.js"  # optional; exec script returning true/false
//! min_gap_ms = 1000                   # optional; pacing between exec requests
//! ```
//!
//! Spoor knows nothing else about a site. Auth, CSRF, what counts as a write —
//! all of that lives in the scripts that run against it.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::cache_dir::sites_path;

pub const DEFAULT_MIN_GAP_MS: u64 = 1000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Site {
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub check: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_gap_ms: Option<u64>,
}

impl Site {
    pub fn min_gap_ms(&self) -> u64 {
        self.min_gap_ms.unwrap_or(DEFAULT_MIN_GAP_MS)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sites {
    #[serde(default)]
    pub sites: BTreeMap<String, Site>,
}

/// Site names become directory and file names, so keep them to one safe segment.
pub fn is_valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}

pub fn validate_url(url: &str) -> Result<()> {
    let parsed = url::Url::parse(url).with_context(|| format!("invalid url {url:?}"))?;
    if !matches!(parsed.scheme(), "http" | "https") {
        bail!("site url must be http(s), got {url:?}");
    }
    Ok(())
}

impl Sites {
    pub fn load() -> Result<Self> {
        Self::load_from(&sites_path())
    }

    pub fn load_from(path: &Path) -> Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(text) => toml::from_str(&text).with_context(|| format!("parse {}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e).with_context(|| format!("read {}", path.display())),
        }
    }

    pub fn save(&self) -> Result<()> {
        self.save_to(&sites_path())
    }

    pub fn save_to(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create {}", parent.display()))?;
        }
        let text = toml::to_string_pretty(self).context("serialize sites")?;
        std::fs::write(path, text).with_context(|| format!("write {}", path.display()))
    }

    pub fn get(&self, name: &str) -> Result<&Site> {
        self.sites.get(name).with_context(|| {
            format!("no site {name:?} — add it with `spoor site add {name} <url>`")
        })
    }

    pub fn add(&mut self, name: &str, site: Site) -> Result<()> {
        if !is_valid_name(name) {
            bail!("site name must be lowercase letters, digits, '-' or '_' (got {name:?})");
        }
        validate_url(&site.url)?;
        self.sites.insert(name.to_string(), site);
        Ok(())
    }

    pub fn remove(&mut self, name: &str) -> bool {
        self.sites.remove(name).is_some()
    }
}

/// Look up one site. The single entry point the runtime uses.
pub fn load_site(name: &str) -> Result<Site> {
    if !is_valid_name(name) {
        bail!("invalid site name {name:?}");
    }
    Ok(Sites::load()?.get(name)?.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_single_safe_segments() {
        for ok in ["cas", "hostpoint", "my-site_2"] {
            assert!(is_valid_name(ok), "{ok}");
        }
        for bad in ["", "Cas", "../x", "a/b", "a b", "x.y", &"a".repeat(65)] {
            assert!(!is_valid_name(bad), "{bad}");
        }
    }

    #[test]
    fn add_rejects_bad_input() {
        let mut sites = Sites::default();
        let site = |url: &str| Site {
            url: url.into(),
            check: None,
            min_gap_ms: None,
        };
        assert!(sites.add("Bad", site("https://x.test/")).is_err());
        assert!(sites.add("ok", site("ftp://x.test/")).is_err());
        assert!(sites.add("ok", site("not a url")).is_err());
        sites.add("ok", site("https://x.test/")).unwrap();
        assert_eq!(sites.get("ok").unwrap().min_gap_ms(), DEFAULT_MIN_GAP_MS);
        assert!(sites.get("missing").is_err());
    }

    #[test]
    fn round_trips_through_toml() {
        let dir = std::env::temp_dir().join(format!("spoor-sites-{}", std::process::id()));
        let path = dir.join("sites.toml");
        let mut sites = Sites::default();
        sites
            .add(
                "cas",
                Site {
                    url: "https://cas.example.test/".into(),
                    check: Some(PathBuf::from("checks/cas.js")),
                    min_gap_ms: Some(250),
                },
            )
            .unwrap();
        sites.save_to(&path).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("[sites.cas]"), "{text}");
        assert_eq!(Sites::load_from(&path).unwrap(), sites);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_file_is_empty_registry() {
        let path = std::env::temp_dir().join("spoor-definitely-missing/sites.toml");
        assert_eq!(Sites::load_from(&path).unwrap(), Sites::default());
    }
}
