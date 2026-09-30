use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use chromiumoxide::Handler;
use chromiumoxide::browser::{BrowserConfig, BrowserConfigBuilder};
use chromiumoxide::fetcher::{BrowserFetcher, BrowserFetcherOptions};
use futures::StreamExt;
use tokio::task::JoinHandle;

use crate::log;

/// All Spoor-owned browser state lives here — never touches system Chrome profiles.
pub fn cache_dir() -> PathBuf {
    crate::cache_dir::spoor_cache_dir()
}

/// Isolation flags for a Spoor-owned profile. These are the "benign defaults"
/// we keep after dropping chromiumoxide `DEFAULT_ARGS` (which include
/// `--enable-automation` and other tells a real browser never ships).
///
/// Formatted as final argv tokens (`--flag` / `--key=value`) so tests can
/// assert on them. `apply_chrome_args` strips the leading `--` before handing
/// them to `BrowserConfigBuilder` — the crate's `From<&str> for Arg` treats the
/// whole string as a key and would otherwise emit `---flag`.
fn isolation_chrome_args() -> Vec<String> {
    vec![
        "--use-mock-keychain".into(),
        "--password-store=basic".into(),
        "--no-first-run".into(),
        "--no-default-browser-check".into(),
        "--disable-sync".into(),
        "--disable-default-apps".into(),
        "--disable-component-update".into(),
        "--no-service-autorun".into(),
        "--disable-infobars".into(),
        "--disable-features=ChromeSignin,SignInProfileCreation,Translate,MediaRouter".into(),
    ]
}

/// Site-browser argv Spoor controls (not the crate-injected
/// `--remote-debugging-port`, `--user-data-dir`, `--window-size`,
/// `--disable-extensions`).
fn recording_chrome_args(disable_dev_shm: bool) -> Vec<String> {
    let mut args = isolation_chrome_args();
    args.push("--disable-blink-features=AutomationControlled".into());
    args.push("--window-position=140,60".into());
    if disable_dev_shm {
        args.push("--disable-dev-shm-usage".into());
    }
    args
}

fn apply_chrome_args(mut builder: BrowserConfigBuilder, args: &[String]) -> BrowserConfigBuilder {
    for raw in args {
        let stripped = raw.strip_prefix("--").unwrap_or(raw.as_str());
        builder = if let Some((key, value)) = stripped.split_once('=') {
            builder.arg((key, value))
        } else {
            builder.arg(stripped)
        };
    }
    builder
}

/// Tiny `/dev/shm` (typical in containers) makes Chrome crash; desktop
/// machines should not get this flag — it is itself an automation tell.
fn needs_disable_dev_shm() -> bool {
    #[cfg(target_os = "linux")]
    {
        std::path::Path::new("/.dockerenv").exists()
            || std::fs::read_to_string("/proc/1/cgroup").is_ok_and(|cgroup| {
                cgroup.contains("docker")
                    || cgroup.contains("lxc")
                    || cgroup.contains("containerd")
                    || cgroup.contains("kubepods")
            })
    }
    #[cfg(not(target_os = "linux"))]
    {
        false
    }
}

fn log_recording_argv(args: &[String], profile: &Path) {
    log::debug("site Chrome argv:");
    for arg in args {
        log::debug(format!("  {arg}"));
    }
    log::debug(format!("  --user-data-dir={}", profile.display()));
    log::debug("  --window-size=1280,800");
    log::debug("  --remote-debugging-port=<ephemeral>  (chromiumoxide)");
    log::debug("  --disable-extensions  (chromiumoxide, no extensions loaded)");
}

