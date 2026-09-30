//! Live end-to-end check of the session runtime against a local server:
//! exec (result, args, console, errors, same-page state like a CSRF token),
//! pacing, the audit log, status via a check script, recording, and the kill
//! switch.
//!
//! Ignored by default: it launches a real browser (a window appears briefly).
//!
//!   cargo test --test smoke_runtime -- --ignored --nocapture
//!
//! Alone in its binary because it sets `SPOOR_CACHE_DIR` (process-global).

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::Html;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{Value, json};
use spoor::runtime::{ExecRequest, Runtime};
use spoor::session::SessionStore;
use spoor::site::{Site, Sites};

const INDEX_HTML: &str = r#"<!doctype html><html><body>
<input type="hidden" name="csrf_token" value="tok-123">
<h1>runtime smoke</h1></body></html>"#;

async fn spawn_server(hits: Arc<AtomicU64>) -> SocketAddr {
    let app = Router::new()
        .route("/", get(|| async { Html(INDEX_HTML) }))
        .route(
            "/api/count",
            get(|State(hits): State<Arc<AtomicU64>>| async move {
                Json(json!({ "n": hits.fetch_add(1, Ordering::SeqCst) + 1 }))
            }),
        )
        .route(
            "/api/echo",
            post(|headers: HeaderMap, body: String| async move {
                let csrf = headers
                    .get("x-csrf-token")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("")
                    .to_string();
                Json(json!({ "csrf": csrf, "body": body }))
            }),
        )
        .route(
            "/api/me",
            get(|| async { Json(json!({ "user": "alice" })) }),
        )
        .with_state(hits);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    addr
}

