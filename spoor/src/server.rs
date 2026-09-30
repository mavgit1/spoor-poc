//! `spoor serve`: the runtime behind a localhost HTTP API.
//!
//! Binds 127.0.0.1 only. Every request needs `Authorization: Bearer <token>`.
//! The url and token are written to `serve.json` (see [`ServeInfo`]) so the
//! CLI and integrations can find them; the file is removed on shutdown.
//!
//! ```text
//! GET  /health
//! GET  /sites
//! GET  /sites/{site}/status
//! POST /sites/{site}/open          {url?, wait?, timeout_secs?}
//! POST /sites/{site}/exec          {script, args?, url?, timeout_secs?}
//! POST /sites/{site}/record/start  {url?}
//! POST /sites/{site}/record/stop
//! POST /sites/{site}/stop          kill switch for one site
//! POST /stop                       kill switch for all sites
//! POST /shutdown                   stop all sites and exit `spoor serve`
//! ```

use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use axum::extract::{Path as UrlPath, Request, State};
use axum::http::{StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::Notify;

use crate::cache_dir::serve_file_path;
use crate::log;
use crate::runtime::{ExecRequest, Runtime};

pub const DEFAULT_PORT: u16 = 7517;
const DEFAULT_WAIT_SECS: u64 = 600;

/// Contents of `serve.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServeInfo {
    pub url: String,
    pub token: String,
    pub pid: u32,
}

impl ServeInfo {
    pub fn read(path: &Path) -> Result<Self> {
        let text =
            std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
        serde_json::from_str(&text).with_context(|| format!("parse {}", path.display()))
    }

    fn write(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, serde_json::to_vec_pretty(self)?)
            .with_context(|| format!("write {}", path.display()))
    }
}

pub fn generate_token() -> Result<String> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|e| anyhow::anyhow!("os rng: {e}"))?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

#[derive(Debug)]
struct AppError(StatusCode, String);

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({ "error": self.1 }))).into_response()
    }
}

impl From<anyhow::Error> for AppError {
    fn from(e: anyhow::Error) -> Self {
        let msg = format!("{e:#}");
        let code = if msg.starts_with("no site ") || msg.starts_with("invalid site name") {
            StatusCode::NOT_FOUND
        } else {
            StatusCode::BAD_REQUEST
        };
        Self(code, msg)
    }
}

type Api<T> = Result<Json<T>, AppError>;

#[derive(Debug, Default, Deserialize)]
struct OpenBody {
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    wait: bool,
    #[serde(default)]
    timeout_secs: Option<u64>,
}

#[derive(Debug, Default, Deserialize)]
struct RecordBody {
    #[serde(default)]
    url: Option<String>,
}

pub fn router(runtime: Arc<Runtime>, token: impl Into<String>, shutdown: Arc<Notify>) -> Router {
    let token: Arc<str> = Arc::from(token.into());
    Router::new()
        .route(
            "/shutdown",
            post(move || async move {
                shutdown.notify_one();
                Json(json!({ "shutting_down": true }))
            }),
        )
        .route("/health", get(|| async { Json(json!({ "ok": true })) }))
        .route("/sites", get(list_sites))
        .route("/sites/{site}/status", get(site_status))
        .route("/sites/{site}/open", post(open_site))
        .route("/sites/{site}/exec", post(exec_site))
        .route("/sites/{site}/record/start", post(record_start))
        .route("/sites/{site}/record/stop", post(record_stop))
        .route("/sites/{site}/stop", post(stop_site))
        .route("/stop", post(stop_all))
        .layer(middleware::from_fn_with_state(token, bearer_auth))
        .with_state(runtime)
}

async fn bearer_auth(State(token): State<Arc<str>>, req: Request, next: Next) -> Response {
    let authorized = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .is_some_and(|got| constant_time_eq(got.as_bytes(), token.as_bytes()));
    if authorized {
        next.run(req).await
    } else {
        AppError(
            StatusCode::UNAUTHORIZED,
            "missing or wrong bearer token".into(),
        )
        .into_response()
    }
}

/// Compare without short-circuiting, so timing does not leak how much of the
/// token a caller guessed correctly.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Accept an empty body as `{}` so `curl -X POST` works without `-d`.
fn body_or_default<T: Default + for<'de> Deserialize<'de>>(bytes: &[u8]) -> Result<T, AppError> {
    if bytes.iter().all(u8::is_ascii_whitespace) {
        return Ok(T::default());
    }
    serde_json::from_slice(bytes)
        .map_err(|e| AppError(StatusCode::BAD_REQUEST, format!("invalid JSON body: {e}")))
}

async fn list_sites(State(rt): State<Arc<Runtime>>) -> Api<serde_json::Value> {
    Ok(Json(json!(rt.list().await?)))
}

async fn site_status(
    State(rt): State<Arc<Runtime>>,
    UrlPath(site): UrlPath<String>,
) -> Api<serde_json::Value> {
    Ok(Json(json!(rt.status(&site).await?)))
}

