//! End-to-end check that a recording lifecycle actually lands on disk.
//!
//! The store and replay paths are covered elsewhere by feeding them fixtures.
//! This covers the link those tests skip: `AppState` arming the persister,
//! `set_recording` driving it, and the snapshot loop writing real flows. That
//! link is the entire point of the feature — a human's browsing must survive a
//! crash — and it is the part with no other coverage.
//!
//! Lives in its own test binary and is the only test in it, because it sets
//! `SPOOR_CACHE_DIR` and env vars are process-global.

use std::path::PathBuf;
use std::time::Duration;

use spoor::capture::{Body, CaptureRecord, Transport};
use spoor::session::{SNAPSHOT_INTERVAL, SessionStore};
use spoor::types::{AppState, BrowsingPage};

fn flow(seq: u64, path: &str) -> CaptureRecord {
    CaptureRecord {
        id: format!("f-{seq}"),
        transport: Transport::Http,
        url: format!("https://api.example.test{path}"),
        method: Some("GET".into()),
        request_headers: Default::default(),
        request_body: None,
        status: Some(200),
        response_headers: None,
        response_body: Some(Body::text(format!(r#"{{"seq":{seq}}}"#))),
        resource_type: Some("XHR".into()),
        sequence: seq,
        timestamp_ms: None,
        ws_request_id: None,
        ws_opcode: None,
        direction: None,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn recording_lifecycle_persists_flows_to_disk() {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let cache = std::env::temp_dir().join(format!("spoor-persist-test-{nanos}"));
    std::fs::create_dir_all(&cache).expect("create temp cache");
    // Safety: this test is alone in its binary, so no other thread reads env.
    unsafe {
        std::env::set_var("SPOOR_CACHE_DIR", &cache);
    }

    let state = AppState::new(PathBuf::from("/nonexistent/chrome"));

    // Recording begins: arms and spawns the snapshot loop.
    state.set_recording(true);
    state
        .flows
        .write()
        .await
        .extend([flow(1, "/v1/me"), flow(2, "/v1/orders")]);
    state.page_urls.write().await.push(BrowsingPage {
        url: "https://app.example.test/dashboard".into(),
        domain: "example.test".into(),
    });

    // Let at least one snapshot land while still recording, proving flows are
    // written during the session rather than only at the end.
    tokio::time::sleep(SNAPSHOT_INTERVAL + Duration::from_millis(750)).await;

    let store = SessionStore::default_store();
    let mid = store.list().expect("list sessions");
    assert_eq!(mid.len(), 1, "one session created on start, got {mid:?}");
    assert!(
        mid[0].meta.flow_count >= 2,
        "flows must be written while recording, not just on stop: {:?}",
        mid[0].meta
    );

    // A late flow must still be captured by the stop-triggered flush.
    state.flows.write().await.push(flow(3, "/v1/invoices"));
    state.set_recording(false);
    tokio::time::sleep(Duration::from_millis(1500)).await;

    let sessions = store.list().expect("list sessions");
    assert_eq!(sessions.len(), 1, "stop must not create a second session");
    let summary = &sessions[0];
    assert_eq!(
        summary.meta.flow_count, 3,
        "stop flush must include the final flow: {:?}",
        summary.meta
    );
    assert!(
        summary.meta.ended_at.is_some(),
        "session must be finalized on stop: {:?}",
        summary.meta
    );
    assert!(summary.size_bytes > 0, "session must occupy real bytes");

    // Reload it: this is what `spoor replay` depends on.
    let loaded = store.load(&summary.meta.id).expect("reload session");
    assert_eq!(loaded.flows.len(), 3);
    let paths: Vec<_> = loaded.flows.iter().map(|f| f.url.clone()).collect();
    assert!(
        paths.iter().any(|u| u.ends_with("/v1/invoices")),
        "reloaded flows must include the last one: {paths:?}"
    );
    // Bodies must survive the JSONL round-trip, or replay classifies nothing.
    assert!(
        loaded
            .flows
            .iter()
            .any(|f| f.text_response().is_some_and(|b| b.contains("\"seq\""))),
        "response bodies must survive persistence"
    );

    let _ = std::fs::remove_dir_all(&cache);
}
