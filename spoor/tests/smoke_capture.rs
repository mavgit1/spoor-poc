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
use chromiumoxide::cdp::browser_protocol::target::CreateTargetParams;
use spoor::browser_util;
use spoor::capture::{self, CaptureRecord};
use spoor::session::BrowsingPage;
use tokio::sync::RwLock;

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

/// Per-test throwaway profile.
///
/// Chromium holds a `SingletonLock` per profile, so two tests launching on the
/// shared recording profile collide and the second launch dies. It would also
/// load the user's real cookies and history, which a test must never touch.
fn scratch_profile(tag: &str) -> std::path::PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!("spoor-smoke-{tag}-{nanos}"));
    std::fs::create_dir_all(&dir).expect("create scratch profile");
    dir
}

/// Close the browser; capture ends when its event streams do.
async fn close(
    browser: Arc<Browser>,
    capture_task: tokio::task::JoinHandle<()>,
    handler_task: tokio::task::JoinHandle<()>,
) {
    browser
        .execute(chromiumoxide::cdp::browser_protocol::browser::CloseParams::default())
        .await
        .ok();
    let _ = tokio::time::timeout(Duration::from_secs(5), capture_task).await;
    let _ = tokio::time::timeout(Duration::from_secs(5), handler_task).await;
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
    let profile = scratch_profile("redirects");
    let config =
        browser_util::site_browser_config(&chromium, profile.clone()).expect("recording config");
    let (browser, handler) = Browser::launch(config).await.expect("launch browser");
    let handler_task = browser_util::spawn_handler(handler, "smoke");

    let browser = Arc::new(browser);
    let page = browser
        .new_page("about:blank")
        .await
        .expect("open first tab");

    let flows: Arc<RwLock<Vec<CaptureRecord>>> = Arc::new(RwLock::new(Vec::new()));
    let flows_capped = Arc::new(AtomicBool::new(false));
    let page_urls: Arc<RwLock<Vec<BrowsingPage>>> = Arc::new(RwLock::new(Vec::new()));

    let capture_task = tokio::spawn({
        let (browser, flows, flows_capped, page_urls) = (
            Arc::clone(&browser),
            Arc::clone(&flows),
            Arc::clone(&flows_capped),
            Arc::clone(&page_urls),
        );
        async move {
            let _ = capture::capture(browser, flows, flows_capped, page_urls).await;
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
    //
    // A browser-level command that returns as soon as the target id exists.
    let popup_id = browser
        .execute(CreateTargetParams::new("about:blank"))
        .await
        .expect("open popup tab")
        .result
        .target_id;
    // Give capture time to attach before the target issues any request, so a
    // failure here means "never attached" rather than "attached too late".
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let popup = browser.get_page(popup_id).await.expect("popup page");
    popup
        .goto(format!("{base}/popup"))
        .await
        .expect("navigate popup tab");
    tokio::time::sleep(Duration::from_millis(2000)).await;

    close(browser, capture_task, handler_task).await;
    let _ = std::fs::remove_dir_all(&profile);

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

/// Known gap. A target that navigates the instant it is created loses its
/// opening requests, because capture only attaches after `Target.targetCreated`
/// and must then resolve the page and enable `Network`. This is the OAuth popup
/// shape — `window.open` straight onto the provider's authorize URL — so the
/// document request and its `Set-Cookie` redirect are exactly what we drop.
///
/// The sibling test above passes only because it pauses before navigating.
///
/// An attempt at browser-level `Target.setAutoAttach` with
/// `waitForDebuggerOnStart` was reverted: chromiumoxide's `Browser::execute`
/// always sends `session_id: None`, and its target init issues its own
/// `attachToTarget`, so the session that paused the target is not the `Page`'s
/// session. Driving pause/resume from a second CDP client on the same browser
/// websocket compiles and runs but still misses the opening requests — the
/// `Page` cannot be resolved while the target is paused, so listeners cannot be
/// armed before resume. Kept out of tree because an unresumed target is a
/// frozen browser window for the user. Attempt preserved at
/// `/tmp/spoor-attach-attempt.rs` for the next try.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "known gap: early requests on a self-navigating target are missed"]
async fn captures_first_request_of_a_self_navigating_target() {
    let addr = spawn_server().await;
    let base = format!("http://{addr}");

    let chromium = browser_util::ensure_chromium()
        .await
        .expect("chromium available");
    let profile = scratch_profile("race");
    let config =
        browser_util::site_browser_config(&chromium, profile.clone()).expect("recording config");
    let (browser, handler) = Browser::launch(config).await.expect("launch browser");
    let handler_task = browser_util::spawn_handler(handler, "smoke-race");

    let browser = Arc::new(browser);
    let _page = browser
        .new_page("about:blank")
        .await
        .expect("open first tab");

    let flows: Arc<RwLock<Vec<CaptureRecord>>> = Arc::new(RwLock::new(Vec::new()));
    let flows_capped = Arc::new(AtomicBool::new(false));
    let page_urls: Arc<RwLock<Vec<BrowsingPage>>> = Arc::new(RwLock::new(Vec::new()));

    let capture_task = tokio::spawn({
        let (browser, flows, flows_capped, page_urls) = (
            Arc::clone(&browser),
            Arc::clone(&flows),
            Arc::clone(&flows_capped),
            Arc::clone(&page_urls),
        );
        async move {
            let _ = capture::capture(browser, flows, flows_capped, page_urls).await;
        }
    });
    tokio::time::sleep(Duration::from_millis(500)).await;

    // No pause between creation and navigation — this is what a popup does.
    browser
        .execute(CreateTargetParams::new(format!("{base}/popup")))
        .await
        .expect("open popup tab");
    tokio::time::sleep(Duration::from_millis(2500)).await;

    close(browser, capture_task, handler_task).await;
    let _ = std::fs::remove_dir_all(&profile);

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