/// Keep the launched browser fully separate from the user's normal profiles.
///
/// `disable_default_args()` drops chromiumoxide 0.9.1 `DEFAULT_ARGS`, which
/// hardcode `--enable-automation` (sets `navigator.webdriver === true`).
/// `hide()` adds `--disable-blink-features=AutomationControlled`.
/// `respect_https_errors()` undoes the crate default of ignoring certificate
/// errors: captured traffic must be trustworthy evidence, and a silent cert
/// bypass would let a MITM'd session look clean in the pack.
fn apply_isolation(builder: BrowserConfigBuilder, profile_dir: PathBuf) -> BrowserConfigBuilder {
    apply_chrome_args(
        builder
            .user_data_dir(profile_dir)
            .env("CHROME_DESKTOP", "spoor-chromium.desktop")
            .env("CHROME_WRAPPER", "spoor")
            .disable_default_args()
            .respect_https_errors()
            .hide(),
        &isolation_chrome_args(),
    )
}

fn profile_in_use(profile: &Path) -> bool {
    let needle = profile.to_string_lossy();
    // Match Google Chrome *and* Chromium; pgrep for the profile path itself
    // would also match the pgrep process (its argv contains the needle).
    ["Google Chrome", "Chromium", "chrome"]
        .iter()
        .any(|pattern| {
            std::process::Command::new("pgrep")
                .args(["-lf", pattern])
                .output()
                .ok()
                .and_then(|o| String::from_utf8(o.stdout).ok())
                .is_some_and(|s| s.contains(needle.as_ref()))
        })
}

pub async fn cleanup_stale_profile_lock(profile: &Path) {
    if profile_in_use(profile) {
        log::debug(format!(
            "profile in use, keeping locks: {}",
            profile.display()
        ));
        return;
    }
    tokio::fs::create_dir_all(profile).await.ok();
    for name in ["SingletonLock", "SingletonSocket", "SingletonCookie"] {
        let lock = profile.join(name);
        if tokio::fs::try_exists(&lock).await.unwrap_or(false)
            && tokio::fs::remove_file(&lock).await.is_ok()
        {
            log::debug(format!("removed stale lock {}", lock.display()));
        }
    }
}

/// The browser every site runs in: one pinned Chromium build, downloaded on
/// first use, so behaviour is the same on every machine. The revision is the
/// chromiumoxide fetcher's default, pinned by `Cargo.lock`.
///
/// `SPOOR_CHROME=<path>` overrides it (e.g. a real Chrome install).
///
/// Anti-detection does not depend on the binary: the launch drops
/// `--enable-automation` (no `navigator.webdriver`), adds
/// `--disable-blink-features=AutomationControlled`, and runs headed.
pub async fn ensure_chromium() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("SPOOR_CHROME").map(PathBuf::from) {
        if path.is_file() {
            log::info(format!(
                "using browser from SPOOR_CHROME: {}",
                path.display()
            ));
            return Ok(path);
        }
        log::warn(format!(
            "SPOOR_CHROME is set but not a file ({}); using the pinned Chromium",
            path.display()
        ));
    }
    let executable = fetch_bundled_chromium().await?;
    log::debug(format!("using pinned Chromium: {}", executable.display()));
    Ok(executable)
}

/// Where the pinned Chromium is unpacked. Deliberately *not* under
/// `SPOOR_CACHE_DIR`: that isolates state (profiles, recordings), and a test
/// pointing it at a temp dir should not re-download a 150 MB browser.
/// `SPOOR_CHROMIUM_DIR` overrides.
fn chromium_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("SPOOR_CHROMIUM_DIR").filter(|s| !s.is_empty()) {
        return PathBuf::from(dir);
    }
    dirs::cache_dir()
        .map(|d| d.join("spoor").join("chromium"))
        .unwrap_or_else(|| cache_dir().join("chromium"))
}

async fn fetch_bundled_chromium() -> Result<PathBuf> {
    let download_path = chromium_dir();
    tokio::fs::create_dir_all(&download_path)
        .await
        .with_context(|| format!("create {}", download_path.display()))?;

    let fetcher = BrowserFetcher::new(
        BrowserFetcherOptions::builder()
            .with_path(&download_path)
            .build()
            .context("fetcher options")?,
    );

    log::info(format!(
        "checking for bundled Chromium in {} …",
        download_path.display()
    ));
    let info = fetcher
        .fetch()
        .await
        .context("download/install Chromium (needs network on first run)")?;
    Ok(info.executable_path)
}

