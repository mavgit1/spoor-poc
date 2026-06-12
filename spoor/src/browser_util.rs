use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use chromiumoxide::Handler;
use chromiumoxide::browser::{Browser, BrowserConfig, BrowserConfigBuilder};
use chromiumoxide::fetcher::{BrowserFetcher, BrowserFetcherOptions};
use futures::StreamExt;
use tokio::sync::broadcast;
use tokio::task::JoinHandle;

use crate::log;

/// All Spoor-owned browser state lives here — never touches system Chrome profiles.
pub fn cache_dir() -> PathBuf {
    crate::cache_dir::spoor_cache_dir()
}

/// Keep bundled Chromium fully separate from the user's normal browsers.
fn apply_isolation(builder: BrowserConfigBuilder, profile_dir: PathBuf) -> BrowserConfigBuilder {
    builder
        .user_data_dir(profile_dir)
        .env("CHROME_DESKTOP", "spoor-chromium.desktop")
        .env("CHROME_WRAPPER", "spoor")
        .arg("--use-mock-keychain")
        .arg("--password-store=basic")
        .arg("--no-first-run")
        .arg("--no-default-browser-check")
        .arg("--disable-sync")
        .arg("--disable-default-apps")
        .arg("--disable-component-update")
        .arg("--no-service-autorun")
        .arg("--disable-infobars")
        .arg("--disable-features=ChromeSignin,SignInProfileCreation,Translate,MediaRouter")
}

fn panel_profile_dir() -> PathBuf {
    cache_dir().join("profile-panel")
}

fn recording_profile_dir() -> PathBuf {
    cache_dir().join("profile-record")
}

fn chromium_app_bundle(executable: &Path) -> Result<PathBuf> {
    // …/Chromium.app/Contents/MacOS/Chromium → …/Chromium.app
    let bundle = executable
        .parent()
        .and_then(|p| p.parent())
        .and_then(|p| p.parent())
        .context("locate Chromium.app from executable path")?;
    if bundle.extension().is_some_and(|e| e == "app") {
        Ok(bundle.to_path_buf())
    } else {
        anyhow::bail!("expected Chromium.app bundle, got {}", bundle.display())
    }
}

fn pick_free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .map(|l| l.local_addr().map(|a| a.port()).unwrap_or(9333))
        .unwrap_or(9333)
}

