//! Live CDP smoke test — exercises the real capture path against a local server.
//!
//! The unit tests cover capture's pure logic with synthetic input. This one
//! actually launches Chromium and asserts on what CDP delivers, which is the
//! only way to catch multi-target attach, redirect finalization and body
//! retrieval breaking.
//!
//! Ignored by default: it launches a headed browser and takes ~15s.
//!
//!   cargo test --test smoke_capture -- --ignored --nocapture

use std::collections::HashSet;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use axum::response::{Html, Redirect};
use axum::routing::get;
use axum::{Json, Router};
use chromiumoxide::Browser;
use spoor::browser_util;
use spoor::capture::{self, CaptureRecord};
use spoor::types::{BrowserSession, BrowsingPage};
use tokio::sync::{Mutex, RwLock};

const INDEX_HTML: &str = r#"<!doctype html>
<html><body><h1>spoor smoke</h1><script>
  fetch('/api/thing').then(r => r.json()).then(d => {
    window.__thing = d;
  });
</script></body></html>"#;

const POPUP_HTML: &str = r#"<!doctype html>
<html><body><h1>popup</h1><script>
  fetch('/api/from-popup').then(r => r.json());
</script></body></html>"#;

async fn spawn_server() -> SocketAddr {
    let app = Router::new()
        .route("/", get(|| async { Html(INDEX_HTML) }))
        .route("/popup", get(|| async { Html(POPUP_HTML) }))
        .route(
            "/api/thing",
            get(|| async { Json(serde_json::json!({ "thing": "ok", "id": "thing-1" })) }),
        )
        .route(
            "/api/from-popup",
            get(|| async { Json(serde_json::json!({ "from": "popup" })) }),
        )
        .route("/hop1", get(|| async { Redirect::temporary("/hop2") }))
        .route("/hop2", get(|| async { Redirect::temporary("/api/final") }))
        .route(
            "/api/final",
            get(|| async { Json(serde_json::json!({ "final": true })) }),
        );

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind test server");
    let addr = listener.local_addr().expect("local addr");
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    addr
}

fn path_of(flow: &CaptureRecord) -> String {
    url::Url::parse(&flow.url)
        .map(|u| u.path().to_string())
        .unwrap_or_default()
}

fn find<'a>(flows: &'a [CaptureRecord], path: &str) -> Option<&'a CaptureRecord> {
    flows.iter().find(|f| path_of(f) == path)
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "launches a real browser"]
async fn captures_redirects_bodies_and_second_target() {
    let addr = spawn_server().await;
    let base = format!("http://{addr}");

    let chromium = browser_util::ensure_chromium()
        .await
        .expect("chromium available");
    let config = browser_util::recording_config(&chromium).expect("recording config");
    let (browser, handler) = Browser::launch(config).await.expect("launch browser");
    let handler_task = browser_util::spawn_handler(handler, "smoke");

    let page = Arc::new(
        browser
            .new_page("about:blank")
            .await
            .expect("open first tab"),
    );

    let flows: Arc<RwLock<Vec<CaptureRecord>>> = Arc::new(RwLock::new(Vec::new()));
    let flows_capped = Arc::new(AtomicBool::new(false));
    let page_urls: Arc<RwLock<Vec<BrowsingPage>>> = Arc::new(RwLock::new(Vec::new()));

    // Mirrors ui::start_handler: the session must be stored before capture runs,
    // because capture watches it for newly created targets.
    let session: Arc<Mutex<Option<BrowserSession>>> = Arc::new(Mutex::new(None));
    *session.lock().await = Some(BrowserSession {
        browser,
        handler_task,
        capture_task: tokio::spawn(async {}),
    });

    let capture_task = tokio::spawn({
        let (page, flows, flows_capped, page_urls, session) = (
            Arc::clone(&page),
            Arc::clone(&flows),
            Arc::clone(&flows_capped),
            Arc::clone(&page_urls),
            Arc::clone(&session),
        );
        async move {
            let _ = capture::capture(page, flows, flows_capped, page_urls, session).await;
        }
    });

    tokio::time::sleep(Duration::from_millis(500)).await;

    page.goto(&base).await.expect("navigate to index");
    tokio::time::sleep(Duration::from_millis(1200)).await;

    page.goto(format!("{base}/hop1"))
        .await
        .expect("navigate redirect chain");
    tokio::time::sleep(Duration::from_millis(1200)).await;

    // Second target: exercises the Target.targetCreated path that new tabs and
    // window.open popups both go through.
    let popup = {
        let guard = session.lock().await;
        let browser = &guard.as_ref().expect("session present").browser;
        browser
            .new_page("about:blank")
            .await
            .expect("open popup tab")
    };
    // Give capture time to attach before the target issues any request, so a
    // failure here means "never attached" rather than "attached too late".
    tokio::time::sleep(Duration::from_millis(1500)).await;
    popup
        .goto(format!("{base}/popup"))
        .await
        .expect("navigate popup tab");
    tokio::time::sleep(Duration::from_millis(2000)).await;

    let mut taken = session.lock().await.take().expect("session present");
    taken.browser.close().await.ok();
    let _ = capture_task.await;
    let _ = taken.handler_task.await;

    let flows = flows.read().await.clone();
    let paths: Vec<String> = flows.iter().map(path_of).collect();
    eprintln!("captured {} flows: {paths:?}", flows.len());

    let thing = find(&flows, "/api/thing").expect("captured /api/thing");
    let body = thing.text_response().expect("json response body retained");
    assert!(
        body.contains("thing-1"),
        "expected response body to survive capture, got: {body}"
    );

    // Redirect hops were previously overwritten by the final hop and lost entirely.
    let hop1 = find(&flows, "/hop1").expect("captured redirect hop /hop1");
    let hop2 = find(&flows, "/hop2").expect("captured redirect hop /hop2");
    assert_eq!(hop1.status, Some(307), "redirect hop keeps its own status");
    assert_eq!(hop2.status, Some(307));
    assert!(find(&flows, "/api/final").is_some(), "captured final hop");

    // Traffic from a second target was previously never captured.
    assert!(
        find(&flows, "/api/from-popup").is_some(),
        "expected second-target traffic, captured paths: {paths:?}"
    );

    let ids: HashSet<&str> = flows.iter().map(|f| f.id.as_str()).collect();
    assert_eq!(
        ids.len(),
        flows.len(),
        "flow ids must stay unique across redirect hops and targets"
    );

    let seqs: HashSet<u64> = flows.iter().map(|f| f.sequence).collect();
    assert_eq!(
        seqs.len(),
        flows.len(),
        "sequence must be unique across targets — export infers ordering from it"
    );
}