fn exec(script: &str) -> ExecRequest {
    ExecRequest {
        script: script.into(),
        timeout_secs: Some(30),
        ..Default::default()
    }
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "launches a real browser"]
async fn exec_status_record_and_stop() {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let home = std::env::temp_dir().join(format!("spoor-runtime-smoke-{nanos}"));
    std::fs::create_dir_all(&home).unwrap();
    // Safety: alone in this test binary; nothing else reads env concurrently.
    unsafe {
        std::env::set_var("SPOOR_CACHE_DIR", &home);
    }

    let hits = Arc::new(AtomicU64::new(0));
    let addr = spawn_server(Arc::clone(&hits)).await;
    let base = format!("http://{addr}");

    let check = home.join("check.js");
    std::fs::write(
        &check,
        "const r = await fetch('/api/me'); return (await r.json()).user === 'alice';",
    )
    .unwrap();
    let mut sites = Sites::default();
    sites
        .add(
            "demo",
            Site {
                url: format!("{base}/"),
                check: Some(check),
                min_gap_ms: Some(300),
            },
        )
        .unwrap();
    sites.save().unwrap();

    let chrome = spoor::browser_util::ensure_chromium()
        .await
        .expect("chrome");
    let rt = Runtime::with_store(chrome, SessionStore::at(home.join("sessions")));

    // 1. Result, args, console, and state from the loaded page (a CSRF token).
    let out = rt
        .exec(
            "demo",
            ExecRequest {
                script: r#"
                    console.log('hello', args.n);
                    const csrf = document.querySelector('input[name=csrf_token]').value;
                    const r = await fetch('/api/echo', {
                        method: 'POST',
                        headers: { 'X-CSRF-Token': csrf },
                        body: 'id=' + args.n,
                    });
                    return await r.json();
                "#
                .into(),
                args: json!({ "n": 7 }),
                timeout_secs: Some(30),
                url: None,
            },
        )
        .await
        .expect("exec");
    eprintln!("exec 1: {out:?}");
    assert!(out.ok, "{:?}", out.error);
    assert_eq!(out.result, json!({ "csrf": "tok-123", "body": "id=7" }));
    assert!(
        out.logs.iter().any(|l| l.contains("hello 7")),
        "{:?}",
        out.logs
    );
    // >= because the browser may add its own requests (favicon.ico) — which
    // are paced and audited like the script's.
    assert!(out.requests >= 1);

    // 2. Pacing: 4 requests at a 300ms minimum gap take at least ~900ms.
    let started = Instant::now();
    let out = rt
        .exec(
            "demo",
            exec(
                "const ns = []; for (let i = 0; i < 4; i++) { ns.push((await (await fetch('/api/count')).json()).n); } return ns;",
            ),
        )
        .await
        .unwrap();
    let elapsed = started.elapsed();
    assert!(out.ok, "{:?}", out.error);
    assert_eq!(out.result, json!([1, 2, 3, 4]));
    assert!(out.requests >= 4);
    assert!(
        elapsed >= Duration::from_millis(900),
        "not paced: {elapsed:?}"
    );

    // Parallel fetches are paced too — the guard sits below the script.
    let started = Instant::now();
    let out = rt
        .exec(
            "demo",
            exec("return (await Promise.all([1,2,3].map(() => fetch('/api/count').then(r => r.json())))).length;"),
        )
        .await
        .unwrap();
    assert!(out.ok, "{:?}", out.error);
    assert_eq!(out.result, json!(3));
    assert!(
        started.elapsed() >= Duration::from_millis(600),
        "parallel not paced"
    );

    // 3. Errors come back as ok=false with the message.
    let out = rt
        .exec("demo", exec("throw new Error('nope')"))
        .await
        .unwrap();
    assert!(!out.ok);
    assert!(
        out.error.as_deref().unwrap_or("").contains("nope"),
        "{:?}",
        out.error
    );
    let out = rt.exec("demo", exec("return (")).await.unwrap();
    assert!(!out.ok, "syntax error must fail");

    // 4. Timeout.
    let out = rt
        .exec(
            "demo",
            ExecRequest {
                script: "await new Promise(r => setTimeout(r, 60000));".into(),
                timeout_secs: Some(2),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert!(!out.ok && out.error.as_deref().unwrap_or("").contains("timed out"));

    // 5. Audit log has the paced requests.
    let audit = std::fs::read_to_string(home.join("audit/demo.jsonl")).expect("audit log");
    let requests = audit
        .lines()
        .map(|l| serde_json::from_str::<Value>(l).unwrap())
        .filter(|v| v["event"] == "request")
        .count();
    assert!(requests >= 8, "audit request lines: {requests}\n{audit}");

    // 6. Status runs the check script.
    let status = rt.status("demo").await.unwrap();
    assert!(status.running);
    assert_eq!(status.logged_in, Some(true), "{status:?}");

    // 7. Record while an exec runs; the exec's traffic lands in the session.
    let info = rt.record_start("demo", None).await.unwrap();
    tokio::time::sleep(Duration::from_millis(800)).await;
    let out = rt
        .exec(
            "demo",
            exec("return (await (await fetch('/api/me')).json()).user"),
        )
        .await
        .unwrap();
    assert_eq!(out.result, json!("alice"));
    tokio::time::sleep(Duration::from_millis(500)).await;
    let done = rt.record_stop("demo").await.unwrap();
    assert_eq!(done.session_id, info.session_id);
    assert!(done.flow_count > 0);
    let loaded = SessionStore::at(home.join("sessions"))
        .load(&info.session_id)
        .unwrap();
    assert_eq!(loaded.meta.site.as_deref(), Some("demo"));
    assert!(
        loaded.flows.iter().any(|f| f.url.ends_with("/api/me")),
        "recorded: {:?}",
        loaded.flows.iter().map(|f| &f.url).collect::<Vec<_>>()
    );

    // 8. Kill switch, then a fresh exec relaunches on the same profile.
    assert!(rt.stop("demo").await);
    assert!(
        !rt.status("demo")
            .await
            .map(|s| s.running && s.recording.is_some())
            .unwrap_or(false)
    );
    let out = rt.exec("demo", exec("return 42")).await.unwrap();
    assert_eq!(out.result, json!(42));

    rt.stop_all().await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    let _ = std::fs::remove_dir_all(&home);
}