async fn open_site(
    State(rt): State<Arc<Runtime>>,
    UrlPath(site): UrlPath<String>,
    body: axum::body::Bytes,
) -> Api<serde_json::Value> {
    let body: OpenBody = body_or_default(&body)?;
    if body.wait {
        let timeout = Duration::from_secs(body.timeout_secs.unwrap_or(DEFAULT_WAIT_SECS));
        let logged_in = rt
            .open_and_wait(&site, body.url.as_deref(), timeout)
            .await?;
        return Ok(Json(
            json!({ "site": site, "opened": true, "logged_in": logged_in }),
        ));
    }
    rt.open(&site, body.url.as_deref()).await?;
    Ok(Json(json!({ "site": site, "opened": true })))
}

async fn exec_site(
    State(rt): State<Arc<Runtime>>,
    UrlPath(site): UrlPath<String>,
    Json(req): Json<ExecRequest>,
) -> Api<serde_json::Value> {
    Ok(Json(json!(rt.exec(&site, req).await?)))
}

async fn record_start(
    State(rt): State<Arc<Runtime>>,
    UrlPath(site): UrlPath<String>,
    body: axum::body::Bytes,
) -> Api<serde_json::Value> {
    let body: RecordBody = body_or_default(&body)?;
    Ok(Json(json!(
        rt.record_start(&site, body.url.as_deref()).await?
    )))
}

async fn record_stop(
    State(rt): State<Arc<Runtime>>,
    UrlPath(site): UrlPath<String>,
) -> Api<serde_json::Value> {
    Ok(Json(json!(rt.record_stop(&site).await?)))
}

async fn stop_site(
    State(rt): State<Arc<Runtime>>,
    UrlPath(site): UrlPath<String>,
) -> Api<serde_json::Value> {
    let was_running = rt.stop(&site).await;
    Ok(Json(json!({ "site": site, "was_running": was_running })))
}

async fn stop_all(State(rt): State<Arc<Runtime>>) -> Api<serde_json::Value> {
    Ok(Json(json!({ "stopped": rt.stop_all().await })))
}

/// Run until Ctrl+C: bind, publish `serve.json`, serve, then close every site
/// browser and remove `serve.json`.
pub async fn serve(port: u16, token: Option<String>) -> Result<()> {
    let token = match token.filter(|t| !t.trim().is_empty()) {
        Some(t) => t,
        None => generate_token()?,
    };
    let executable = crate::browser_util::ensure_chromium().await?;
    let runtime = Arc::new(Runtime::new(executable));

    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("bind {addr} (is another `spoor serve` running?)"))?;
    let info = ServeInfo {
        url: format!("http://{addr}"),
        token: token.clone(),
        pid: std::process::id(),
    };
    let serve_file = serve_file_path();
    info.write(&serve_file)?;
    log::info(format!(
        "spoor serve at {} (token in {})",
        info.url,
        serve_file.display()
    ));

    let shutdown = Arc::new(Notify::new());
    let app = router(Arc::clone(&runtime), token, Arc::clone(&shutdown));
    let result = axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {}
                _ = shutdown.notified() => {}
            }
            log::info("shutting down…");
        })
        .await
        .context("server error");

    runtime.stop_all().await;
    // Only remove the file if it is still ours (a newer serve may have replaced it).
    if ServeInfo::read(&serve_file).is_ok_and(|i| i.pid == info.pid) {
        let _ = std::fs::remove_file(&serve_file);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request as HttpRequest;
    use tower::ServiceExt;

    fn app() -> Router {
        let rt = Arc::new(Runtime::new("/nonexistent/chrome".into()));
        router(rt, "secret", Arc::new(Notify::new()))
    }

    async fn call(app: Router, path: &str, token: Option<&str>) -> StatusCode {
        let mut req = HttpRequest::builder().uri(path);
        if let Some(t) = token {
            req = req.header(header::AUTHORIZATION, format!("Bearer {t}"));
        }
        app.oneshot(req.body(Body::empty()).unwrap())
            .await
            .unwrap()
            .status()
    }

    #[tokio::test]
    async fn requires_bearer_token() {
        assert_eq!(call(app(), "/health", None).await, StatusCode::UNAUTHORIZED);
        assert_eq!(
            call(app(), "/health", Some("wrong")).await,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(call(app(), "/health", Some("secret")).await, StatusCode::OK);
    }

    #[tokio::test]
    async fn invalid_site_name_is_not_found() {
        assert_eq!(
            call(app(), "/sites/Not..Valid/status", Some("secret")).await,
            StatusCode::NOT_FOUND
        );
    }

    #[test]
    fn tokens_are_random_hex() {
        let a = generate_token().unwrap();
        let b = generate_token().unwrap();
        assert_eq!(a.len(), 64);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b);
    }

    #[test]
    fn empty_body_defaults() {
        let b: OpenBody = body_or_default(b"").unwrap();
        assert!(!b.wait && b.url.is_none());
        let b: OpenBody = body_or_default(br#"{"wait":true}"#).unwrap();
        assert!(b.wait);
        assert!(body_or_default::<OpenBody>(b"{nope").is_err());
    }
}