fn profile_in_use(profile: &Path) -> bool {
    let needle = profile.to_string_lossy();
    std::process::Command::new("pgrep")
        .args(["-lf", "Chromium"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .is_some_and(|s| s.contains(needle.as_ref()))
}

async fn cleanup_stale_profile_lock(profile: &Path) {
    if profile_in_use(profile) {
        log::debug(&format!("profile in use, keeping locks: {}", profile.display()));
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

fn panel_cdp_port_file() -> PathBuf {
    cache_dir().join("panel-cdp-port")
}

async fn read_saved_panel_port() -> Option<u16> {
    let content = tokio::fs::read_to_string(panel_cdp_port_file()).await.ok()?;
    content.trim().parse().ok()
}

async fn save_panel_port(port: u16) {
    let _ = tokio::fs::write(panel_cdp_port_file(), port.to_string()).await;
}

async fn clear_panel_port_file() {
    let _ = tokio::fs::remove_file(panel_cdp_port_file()).await;
}

async fn try_connect_panel_cdp(port: u16) -> Result<(Browser, Handler)> {
    wait_for_cdp_port(port).await?;
    Browser::connect(format!("http://127.0.0.1:{port}"))
        .await
        .context("connect to existing panel Chromium over CDP")
}

fn panel_chrome_args(app_url: &str, profile: &Path, port: u16) -> Vec<String> {
    vec![
        format!("--user-data-dir={}", profile.display()),
        format!("--remote-debugging-port={port}"),
        "--remote-debugging-address=127.0.0.1".into(),
        "--window-size=380,520".into(),
        "--window-position=120,80".into(),
        format!("--app={app_url}"),
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

async fn wait_for_cdp_port(port: u16) -> Result<()> {
    let url = format!("http://127.0.0.1:{port}/json/version");
    let client = reqwest::Client::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(25);
    loop {
        if tokio::time::Instant::now() >= deadline {
            anyhow::bail!("timed out waiting for Chromium CDP on port {port}");
        }
        if let Ok(res) = client.get(&url).send().await {
            if res.status().is_success() {
                log::debug(&format!("CDP ready at {url}"));
                return Ok(());
            }
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

/// macOS: launch via `open Chromium.app` so the window appears as a real GUI app.
#[cfg(target_os = "macos")]
async fn launch_panel_macos(executable: &Path, app_url: &str) -> Result<(Browser, Handler)> {
    let profile = panel_profile_dir();
    cleanup_stale_profile_lock(&profile).await;

    if profile_in_use(&profile) {
        if let Some(port) = read_saved_panel_port().await {
            log::info(&format!("reusing panel Chromium on CDP port {port}"));
            match try_connect_panel_cdp(port).await {
                Ok(pair) => return Ok(pair),
                Err(e) => log::warn(&format!("panel reconnect failed ({e:#}), launching fresh")),
            }
        }
    }

    let bundle = chromium_app_bundle(executable)?;
    let port = pick_free_port();
    log::info(&format!(
        "macOS: opening panel with {} (CDP port {port})",
        bundle.display()
    ));

    let args = panel_chrome_args(app_url, &profile, port);
    for arg in &args {
        log::debug(&format!("chromium arg: {arg}"));
    }

    std::process::Command::new("open")
        .arg(&bundle)
        .arg("--args")
        .args(&args)
        .status()
        .context("open Chromium.app")?;

    wait_for_cdp_port(port).await?;
    save_panel_port(port).await;
    let (browser, handler) = Browser::connect(format!("http://127.0.0.1:{port}"))
        .await
        .context("connect to panel Chromium over CDP")?;

    tokio::time::sleep(Duration::from_millis(500)).await;
    log::info("panel browser connected — look for a small Spoor window (top-left area)");
    Ok((browser, handler))
}

/// Direct CDP launch (non-macOS, or macOS fallback).
async fn launch_panel_direct(executable: &Path, app_url: &str) -> Result<(Browser, Handler)> {
    let profile = panel_profile_dir();
    cleanup_stale_profile_lock(&profile).await;

    let config = apply_isolation(
        BrowserConfig::builder()
            .chrome_executable(executable)
            .with_head()
            .window_size(380, 520)
            .viewport(None)
            .arg("--window-position=120,80"),
        profile,
    )
    .disable_cache()
    .request_timeout(Duration::from_secs(30))
    .build()
    .map_err(|e| anyhow::anyhow!(e))?;

    log::info("launching panel browser directly over CDP");
    let (browser, handler) = Browser::launch(config)
        .await
        .context("Browser::launch panel")?;
    browser
        .new_page(app_url)
        .await
        .context("open panel URL in browser")?;
    Ok((browser, handler))
}

async fn launch_panel(executable: &Path, app_url: &str) -> Result<(Browser, Handler)> {
    #[cfg(target_os = "macos")]
    {
        match launch_panel_macos(executable, app_url).await {
            Ok(pair) => return Ok(pair),
            Err(e) => log::warn(&format!("macOS open launch failed ({e:#}), trying direct launch")),
        }
    }
    launch_panel_direct(executable, app_url).await
}

/// Download Chromium on first run (~150MB). Reuses cached binary after that.
pub async fn ensure_chromium() -> Result<PathBuf> {
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

    log::info(&format!(
        "checking for bundled Chromium in {} …",
        download_path.display()
    ));
    let info = fetcher
        .fetch()
        .await
        .context("download/install Chromium (needs network on first run)")?;

    log::info(&format!("using Spoor Chromium: {}", info.executable_path.display()));
    log::info(&format!(
        "browser data isolated under {} (not your system Chrome/Safari profiles)",
        cache_dir().display()
    ));
    Ok(info.executable_path)
}

/// Full-size headed browser for the user to browse the target site.
pub fn recording_config(executable: &Path) -> Result<BrowserConfig> {
    apply_isolation(
        BrowserConfig::builder()
            .chrome_executable(executable)
            .with_head()
            .window_size(1280, 800)
            .viewport(None)
            .arg("--window-position=140,60"),
        recording_profile_dir(),
    )
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

/// Keep the panel browser alive until shutdown.
pub async fn run_panel_browser(
    executable: PathBuf,
    app_url: String,
    mut shutdown: broadcast::Receiver<()>,
) {
    log::info(&format!("opening panel browser at {app_url}"));
    log::debug(&format!("panel profile: {}", panel_profile_dir().display()));

    let (mut browser, handler) = match launch_panel(&executable, &app_url).await {
        Ok(b) => b,
        Err(e) => {
            log::error(&format!("could not open panel browser: {e:#}"));
            log::info(&format!("open {app_url} manually in any browser"));
            return;
        }
    };

    let mut handler_task = spawn_handler(handler, "panel");

    tokio::select! {
        _ = shutdown.recv() => {
            log::info("closing panel browser (shutdown signal)");
            if let Err(e) = browser.close().await {
                log::warn(&format!("panel browser close error: {e:#}"));
            }
            clear_panel_port_file().await;
            let _ = handler_task.await;
        }
        _ = &mut handler_task => {
            clear_panel_port_file().await;
            log::warn(&format!(
                "panel browser window closed unexpectedly — server still at {app_url}"
            ));
        }
    }
    log::info("panel browser task finished");
}
