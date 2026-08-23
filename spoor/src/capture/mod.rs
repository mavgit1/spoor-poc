mod http;
pub mod model;
mod navigation;
mod websocket;

use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64};
use std::time::Duration;

use anyhow::Result;
use chromiumoxide::Page;
use chromiumoxide::cdp::browser_protocol::network::EnableParams;
use chromiumoxide::cdp::browser_protocol::target::{
    EventAttachedToTarget, EventTargetCreated, TargetId,
};
use futures::StreamExt;
use tokio::sync::{Mutex, RwLock, mpsc};
use tokio::task::JoinSet;

pub use model::{
    Body, CaptureRecord, CapturedFlow, Direction, MAX_BINARY_BYTES, OmitReason, Transport,
    is_retainable_binary_content_type, media_omit_reason, resource_type_omit_reason,
};

use crate::log;
use crate::types::{BrowserSession, BrowsingPage};

const DEFAULT_MAX_FLOWS: usize = 10_000;

pub fn max_flows_limit() -> usize {
    std::env::var("SPOOR_MAX_FLOWS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_MAX_FLOWS)
}

fn is_page_like(kind: &str) -> bool {
    matches!(kind, "page" | "webview")
}

async fn mark_seen(seen: &Mutex<HashSet<String>>, target_id: &TargetId) -> bool {
    seen.lock().await.insert(target_id.inner().clone())
}

async fn capture_one_page(
    page: Arc<Page>,
    flows: Arc<RwLock<Vec<CaptureRecord>>>,
    flows_capped: Arc<AtomicBool>,
    sequence: Arc<AtomicU64>,
    page_urls: Arc<RwLock<Vec<BrowsingPage>>>,
    max_flows: usize,
) {
    if let Err(e) = page.execute(EnableParams::default()).await {
        log::debug(format!(
            "capture: Network.enable on {} failed: {e:#}",
            page.target_id().inner()
        ));
    }

    let http_fut = http::run(
        page.clone(),
        flows.clone(),
        flows_capped.clone(),
        sequence.clone(),
        max_flows,
    );
    let ws_fut = websocket::run(page.clone(), flows, flows_capped, sequence, max_flows);
    let nav_fut = navigation::run(page, page_urls);

    let (http_res, ws_res, nav_res) = tokio::join!(http_fut, ws_fut, nav_fut);
    if let Err(e) = http_res {
        log::debug(format!("capture/http ended: {e:#}"));
    }
    if let Err(e) = ws_res {
        log::debug(format!("capture/ws ended: {e:#}"));
    }
    if let Err(e) = nav_res {
        log::debug(format!("capture/navigation ended: {e:#}"));
    }
}

/// Upper bound on one `get_page` CDP round-trip.
///
/// `stop_handler` needs this same mutex to close the browser, and `Browser::close`
/// takes `&mut self` so the lock cannot be avoided. Holding it across an unbounded
/// await would hang Stop if CDP stalls.
const GET_PAGE_TIMEOUT: Duration = Duration::from_millis(500);

async fn resolve_page(
    session: &Mutex<Option<BrowserSession>>,
    target_id: TargetId,
) -> Option<Page> {
    for _ in 0..40 {
        {
            let guard = session.lock().await;
            // Session taken by stop_handler — the browser is going away, stop retrying.
            let s = guard.as_ref()?;
            match tokio::time::timeout(GET_PAGE_TIMEOUT, s.browser.get_page(target_id.clone()))
                .await
            {
                Ok(Ok(page)) => return Some(page),
                Ok(Err(_)) => {}
                Err(_) => log::debug(format!(
                    "capture: get_page timed out for target {}",
                    target_id.inner()
                )),
            }
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    None
}

async fn wait_for_session(session: &Mutex<Option<BrowserSession>>) -> bool {
    for _ in 0..100 {
        if session.lock().await.is_some() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    false
}

async fn watch_new_targets(
    session: Arc<Mutex<Option<BrowserSession>>>,
    page_tx: mpsc::UnboundedSender<Arc<Page>>,
) -> Result<()> {
    if !wait_for_session(&session).await {
        log::debug("capture: session not ready — watching only the initial tab");
        return Ok(());
    }

    let (mut created, mut attached, existing) = {
        let guard = session.lock().await;
        let Some(s) = guard.as_ref() else {
            return Ok(());
        };
        let created = s.browser.event_listener::<EventTargetCreated>().await?;
        let attached = s.browser.event_listener::<EventAttachedToTarget>().await?;
        let existing = s.browser.pages().await.unwrap_or_default();
        (created, attached, existing)
    };

    for page in existing {
        if page_tx.send(Arc::new(page)).is_err() {
            return Ok(());
        }
    }

    let mut created_open = true;
    let mut attached_open = true;
    loop {
        if !created_open && !attached_open {
            break;
        }
        tokio::select! {
            ev = created.next(), if created_open => {
                match ev {
                    None => created_open = false,
                    Some(ev) => {
                        if !is_page_like(&ev.target_info.r#type) {
                            continue;
                        }
                        let id = ev.target_info.target_id.clone();
                        if let Some(page) = resolve_page(&session, id).await {
                            log::debug(format!(
                                "capture: attached to new {} target {}",
                                ev.target_info.r#type,
                                page.target_id().inner()
                            ));
                            if page_tx.send(Arc::new(page)).is_err() {
                                break;
                            }
                        }
                    }
                }
            }
            ev = attached.next(), if attached_open => {
                match ev {
                    None => attached_open = false,
                    Some(ev) => {
                        if !is_page_like(&ev.target_info.r#type) {
                            continue;
                        }
                        let id = ev.target_info.target_id.clone();
                        if let Some(page) = resolve_page(&session, id).await {
                            log::debug(format!(
                                "capture: session attached to {} target {}",
                                ev.target_info.r#type,
                                page.target_id().inner()
                            ));
                            if page_tx.send(Arc::new(page)).is_err() {
                                break;
                            }
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

pub async fn capture(
    page: Arc<Page>,
    flows: Arc<RwLock<Vec<CaptureRecord>>>,
    flows_capped: Arc<AtomicBool>,
    page_urls: Arc<RwLock<Vec<BrowsingPage>>>,
    session: Arc<Mutex<Option<BrowserSession>>>,
) -> Result<()> {
    let max_flows = max_flows_limit();
    let sequence = Arc::new(AtomicU64::new(0));
    let seen = Arc::new(Mutex::new(HashSet::new()));
    let mut tasks: JoinSet<()> = JoinSet::new();

    let (page_tx, mut page_rx) = mpsc::unbounded_channel();
    let _ = page_tx.send(page);

    let watch_tx = page_tx.clone();
    let watch_session = Arc::clone(&session);
    let watch = tokio::spawn(async move {
        if let Err(e) = watch_new_targets(watch_session, watch_tx).await {
            log::debug(format!("capture/targets ended: {e:#}"));
        }
    });
    drop(page_tx);

    while let Some(page) = page_rx.recv().await {
        if !mark_seen(&seen, page.target_id()).await {
            continue;
        }
        let flows = Arc::clone(&flows);
        let flows_capped = Arc::clone(&flows_capped);
        let sequence = Arc::clone(&sequence);
        let page_urls = Arc::clone(&page_urls);
        tasks.spawn(async move {
            capture_one_page(page, flows, flows_capped, sequence, page_urls, max_flows).await;
        });
    }

    while tasks.join_next().await.is_some() {}
    let _ = watch.await;

    let count = flows.read().await.len();
    log::info(format!("capture loop ended with {count} flows stored"));
    Ok(())
}
