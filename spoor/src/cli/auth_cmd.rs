//! `spoor auth --surface <id>` — human logs in; Spoor stores the credential in the keychain.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use chromiumoxide::Browser;
use tokio::sync::{Mutex, RwLock};

use crate::auth::{AuthDocument, Carrier, collect_from_flow_for_carrier, handle_for, store_secret};
use crate::browser_util;
use crate::capture::{self, CaptureRecord};
use crate::log;
use crate::types::{BrowserSession, BrowsingPage};

use super::call_cmd::{load_auth_document, load_entry_url, resolve_pack_dir};

const WAIT_SECS: u64 = 15 * 60;
const POLL_MS: u64 = 250;

pub async fn run_auth(surface_id: &str, pack: Option<&Path>) -> Result<()> {
    let pack_dir = resolve_pack_dir(pack)?;
    let auth = load_auth_document(&pack_dir, surface_id)?;
    let entry_url = load_entry_url(&pack_dir, surface_id, &auth.origin)?;
    let carrier = primary_carrier(&auth).ok_or_else(|| {
        anyhow!("pack for surface {surface_id} has no observed credential carrier to watch")
    })?;

    log::info(format!(
        "opening recording browser for surface {surface_id}; watching {carrier}"
    ));
    log::info(format!("navigating to {entry_url}"));

    let executable = browser_util::ensure_chromium().await?;
    let config = browser_util::recording_config(&executable)?;
    let (browser, handler) = Browser::launch(config)
        .await
        .context("launch recording browser")?;
    let handler_task = browser_util::spawn_handler(handler, "auth");

    let page = recording_page(&browser).await?;
    let page = Arc::new(page);
    let flows: Arc<RwLock<Vec<CaptureRecord>>> = Arc::new(RwLock::new(Vec::new()));
    let flows_capped = Arc::new(AtomicBool::new(false));
    let page_urls: Arc<RwLock<Vec<BrowsingPage>>> = Arc::new(RwLock::new(Vec::new()));
    let session: Arc<Mutex<Option<BrowserSession>>> = Arc::new(Mutex::new(None));

    let capture_page = Arc::clone(&page);
    let capture_flows = Arc::clone(&flows);
    let capture_capped = Arc::clone(&flows_capped);
    let capture_pages = Arc::clone(&page_urls);
    let capture_session = Arc::clone(&session);
    let capture_task = tokio::spawn(async move {
        if let Err(e) = capture::capture(
            capture_page,
            capture_flows,
            capture_capped,
            capture_pages,
            capture_session,
        )
        .await
        {
            log::error(format!("auth capture ended: {e:#}"));
        }
    });

    *session.lock().await = Some(BrowserSession {
        browser,
        handler_task,
        capture_task,
    });

    page.goto(entry_url.as_str())
        .await
        .with_context(|| format!("navigate to {entry_url}"))?;

    let deadline = tokio::time::Instant::now() + Duration::from_secs(WAIT_SECS);
    let mut stored_handle: Option<String> = None;
    while tokio::time::Instant::now() < deadline {
        let finished = session
            .lock()
            .await
            .as_ref()
            .is_none_or(|s| s.handler_task.is_finished());
        if finished {
            break;
        }
        let snapshot = flows.read().await.clone();
        if let Some(value) = collect_from_flow_for_carrier(&snapshot, &auth.origin, &carrier) {
            let handle = handle_for(surface_id, &carrier);
            store_secret(&handle, &value)?;
            stored_handle = Some(handle);
            break;
        }
        tokio::time::sleep(Duration::from_millis(POLL_MS)).await;
    }

    if let Some(mut sess) = session.lock().await.take() {
        let _ = sess.browser.close().await;
        sess.capture_task.abort();
        sess.handler_task.abort();
    }

    let handle = stored_handle.ok_or_else(|| {
        anyhow!(
            "timed out waiting for {carrier} on origin {} (waited {WAIT_SECS}s)",
            auth.origin
        )
    })?;
    println!("{handle}");
    Ok(())
}

fn primary_carrier(auth: &AuthDocument) -> Option<Carrier> {
    auth.credentials.first().map(|c| c.carrier.clone())
}

async fn recording_page(browser: &Browser) -> Result<chromiumoxide::Page> {
    let pages = browser.pages().await.context("list tabs")?;
    if let Some(page) = pages.into_iter().next() {
        return Ok(page);
    }
    browser.new_page("about:blank").await.context("open tab")
}