/// Known gap: a target that navigates the instant it is created loses its
/// opening requests, because capture only attaches after `Target.targetCreated`
/// and then has to resolve the page and enable `Network`.
///
/// The sibling test above sidesteps this by pausing before navigating. This one
/// does not, and it is the shape that matters in practice: an OAuth popup opens
/// straight onto the provider's authorize URL, so the document request and any
/// immediate redirect are exactly what we drop.
///
/// The real fix is browser-level `Target.setAutoAttach` with
/// `waitForDebuggerOnStart`, resuming via `Runtime.runIfWaitingForDebugger`
/// once `Network` is enabled on the new session. chromiumoxide's handler does
/// not send that resume for browser-level attachments, so it needs to be driven
/// manually.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "known gap: early requests on a self-navigating new target are missed"]
async fn captures_first_request_of_a_self_navigating_target() {
    let addr = spawn_server().await;
    let base = format!("http://{addr}");

    let chromium = browser_util::ensure_chromium()
        .await
        .expect("chromium available");
    let config = browser_util::recording_config(&chromium).expect("recording config");
    let (browser, handler) = Browser::launch(config).await.expect("launch browser");
    let handler_task = browser_util::spawn_handler(handler, "smoke-race");

    let page = Arc::new(
        browser
            .new_page("about:blank")
            .await
            .expect("open first tab"),
    );
    let flows: Arc<RwLock<Vec<CaptureRecord>>> = Arc::new(RwLock::new(Vec::new()));
    let flows_capped = Arc::new(AtomicBool::new(false));
    let page_urls: Arc<RwLock<Vec<BrowsingPage>>> = Arc::new(RwLock::new(Vec::new()));

    let session: Arc<Mutex<Option<BrowserSession>>> = Arc::new(Mutex::new(None));
    *session.lock().await = Some(BrowserSession {
        browser,
        handler_task,
        capture_task: tokio::spawn(async {}),
    });

    let capture_task = tokio::spawn({
        let (page, flows, flows_capped, page_urls, session) = (
            Arc::clone(&page),
            Arc::clone(&flows),
            Arc::clone(&flows_capped),
            Arc::clone(&page_urls),
            Arc::clone(&session),
        );
        async move {
            let _ = capture::capture(page, flows, flows_capped, page_urls, session).await;
        }
    });
    tokio::time::sleep(Duration::from_millis(500)).await;

    // No pause between creation and navigation — this is what a popup does.
    {
        let guard = session.lock().await;
        let browser = &guard.as_ref().expect("session present").browser;
        browser
            .new_page(format!("{base}/popup"))
            .await
            .expect("open popup tab");
    }
    tokio::time::sleep(Duration::from_millis(2500)).await;

    let mut taken = session.lock().await.take().expect("session present");
    taken.browser.close().await.ok();
    let _ = capture_task.await;
    let _ = taken.handler_task.await;

    let flows = flows.read().await.clone();
    assert!(
        find(&flows, "/popup").is_some(),
        "popup document request must be captured"
    );
    assert!(
        find(&flows, "/api/from-popup").is_some(),
        "popup's opening API call must be captured"
    );
}
