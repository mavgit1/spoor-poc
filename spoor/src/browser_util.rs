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

/// Recording-browser argv Spoor controls (not the crate-injected
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
    log::debug("recording Chrome argv:");
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

/// Isolated Chrome/Chromium profile used for recording. Persists cookies,
/// history, and challenge cookies (`cf_clearance`, etc.) across sessions.
pub fn recording_profile_dir() -> PathBuf {
    cache_dir().join("profile-record")
}

/// Delete the recording profile so the next session starts logged-out / clean.
/// Refuses if a browser currently has the profile open. Persistence is the
/// default — nothing calls this automatically.
pub fn reset_recording_profile() -> Result<()> {
    reset_profile_dir(&recording_profile_dir())
}

fn reset_profile_dir(profile: &Path) -> Result<()> {
    if profile_in_use(profile) {
        anyhow::bail!(
            "recording profile is in use at {}; close the browser first",
            profile.display()
        );
    }
    if profile.exists() {
        std::fs::remove_dir_all(profile)
            .with_context(|| format!("remove recording profile {}", profile.display()))?;
        log::info(format!("cleared recording profile {}", profile.display()));
    } else {
        log::info(format!(
            "recording profile already empty ({})",
            profile.display()
        ));
    }
    Ok(())
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
        log::debug(&format!(
            "profile in use, keeping locks: {}",
            profile.display()
        ));
        return;
    }
    tokio::fs::create_dir_all(profile).await.ok();
    for name in ["SingletonLock", "SingletonSocket", "SingletonCookie"] {
        let lock = profile.join(name);
        if tokio::fs::try_exists(&lock).await.unwrap_or(false) {
            if tokio::fs::remove_file(&lock).await.is_ok() {
                log::debug(&format!("removed stale lock {}", lock.display()));
            }
        }
    }
}

/// Prefer a real local Chrome so the recording browser is not trivially
/// fingerprinted as Chromium-for-Testing. Fetched Chromium is last resort.
///
/// Priority: `SPOOR_CHROME` (if it is a file) → platform Chrome install →
/// `BrowserFetcher` download.
pub async fn ensure_chromium() -> Result<PathBuf> {
    let env_override = std::env::var_os("SPOOR_CHROME").map(PathBuf::from);
    if let Some(path) = env_override.as_ref()
        && !path.is_file()
    {
        log::warn(format!(
            "SPOOR_CHROME is set but not a file ({}); looking for a local Chrome",
            path.display()
        ));
    }

    if let Some(path) = pick_chrome_executable(env_override.as_deref(), &local_chrome_candidates())
    {
        let via_env = env_override.as_ref().is_some_and(|p| p == &path);
        if via_env {
            log::info(format!(
                "using Chrome from SPOOR_CHROME: {}",
                path.display()
            ));
        } else {
            log::info(format!(
                "using locally installed Chrome: {}",
                path.display()
            ));
        }
        log_profile_isolation();
        return Ok(path);
    }

    log::warn(
        "no local Chrome found; falling back to fetched Chromium — fingerprinting resistance is reduced",
    );
    let executable = fetch_bundled_chromium().await?;
    log::info(format!("using Spoor Chromium: {}", executable.display()));
    log_profile_isolation();
    Ok(executable)
}

fn log_profile_isolation() {
    log::info(format!(
        "browser data isolated under {} (not your system Chrome/Safari profiles)",
        cache_dir().display()
    ));
}

fn pick_chrome_executable(
    env_override: Option<&Path>,
    local_candidates: &[PathBuf],
) -> Option<PathBuf> {
    env_override
        .filter(|path| path.is_file())
        .map(Path::to_path_buf)
        .or_else(|| local_candidates.iter().find(|p| p.is_file()).cloned())
}

#[cfg(target_os = "macos")]
fn local_chrome_candidates() -> Vec<PathBuf> {
    const REL: &[&str] = &[
        "Google Chrome.app/Contents/MacOS/Google Chrome",
        "Google Chrome Beta.app/Contents/MacOS/Google Chrome Beta",
        "Google Chrome Dev.app/Contents/MacOS/Google Chrome Dev",
        "Google Chrome Canary.app/Contents/MacOS/Google Chrome Canary",
    ];
    let mut roots = vec![PathBuf::from("/Applications")];
    if let Some(home) = dirs::home_dir() {
        roots.push(home.join("Applications"));
    }
    let mut out = Vec::with_capacity(roots.len() * REL.len());
    for root in &roots {
        for rel in REL {
            out.push(root.join(rel));
        }
    }
    out
}

