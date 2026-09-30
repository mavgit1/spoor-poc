//! The session runtime: one persistent browser per site, and the handful of
//! things you can do with it.
//!
//! | op       | what happens                                                    |
//! |----------|-----------------------------------------------------------------|
//! | `open`   | show the site's window (log in, fix things) — human-in-the-loop |
//! | `record` | same, and capture traffic into a session on disk                |
//! | `exec`   | run a script in a minimized tab of the logged-in browser           |
//! | `status` | run the site's `check` script → `logged_in: true/false`         |
//! | `stop`   | kill switch: close the site's browser, ending everything on it  |
//!
//! The browser *is* the session. Spoor never reads or refreshes cookies or
//! tokens; the profile persists them and the site's own JS maintains them.

mod exec;
mod guard;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use chromiumoxide::Browser;
use chromiumoxide::cdp::browser_protocol::browser::{
    Bounds, CloseParams, GetWindowForTargetParams, SetWindowBoundsParams, WindowState,
};
use chromiumoxide::cdp::browser_protocol::target::{
    ActivateTargetParams, CreateTargetParams, TargetId,
};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use tokio::task::JoinHandle;

pub use exec::{DEFAULT_TIMEOUT_SECS, ExecOutcome, ExecRequest};
pub use guard::{Audit, Pacer};

use crate::browser_util::{self, spawn_handler};
use crate::cache_dir::{audit_path, profile_dir};
use crate::log;
use crate::session::{Recording, SessionStore};
use crate::site::{Site, load_site};

const STATUS_TIMEOUT_SECS: u64 = 60;
const WAIT_POLL: Duration = Duration::from_secs(5);

/// A running browser on one site's profile.
pub struct SiteBrowser {
    browser: Arc<Browser>,
    handler: JoinHandle<()>,
    pacer: Arc<Pacer>,
    audit: Arc<Audit>,
    recording: Mutex<Option<Recording>>,
}

impl SiteBrowser {
    async fn launch(executable: &std::path::Path, name: &str) -> Result<Self> {
        let profile = profile_dir(name);
        browser_util::cleanup_stale_profile_lock(&profile).await;
        let config = browser_util::site_browser_config(executable, profile)?;
        let (browser, handler) = Browser::launch(config)
            .await
            .with_context(|| format!("launch browser for {name}"))?;
        log::info(format!("{name}: browser launched"));
        Ok(Self {
            browser: Arc::new(browser),
            handler: spawn_handler(handler, "site"),
            pacer: Arc::new(Pacer::default()),
            audit: Arc::new(Audit::new(audit_path(name))),
            recording: Mutex::new(None),
        })
    }

    /// False once the user closed the last window or Chrome crashed.
    fn is_alive(&self) -> bool {
        !self.handler.is_finished()
    }

    async fn set_window_state(&self, target: TargetId, state: WindowState) -> Result<()> {
        let window = self
            .browser
            .execute(GetWindowForTargetParams {
                target_id: Some(target),
            })
            .await?
            .result
            .window_id;
        let bounds = Bounds {
            window_state: Some(state),
            ..Default::default()
        };
        self.browser
            .execute(SetWindowBoundsParams::new(window, bounds))
            .await?;
        Ok(())
    }

    /// Minimize whatever window Chrome opened at launch, so a browser started
    /// only to run execs stays out of the way.
    async fn minimize_initial_window(&self) {
        if let Ok(pages) = self.browser.pages().await
            && let Some(page) = pages.first()
            && let Err(e) = self
                .set_window_state(page.target_id().clone(), WindowState::Minimized)
                .await
        {
            log::debug(format!("minimize failed: {e:#}"));
        }
    }

    /// Open `url` in a visible tab and bring its window to the front.
    async fn show(&self, url: &str) -> Result<()> {
        let target = self
            .browser
            .execute(CreateTargetParams::new(url))
            .await
            .context("open tab")?
            .result
            .target_id;
        self.set_window_state(target.clone(), WindowState::Normal)
            .await
            .ok();
        self.browser
            .execute(ActivateTargetParams::new(target))
            .await
            .ok();
        Ok(())
    }