/// Headed browser on a site's own persistent profile.
///
/// One profile per site ([`crate::cache_dir::profile_dir`]) is the whole
/// session model: cookies, storage and "remember this device" persist there,
/// and Chromium's per-profile `SingletonLock` means one process owns it.
/// Tests pass a throwaway profile so they never touch real logins.
pub fn site_browser_config(executable: &Path, profile: PathBuf) -> Result<BrowserConfig> {
    let disable_dev_shm = needs_disable_dev_shm();
    let args = recording_chrome_args(disable_dev_shm);
    log_recording_argv(&args, &profile);

    let mut builder = apply_isolation(
        BrowserConfig::builder()
            .chrome_executable(executable)
            .with_head()
            .window_size(1280, 800)
            .viewport(None),
        profile,
    );
    builder = builder.arg(("window-position", "140,60"));
    if disable_dev_shm {
        builder = builder.arg("disable-dev-shm-usage");
    }
    builder
        .request_timeout(Duration::from_secs(30))
        .build()
        .map_err(|e| anyhow::anyhow!(e))
}

pub fn spawn_handler(mut handler: Handler, label: &'static str) -> JoinHandle<()> {
    tokio::spawn(async move {
        log::debug(format!("{label}: CDP handler started"));
        while let Some(h) = handler.next().await {
            if let Err(e) = h {
                log::warn(format!("{label}: CDP handler error: {e:#}"));
                break;
            }
        }
        log::info(format!(
            "{label}: CDP connection ended (window closed or browser crashed)"
        ));
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn has_flag(args: &[String], flag: &str) -> bool {
        args.iter().any(|a| a == flag)
    }

    #[test]
    fn recording_args_omit_automation_tells() {
        let args = recording_chrome_args(false);
        for flag in [
            "--enable-automation",
            "--enable-blink-features=IdleDetection",
            "--metrics-recording-only",
            "--lang=en_US",
            "--disable-dev-shm-usage",
            "--no-sandbox",
        ] {
            assert!(
                !has_flag(&args, flag),
                "recording argv must not contain {flag}: {args:?}"
            );
        }
        assert!(
            !args.iter().any(|a| a == "--enable-automation"
                || a.starts_with("--enable-automation=")
                || a.contains("IdleDetection")
                || a.starts_with("--lang=")),
            "recording argv leaked an automation/locale override: {args:?}"
        );
        assert!(
            args.iter()
                .all(|a| a.starts_with("--") && !a.starts_with("---")),
            "argv tokens must be real --flags, not ---flags: {args:?}"
        );
    }

    #[test]
    fn recording_args_include_stealth_and_isolation() {
        let args = recording_chrome_args(false);
        for flag in [
            "--disable-blink-features=AutomationControlled",
            "--no-first-run",
            "--no-default-browser-check",
            "--disable-sync",
            "--use-mock-keychain",
            "--password-store=basic",
            "--disable-default-apps",
            "--disable-component-update",
            "--no-service-autorun",
            "--window-position=140,60",
        ] {
            assert!(
                has_flag(&args, flag),
                "recording argv missing {flag}: {args:?}"
            );
        }
    }

    #[test]
    fn disable_dev_shm_only_when_requested() {
        assert!(!has_flag(
            &recording_chrome_args(false),
            "--disable-dev-shm-usage"
        ));
        assert!(has_flag(
            &recording_chrome_args(true),
            "--disable-dev-shm-usage"
        ));
    }

    #[test]
    fn site_config_builds_without_launching() {
        site_browser_config(Path::new("/nonexistent/chrome"), std::env::temp_dir())
            .expect("config should build");
    }
}