#[cfg(all(unix, not(target_os = "macos")))]
fn local_chrome_candidates() -> Vec<PathBuf> {
    const NAMES: &[&str] = &[
        "google-chrome",
        "google-chrome-stable",
        "chromium",
        "chromium-browser",
    ];
    let dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    let mut found = Vec::new();
    for name in NAMES {
        if let Some(path) = dirs.iter().map(|d| d.join(name)).find(|p| p.is_file()) {
            found.push(path);
        }
    }
    found
}

#[cfg(windows)]
fn local_chrome_candidates() -> Vec<PathBuf> {
    const REL: &[&str] = &[
        r"Google\Chrome\Application\chrome.exe",
        r"Google\Chrome Beta\Application\chrome.exe",
        r"Google\Chrome Dev\Application\chrome.exe",
        r"Google\Chrome SxS\Application\chrome.exe",
    ];
    let mut roots = Vec::new();
    for key in ["PROGRAMFILES", "PROGRAMFILES(X86)", "LOCALAPPDATA"] {
        if let Some(v) = std::env::var_os(key) {
            roots.push(PathBuf::from(v));
        }
    }
    let mut out = Vec::new();
    for root in &roots {
        for rel in REL {
            out.push(root.join(rel));
        }
    }
    out
}

async fn fetch_bundled_chromium() -> Result<PathBuf> {
    let download_path = cache_dir().join("chromium");
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

/// Full-size headed browser for the user to browse the target site.
pub fn recording_config(executable: &Path) -> Result<BrowserConfig> {
    let profile = recording_profile_dir();
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
        log::debug(&format!("{label}: CDP handler started"));
        while let Some(h) = handler.next().await {
            if let Err(e) = h {
                log::warn(&format!("{label}: CDP handler error: {e:#}"));
                break;
            }
        }
        log::info(&format!(
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
    fn recording_config_builds_without_launching() {
        recording_config(Path::new("/nonexistent/chrome")).expect("config should build");
    }

    #[test]
    fn recording_profile_is_isolated_under_cache() {
        let profile = recording_profile_dir();
        assert_eq!(profile.file_name().unwrap(), "profile-record");
        assert_eq!(profile.parent().unwrap(), cache_dir());
        assert!(
            !profile.to_string_lossy().contains("Google/Chrome")
                && !profile
                    .to_string_lossy()
                    .contains("Library/Application Support/Google"),
            "must not point at the user's default Chrome profile: {}",
            profile.display()
        );
    }

    fn temp_workspace(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "spoor-browser-util-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn spoor_chrome_env_wins_over_local_candidates() {
        let dir = temp_workspace("env-wins");
        let env_bin = dir.join("spoor-chrome");
        let local_bin = dir.join("Google Chrome");
        std::fs::write(&env_bin, b"env").unwrap();
        std::fs::write(&local_bin, b"local").unwrap();

        let picked = pick_chrome_executable(Some(&env_bin), std::slice::from_ref(&local_bin));
        assert_eq!(picked.as_deref(), Some(env_bin.as_path()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_spoor_chrome_falls_through_to_local() {
        let dir = temp_workspace("env-missing");
        let missing = dir.join("nope");
        let local_bin = dir.join("chrome");
        std::fs::write(&local_bin, b"local").unwrap();

        let picked = pick_chrome_executable(Some(&missing), std::slice::from_ref(&local_bin));
        assert_eq!(picked.as_deref(), Some(local_bin.as_path()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn no_install_means_fetch_fallback() {
        let dir = temp_workspace("none");
        let missing_env = dir.join("missing-env");
        let missing_local = dir.join("missing-local");
        assert!(
            pick_chrome_executable(Some(&missing_env), &[missing_local]).is_none(),
            "none of the paths exist; caller should fetch Chromium"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reset_profile_deletes_when_idle() {
        let dir = temp_workspace("reset");
        std::fs::create_dir_all(dir.join("Default")).unwrap();
        std::fs::write(dir.join("Default").join("Cookies"), b"x").unwrap();
        reset_profile_dir(&dir).unwrap();
        assert!(!dir.exists());
        reset_profile_dir(&dir).unwrap();
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_chrome_lookup_prefers_stable_then_channels() {
        let candidates = local_chrome_candidates();
        assert_eq!(
            candidates[0],
            PathBuf::from("/Applications/Google Chrome.app/Contents/MacOS/Google Chrome")
        );
        let names: Vec<String> = candidates
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        let apps: Vec<&str> = names.iter().map(String::as_str).collect();
        assert!(apps.contains(&"Google Chrome"));
        assert!(apps.contains(&"Google Chrome Beta"));
        assert!(apps.contains(&"Google Chrome Dev"));
        assert!(apps.contains(&"Google Chrome Canary"));
    }
}