    async fn close(&self) {
        let recording = self.recording.lock().await.take();
        if let Some(rec) = recording
            && let Err(e) = rec.stop().await
        {
            log::warn(format!("finalizing recording on stop failed: {e:#}"));
        }
        if let Err(e) = self.browser.execute(CloseParams::default()).await {
            log::debug(format!("browser close: {e}"));
        }
        self.handler.abort();
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SiteInfo {
    pub name: String,
    pub url: String,
    #[serde(default)]
    pub check: Option<PathBuf>,
    pub min_gap_ms: u64,
    pub running: bool,
    #[serde(default)]
    pub recording: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SiteStatus {
    pub site: String,
    pub running: bool,
    #[serde(default)]
    pub recording: Option<String>,
    /// `None` when the site has no `check` script.
    #[serde(default)]
    pub logged_in: Option<bool>,
    #[serde(default)]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordInfo {
    pub site: String,
    pub session_id: String,
    pub dir: PathBuf,
    pub flow_count: usize,
}

/// JS truthiness for a check result, so `return me.id` works as well as `return true`.
fn truthy(v: &serde_json::Value) -> bool {
    match v {
        serde_json::Value::Null => false,
        serde_json::Value::Bool(b) => *b,
        serde_json::Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        serde_json::Value::String(s) => !s.is_empty(),
        _ => true,
    }
}

pub struct Runtime {
    executable: PathBuf,
    sites: Mutex<HashMap<String, Arc<SiteBrowser>>>,
    exec_seq: AtomicU64,
    store: SessionStore,
}

impl Runtime {
    pub fn new(executable: PathBuf) -> Self {
        Self::with_store(executable, SessionStore::default_store())
    }

    pub fn with_store(executable: PathBuf, store: SessionStore) -> Self {
        Self {
            executable,
            sites: Mutex::new(HashMap::new()),
            exec_seq: AtomicU64::new(0),
            store,
        }
    }

    async fn running(&self, name: &str) -> Option<Arc<SiteBrowser>> {
        let sites = self.sites.lock().await;
        sites.get(name).filter(|b| b.is_alive()).cloned()
    }

    /// The site's browser, launching it if needed. `visible: false` minimizes a
    /// freshly launched window; an already-open window is left as it is.
    async fn browser_for(&self, name: &str, visible: bool) -> Result<Arc<SiteBrowser>> {
        let mut sites = self.sites.lock().await;
        if let Some(existing) = sites.get(name) {
            if existing.is_alive() {
                return Ok(Arc::clone(existing));
            }
            log::info(format!("{name}: browser was closed; relaunching"));
            sites.remove(name);
        }
        let browser = Arc::new(SiteBrowser::launch(&self.executable, name).await?);
        if !visible {
            browser.minimize_initial_window().await;
        }
        sites.insert(name.to_string(), Arc::clone(&browser));
        Ok(browser)
    }

    pub async fn list(&self) -> Result<Vec<SiteInfo>> {
        let sites = crate::site::Sites::load()?;
        let mut out = Vec::new();
        for (name, site) in sites.sites {
            let running = self.running(&name).await;
            let recording = match &running {
                Some(b) => b
                    .recording
                    .lock()
                    .await
                    .as_ref()
                    .map(|r| r.session_id.clone()),
                None => None,
            };
            out.push(SiteInfo {
                min_gap_ms: site.min_gap_ms(),
                name,
                url: site.url,
                check: site.check,
                running: running.is_some(),
                recording,
            });
        }
        Ok(out)
    }

    pub async fn open(&self, name: &str, url: Option<&str>) -> Result<()> {
        let site = load_site(name)?;
        let url = url.unwrap_or(&site.url);
        let browser = self.browser_for(name, true).await?;
        browser.show(url).await
    }

    /// Open the window and wait until the site's check passes — the re-login
    /// primitive an integration calls when it sees it was logged out.
    pub async fn open_and_wait(
        &self,
        name: &str,
        url: Option<&str>,
        timeout: Duration,
    ) -> Result<bool> {
        let site = load_site(name)?;
        if site.check.is_none() {
            bail!("site {name} has no check script, so there is nothing to wait for");
        }
        self.open(name, url).await?;
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            if self.status(name).await?.logged_in == Some(true) {
                return Ok(true);
            }
            if tokio::time::Instant::now() + WAIT_POLL > deadline {
                return Ok(false);
            }
            tokio::time::sleep(WAIT_POLL).await;
        }
    }

    pub async fn exec(&self, name: &str, req: ExecRequest) -> Result<ExecOutcome> {
        let site = load_site(name)?;
        self.exec_on(name, &site, req).await
    }

    async fn exec_on(&self, name: &str, site: &Site, req: ExecRequest) -> Result<ExecOutcome> {
        let url = req.url.clone().unwrap_or_else(|| site.url.clone());
        crate::site::validate_url(&url)?;
        let browser = self.browser_for(name, false).await?;
        let seq = self.exec_seq.fetch_add(1, Ordering::SeqCst);
        let exec_id = format!(
            "{}-{seq}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis())
                .unwrap_or(0)
        );
        let env = exec::ExecEnv {
            browser: &browser.browser,
            pacer: &browser.pacer,
            audit: &browser.audit,
            gap: Duration::from_millis(site.min_gap_ms()),
        };
        Ok(exec::run(env, &exec_id, &url, &req).await)
    }

    pub async fn status(&self, name: &str) -> Result<SiteStatus> {
        let site = load_site(name)?;
        let mut status = SiteStatus {
            site: name.to_string(),
            running: false,
            recording: None,
            logged_in: None,
            error: None,
        };
        if let Some(check) = &site.check {
            let script = std::fs::read_to_string(check)
                .with_context(|| format!("read check script {}", check.display()))?;
            let req = ExecRequest {
                script,
                timeout_secs: Some(STATUS_TIMEOUT_SECS),
                ..Default::default()
            };
            let outcome = self.exec_on(name, &site, req).await?;
            if outcome.ok {
                status.logged_in = Some(truthy(&outcome.result));
            } else {
                status.logged_in = Some(false);
                status.error = outcome.error;
            }
        }
        if let Some(browser) = self.running(name).await {
            status.running = true;
            status.recording = browser
                .recording
                .lock()
                .await
                .as_ref()
                .map(|r| r.session_id.clone());
        }
        Ok(status)
    }

    pub async fn record_start(&self, name: &str, url: Option<&str>) -> Result<RecordInfo> {
        let site = load_site(name)?;
        let browser = self.browser_for(name, true).await?;
        let mut slot = browser.recording.lock().await;
        if let Some(rec) = slot.as_ref() {
            bail!("{name} is already recording ({})", rec.session_id);
        }
        let rec = Recording::start(Arc::clone(&browser.browser), name, self.store.clone())?;
        let info = RecordInfo {
            site: name.to_string(),
            session_id: rec.session_id.clone(),
            dir: rec.dir.clone(),
            flow_count: 0,
        };
        *slot = Some(rec);
        drop(slot);
        // Give capture a moment to attach to existing tabs before the first load.
        tokio::time::sleep(Duration::from_millis(300)).await;
        browser.show(url.unwrap_or(&site.url)).await?;
        Ok(info)
    }

    pub async fn record_stop(&self, name: &str) -> Result<RecordInfo> {
        let browser = {
            let sites = self.sites.lock().await;
            sites.get(name).cloned()
        }
        .ok_or_else(|| anyhow!("{name} is not recording"))?;
        let rec = browser
            .recording
            .lock()
            .await
            .take()
            .ok_or_else(|| anyhow!("{name} is not recording"))?;
        let (session_id, dir) = (rec.session_id.clone(), rec.dir.clone());
        let meta = rec.stop().await?;
        Ok(RecordInfo {
            site: name.to_string(),
            session_id,
            dir,
            flow_count: meta.map(|m| m.flow_count).unwrap_or(0),
        })
    }

    /// Kill switch for one site. Returns whether a browser was running.
    pub async fn stop(&self, name: &str) -> bool {
        let browser = self.sites.lock().await.remove(name);
        match browser {
            Some(b) => {
                b.close().await;
                log::info(format!("{name}: stopped"));
                true
            }
            None => false,
        }
    }

    pub async fn stop_all(&self) -> Vec<String> {
        let all: Vec<(String, Arc<SiteBrowser>)> = self.sites.lock().await.drain().collect();
        let mut names = Vec::new();
        for (name, browser) in all {
            browser.close().await;
            names.push(name);
        }
        names
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn truthiness_matches_js() {
        for v in [json!(true), json!(1), json!("x"), json!({}), json!([])] {
            assert!(truthy(&v), "{v}");
        }
        for v in [json!(false), json!(0), json!(""), json!(null)] {
            assert!(!truthy(&v), "{v}");
        }
    }
}
