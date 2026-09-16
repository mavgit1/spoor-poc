use std::sync::Arc;

use anyhow::Context;
use axum::{
    Json, Router,
    extract::{Request, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use chromiumoxide::browser::Browser;
use serde::Serialize;

use crate::browser_util::{self, spawn_handler};
use crate::capture;
use crate::classify::Protocol;
use crate::classify::filters::{self, FilterAction};
use crate::dump;
use crate::export;
use crate::ir;
use crate::log;
use crate::pipeline;
use crate::session::{SessionStore, format_bytes, is_safe_session_id, keep_count, max_bytes};
use crate::types::{AppState, BrowserSession, Candidate, FilterPreferenceRequest, GenerateRequest};

/// Session/API failure with an HTTP-equivalent class for the headless router.
#[derive(Debug)]
pub enum SessionError {
    Conflict(String),
    NotFound(String),
    BadRequest(String),
    Failed(String),
}

impl SessionError {
    fn status_code(&self) -> StatusCode {
        match self {
            Self::Conflict(_) => StatusCode::CONFLICT,
            Self::NotFound(_) => StatusCode::NOT_FOUND,
            Self::BadRequest(_) => StatusCode::BAD_REQUEST,
            Self::Failed(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

impl std::fmt::Display for SessionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Conflict(s) | Self::NotFound(s) | Self::BadRequest(s) | Self::Failed(s) => {
                write!(f, "{s}")
            }
        }
    }
}

impl std::error::Error for SessionError {}

impl From<SessionError> for Response {
    fn from(err: SessionError) -> Self {
        (err.status_code(), err.to_string()).into_response()
    }
}

#[derive(Serialize, Clone, Debug)]
pub struct StatusSnapshot {
    pub recording: bool,
    pub analyzing: bool,
    pub flow_count: usize,
    pub spec_ready: bool,
    pub candidate_count: usize,
    pub graphql_ops: usize,
    pub jsonrpc_ops: usize,
    pub rest_endpoints: usize,
    pub websocket_ops: usize,
    pub form_ops: usize,
    pub grpc_ops: usize,
    pub traffic_graphql: usize,
    pub traffic_jsonrpc: usize,
    pub traffic_rest: usize,
    pub traffic_websocket: usize,
    pub flows_classified: usize,
    pub flows_filtered: usize,
    pub flows_capped: bool,
    pub undecoded_binary: usize,
    pub websocket_frames: usize,
    pub grpc_or_protobuf: usize,
    pub filters_config: String,
}

#[derive(Serialize, Clone, Debug)]
pub struct CandidatesSnapshot {
    pub origins: Vec<String>,
    pub candidates: Vec<Candidate>,
}

#[derive(Serialize, Clone, Debug)]
pub struct GenerateOutcome {
    pub message: String,
    pub warnings: Vec<String>,
}

#[derive(Serialize, Clone, Debug)]
pub struct FilterOutcome {
    pub message: String,
    pub config_path: String,
    pub action: String,
}

#[derive(Serialize, Clone, Debug)]
pub struct StoredSessionItem {
    pub id: String,
    pub started_at: String,
    pub ended_at: Option<String>,
    pub flow_count: usize,
    pub size_bytes: u64,
    pub size_label: String,
    pub gzipped: bool,
    pub flows_capped: bool,
}

#[derive(Serialize, Clone, Debug)]
pub struct SessionsSnapshot {
    pub sessions: Vec<StoredSessionItem>,
    pub store_path: String,
    pub keep: usize,
    pub max_bytes: u64,
    pub max_label: String,
    pub total_bytes: u64,
    pub total_label: String,
}

/// Headless HTTP API. Every route requires `Authorization: Bearer <token>`.
///
/// The desktop app must **not** call this — it talks to the library over IPC.
/// CORS is intentionally omitted: a browser page must not read captured bodies.
pub fn router(state: AppState, bearer_token: impl Into<String>) -> Router {
    let token: Arc<str> = Arc::from(bearer_token.into());
    Router::new()
        .route("/api/start", post(start_handler))
        .route("/api/stop", post(stop_handler))
        .route("/api/status", get(status_handler))
        .route("/api/candidates", get(candidates_handler))
        .route("/api/generate", post(generate_handler))
        .route("/api/ignore", post(filter_preference_handler))
        .route("/api/filter", post(filter_preference_handler))
        .route("/api/download", get(download_handler))
        .route("/api/dump", get(dump_handler))
        .layer(middleware::from_fn_with_state(token, bearer_auth))
        .with_state(state)
}

async fn bearer_auth(
    State(token): State<Arc<str>>,
    req: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    let authorized = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .is_some_and(|got| constant_time_eq(got.as_bytes(), token.as_bytes()));
    if authorized {
        Ok(next.run(req).await)
    } else {
        Err(StatusCode::UNAUTHORIZED)
    }
}

/// Compare without short-circuiting on the first differing byte, so response
/// timing does not reveal how much of the token a caller guessed correctly.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

pub async fn status_snapshot(state: &AppState) -> StatusSnapshot {
    let flow_count = state.flows.read().await.len();
    let candidates = state.candidates.read().await;
    let candidate_count = candidates.len();
    let graphql_ops = candidates
        .iter()
        .filter(|c| c.protocol == "graphql")
        .count();
    let jsonrpc_ops = candidates
        .iter()
        .filter(|c| c.protocol == "jsonrpc")
        .count();
    let rest_endpoints = candidates.iter().filter(|c| c.protocol == "rest").count();
    let websocket_ops = candidates
        .iter()
        .filter(|c| c.protocol == "websocket")
        .count();
    let form_ops = candidates.iter().filter(|c| c.protocol == "form").count();
    let grpc_ops = candidates
        .iter()
        .filter(|c| c.protocol == "grpcweb" || c.protocol == "protobuf")
        .count();
    let classified = state.classified.read().await;
    let traffic_graphql = classified
        .iter()
        .filter(|c| c.protocol == Protocol::Graphql)
        .count();
    let traffic_jsonrpc = classified
        .iter()
        .filter(|c| c.protocol == Protocol::JsonRpc)
        .count();
    let traffic_rest = classified
        .iter()
        .filter(|c| c.protocol == Protocol::Rest)
        .count();
    let traffic_websocket = classified
        .iter()
        .filter(|c| c.protocol == Protocol::WebSocket)
        .count();
    let flows_classified = classified.len();
    let flows_filtered = flow_count.saturating_sub(flows_classified);
    let spec_ready = state.export_bundle.read().await.is_some();
    let coverage = state.coverage.read().await.clone();
    StatusSnapshot {
        recording: state.is_recording(),
        analyzing: state.is_analyzing(),
        flow_count,
        spec_ready,
        candidate_count,
        graphql_ops,
        jsonrpc_ops,
        rest_endpoints,
        websocket_ops,
        form_ops,
        grpc_ops,
        traffic_graphql,
        traffic_jsonrpc,
        traffic_rest,
        traffic_websocket,
        flows_classified,
        flows_filtered,
        flows_capped: state.flows_capped.load(std::sync::atomic::Ordering::SeqCst),
        undecoded_binary: coverage.undecoded_binary,
        websocket_frames: coverage.websocket_frames,
        grpc_or_protobuf: coverage.grpc_or_protobuf,
        filters_config: crate::classify::filters::filters_config_path()
            .to_string_lossy()
            .into_owned(),
    }
}

pub async fn candidates_snapshot(state: &AppState) -> CandidatesSnapshot {
    let classified = state.classified.read().await;
    let entries: Vec<_> = classified.iter().map(|c| c.entry.clone()).collect();
    let origins = ir::unique_origins(&entries);
    let registry = filters::FilterRegistry::load();
    let candidates: Vec<Candidate> = state
        .candidates
        .read()
        .await
        .iter()
        .map(|c| {
            let mut c = c.clone();
            c.preference_ignored = filters::preference_ignored(&c, &registry);
            c.default_selected = filters::default_selected(&c, &registry);
            c
        })
        .collect();
    CandidatesSnapshot {
        origins,
        candidates,
    }
}

pub async fn start_recording(state: &AppState) -> Result<(), SessionError> {
    log::info("start recording");
    if state.is_recording() {
        log::warn("start rejected: already recording");
        return Err(SessionError::Conflict("Already recording".into()));
    }

    state.set_recording(true);
    *state.export_bundle.write().await = None;
    *state.candidates.write().await = Vec::new();
    *state.classified.write().await = Vec::new();
    state.flows.write().await.clear();
    state.page_urls.write().await.clear();
    state
        .flows_capped
        .store(false, std::sync::atomic::Ordering::SeqCst);
    log::debug("cleared previous session state");

    browser_util::cleanup_stale_profile_lock(&browser_util::recording_profile_dir()).await;

    let config = match browser_util::recording_config(state.chromium_executable.as_path()) {
        Ok(c) => c,
        Err(e) => {
            state.set_recording(false);
            log::error(format!("recording browser config error: {e:#}"));
            return Err(SessionError::Failed(e.to_string()));
        }
    };

    log::info("launching recording browser (1280×800)");
    let (mut browser, handler) = match Browser::launch(config).await {
        Ok(b) => b,
        Err(e) => {
            state.set_recording(false);
            log::error(format!("recording browser launch failed: {e:#}"));
            return Err(SessionError::Failed(format!(
                "Failed to launch browser: {e}"
            )));
        }
    };
    log::info("recording browser launched");

    let handler_task = spawn_handler(handler, "recording");

    let page = match recording_page(&browser).await {
        Ok(p) => p,
        Err(e) => {
            state.set_recording(false);
            log::error(format!("failed to get recording page: {e:#}"));
            let _ = browser.close().await;
            handler_task.abort();
            return Err(SessionError::Failed(format!("Failed to open page: {e}")));
        }
    };

    let page = Arc::new(page);
    let flows = Arc::clone(&state.flows);
    let capture_page = Arc::clone(&page);
    let flows_capped = Arc::clone(&state.flows_capped);
    let page_urls = Arc::clone(&state.page_urls);
    let session = Arc::clone(&state.session);
    let capture_task = tokio::spawn(async move {
        log::debug("capture task started");
        match capture::capture(capture_page, flows, flows_capped, page_urls, session).await {
            Ok(()) => log::info("capture task ended (event listeners closed)"),
            Err(e) => log::error(format!("capture task failed: {e:#}")),
        }
    });

    *state.session.lock().await = Some(BrowserSession {
        browser,
        handler_task,
        capture_task,
    });

    log::info("recording started — close the recording browser with Stop, not the window ✕");
    Ok(())
}

async fn recording_page(browser: &Browser) -> anyhow::Result<chromiumoxide::Page> {
    let pages = browser.pages().await.context("list browser tabs")?;
    let tab_count = pages.len();
    if let Some(page) = pages.into_iter().next() {
        log::info(format!(
            "capture attached to main tab ({tab_count} tab(s) open)"
        ));
        return Ok(page);
    }
    log::info("no tabs yet — opening one");
    browser
        .new_page("about:blank")
        .await
        .context("open recording tab")
}

/// Close the recording browser. Does **not** run discover — caller must.
pub async fn stop_recording(state: &AppState) -> Result<(), SessionError> {
    log::info("stop recording");
    if !state.is_recording() {
        log::warn("stop rejected: not recording");
        return Err(SessionError::Conflict("Not recording".into()));
    }

    state.set_recording(false);
    let flow_count = state.flows.read().await.len();
    log::info(format!("stopping recording ({flow_count} flows captured)"));

    let session = state.session.lock().await.take();
    let Some(mut session) = session else {
        log::error("stop failed: no active browser session");
        return Err(SessionError::Failed("No active session".into()));
    };

    log::info("closing recording browser");
    if let Err(e) = session.browser.close().await {
        log::error(format!("failed to close recording browser: {e:#}"));
        return Err(SessionError::Failed(format!(
            "Failed to close browser: {e}"
        )));
    }

    let _ = session.capture_task.await;
    let _ = session.handler_task.await;
    log::info("recording browser shut down");
    Ok(())
}

/// Classify + discover. Sets `analyzing` for the duration.
pub async fn run_discover_session(state: &AppState) -> Result<(), SessionError> {
    state.set_analyzing(true);
    log::info("discover pipeline started");
    let result = pipeline::run_discover(state).await;
    state.set_analyzing(false);
    match result {
        Ok(()) => {
            let n = state.candidates.read().await.len();
            log::info(format!("discover finished — {n} candidates ready"));
            Ok(())
        }
        Err(e) => {
            log::error(format!("discover failed: {e:#}"));
            Err(SessionError::Failed(e.to_string()))
        }
    }
}

pub async fn generate_export(
    state: &AppState,
    req: GenerateRequest,
) -> Result<GenerateOutcome, SessionError> {
    if req.selected.is_empty() {
        return Err(SessionError::BadRequest("No candidates selected".into()));
    }

    let classified = state.classified.read().await.clone();
    let candidates = state.candidates.read().await.clone();
    let flows = state.flows.read().await.clone();
    let coverage = state.coverage.read().await.clone();
    let page_urls = state.page_urls.read().await.clone();

    match export::generate_bundle_with_coverage(
        &classified,
        &candidates,
        &req,
        &flows,
        &coverage,
        &page_urls,
    ) {
        Ok(result) => {
            *state.export_bundle.write().await = Some(result.bundle);
            Ok(GenerateOutcome {
                message: "Export generated".to_string(),
                warnings: result.warnings,
            })
        }
        Err(e) => {
            log::error(format!("generate failed: {e:#}"));
            Err(SessionError::Failed(e.to_string()))
        }
    }
}

pub fn persist_filter_preference(
    req: FilterPreferenceRequest,
) -> Result<FilterOutcome, SessionError> {
    if req.pattern.trim().is_empty() {
        return Err(SessionError::BadRequest("Empty pattern".into()));
    }
    let action = match req.action.trim().to_ascii_lowercase().as_str() {
        "ignore" => FilterAction::Ignore,
        "allow" => FilterAction::Allow,
        _ => {
            return Err(SessionError::BadRequest(
                "action must be ignore or allow".into(),
            ));
        }
    };
    match filters::persist_preference(req.pattern.trim(), action) {
        Ok(path) => {
            let message = match action {
                FilterAction::Ignore => "Ignore pattern saved".to_string(),
                FilterAction::Allow => "Removed from ignore list".to_string(),
            };
            Ok(FilterOutcome {
                message,
                config_path: path.to_string_lossy().into_owned(),
                action: req.action,
            })
        }
        Err(e) => {
            log::error(format!("filter preference persist failed: {e:#}"));
            Err(SessionError::Failed(e.to_string()))
        }
    }
}

pub fn sessions_snapshot() -> Result<SessionsSnapshot, SessionError> {
    sessions_snapshot_from(&SessionStore::default_store())
}

fn sessions_snapshot_from(store: &SessionStore) -> Result<SessionsSnapshot, SessionError> {
    let list = store
        .list()
        .map_err(|e| SessionError::Failed(e.to_string()))?;
    let total_bytes: u64 = list.iter().map(|s| s.size_bytes).sum();
    let cap = max_bytes();
    Ok(SessionsSnapshot {
        sessions: list
            .into_iter()
            .map(|s| StoredSessionItem {
                id: s.meta.id,
                started_at: s.meta.started_at,
                ended_at: s.meta.ended_at,
                flow_count: s.meta.flow_count,
                size_bytes: s.size_bytes,
                size_label: format_bytes(s.size_bytes),
                gzipped: s.meta.gzipped,
                flows_capped: s.meta.flows_capped,
            })
            .collect(),
        store_path: store.root().display().to_string(),
        keep: keep_count(),
        max_bytes: cap,
        max_label: format_bytes(cap),
        total_bytes,
        total_label: format_bytes(total_bytes),
    })
}

fn refuse_if_recording(state: &AppState, action: &str) -> Result<(), SessionError> {
    if state.is_recording() {
        return Err(SessionError::Conflict(format!(
            "Cannot {action} while recording is in progress. Stop first so the current capture is fully written."
        )));
    }
    Ok(())
}

/// Load a stored session into `state`. Does **not** run discover — caller must,
/// same as after Stop.
pub async fn load_stored_session(state: &AppState, id: &str) -> Result<(), SessionError> {
    load_stored_session_from(state, SessionStore::default_store(), id).await
}

async fn load_stored_session_from(
    state: &AppState,
    store: SessionStore,
    id: &str,
) -> Result<(), SessionError> {
    refuse_if_recording(state, "load a stored session")?;
    if state.is_analyzing() {
        return Err(SessionError::Conflict(
            "Cannot load a stored session while discovery is running.".into(),
        ));
    }
    if !is_safe_session_id(id) {
        return Err(SessionError::BadRequest("Invalid session id".into()));
    }
    let id_owned = id.to_string();
    let loaded = tokio::task::spawn_blocking(move || store.load(&id_owned))
        .await
        .map_err(|e| SessionError::Failed(e.to_string()))?
        .map_err(|e| SessionError::Failed(e.to_string()))?;

    let n = loaded.flows.len();
    let sid = loaded.meta.id.clone();
    *state.flows.write().await = loaded.flows;
    *state.page_urls.write().await = loaded.meta.pages;
    state.flows_capped.store(
        loaded.meta.flows_capped,
        std::sync::atomic::Ordering::SeqCst,
    );
    *state.export_bundle.write().await = None;
    *state.candidates.write().await = Vec::new();
    *state.classified.write().await = Vec::new();
    *state.coverage.write().await = crate::classify::CoverageReport::default();
    log::info(format!("loaded session {sid} — {n} flow(s)"));
    Ok(())
}

pub fn delete_stored_session(state: &AppState, id: &str) -> Result<(), SessionError> {
    refuse_if_recording(state, "delete a stored session")?;
    delete_stored_session_from(&SessionStore::default_store(), id)
}

fn delete_stored_session_from(store: &SessionStore, id: &str) -> Result<(), SessionError> {
    if !is_safe_session_id(id) {
        return Err(SessionError::BadRequest("Invalid session id".into()));
    }
    let dir = store.session_dir(id);
    if !dir.join("meta.json").is_file() {
        return Err(SessionError::NotFound(format!("No stored session {id}")));
    }
    std::fs::remove_dir_all(&dir)
        .map_err(|e| SessionError::Failed(format!("Failed to delete session {id}: {e}")))?;
    log::info(format!("deleted session {id}"));
    Ok(())
}

pub fn delete_all_stored_sessions(state: &AppState) -> Result<usize, SessionError> {
    refuse_if_recording(state, "delete stored sessions")?;
    delete_all_stored_sessions_from(&SessionStore::default_store())
}

fn delete_all_stored_sessions_from(store: &SessionStore) -> Result<usize, SessionError> {
    let list = store
        .list()
        .map_err(|e| SessionError::Failed(e.to_string()))?;
    let n = list.len();
    for s in &list {
        std::fs::remove_dir_all(&s.path).map_err(|e| {
            SessionError::Failed(format!("Failed to delete session {}: {e}", s.meta.id))
        })?;
    }
    if n > 0 {
        log::info(format!("deleted {n} stored session(s)"));
    }
    Ok(n)
}

pub async fn capture_dump_gzip(state: &AppState) -> Result<Vec<u8>, SessionError> {
    if state.is_recording() {
        return Err(SessionError::Conflict(
            "Stop recording before downloading capture".into(),
        ));
    }

    let flows = state.flows.read().await;
    if flows.is_empty() {
        return Err(SessionError::NotFound("No captured traffic yet".into()));
    }

    let classified = state.classified.read().await;
    let flows_capped = state.flows_capped.load(std::sync::atomic::Ordering::SeqCst);

    match dump::build_capture_dump_gzip(&flows, &classified, flows_capped) {
        Ok(bytes) => {
            log::info(format!(
                "capture dump: {} flows ({} classified), {} bytes gzip",
                flows.len(),
                classified.len(),
                bytes.len()
            ));
            Ok(bytes)
        }
        Err(e) => {
            log::error(format!("capture dump failed: {e:#}"));
            Err(SessionError::Failed(e.to_string()))
        }
    }
}

pub async fn export_zip_bytes(state: &AppState) -> Result<Vec<u8>, SessionError> {
    let bundle = state.export_bundle.read().await;
    match bundle.as_ref() {
        Some(b) if !b.zip_bytes.is_empty() => Ok(b.zip_bytes.clone()),
        _ => Err(SessionError::NotFound("No export generated yet".into())),
    }
}

async fn status_handler(State(state): State<AppState>) -> Json<StatusSnapshot> {
    Json(status_snapshot(&state).await)
}

async fn candidates_handler(State(state): State<AppState>) -> Json<CandidatesSnapshot> {
    Json(candidates_snapshot(&state).await)
}

async fn generate_handler(
    State(state): State<AppState>,
    Json(req): Json<GenerateRequest>,
) -> Response {
    match generate_export(&state, req).await {
        Ok(outcome) => Json(outcome).into_response(),
        Err(e) => e.into(),
    }
}

async fn filter_preference_handler(Json(req): Json<FilterPreferenceRequest>) -> Response {
    match persist_filter_preference(req) {
        Ok(outcome) => Json(outcome).into_response(),
        Err(e) => e.into(),
    }
}

async fn dump_handler(State(state): State<AppState>) -> Response {
    match capture_dump_gzip(&state).await {
        Ok(bytes) => {
            let mut headers = HeaderMap::new();
            headers.insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/gzip"),
            );
            headers.insert(
                header::CONTENT_DISPOSITION,
                HeaderValue::from_static("attachment; filename=\"spoor-capture.json.gz\""),
            );
            (StatusCode::OK, headers, bytes).into_response()
        }
        Err(e) => e.into(),
    }
}

async fn download_handler(State(state): State<AppState>) -> Response {
    match export_zip_bytes(&state).await {
        Ok(bytes) => {
            let mut headers = HeaderMap::new();
            headers.insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/zip"),
            );
            headers.insert(
                header::CONTENT_DISPOSITION,
                HeaderValue::from_static("attachment; filename=\"spoor-export.zip\""),
            );
            (StatusCode::OK, headers, bytes).into_response()
        }
        Err(e) => e.into(),
    }
}

async fn start_handler(State(state): State<AppState>) -> Response {
    log::info("POST /api/start");
    match start_recording(&state).await {
        Ok(()) => (StatusCode::OK, "Recording started").into_response(),
        Err(e) => e.into(),
    }
}

async fn stop_handler(State(state): State<AppState>) -> Response {
    log::info("POST /api/stop");
    if let Err(e) = stop_recording(&state).await {
        return e.into();
    }

    let state_clone = state.clone();
    tokio::spawn(async move {
        let _ = run_discover_session(&state_clone).await;
    });

    (StatusCode::OK, "Recording stopped, discovering APIs…").into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[tokio::test]
    async fn empty_status_is_idle() {
        let state = AppState::new(PathBuf::from("/nonexistent"));
        let s = status_snapshot(&state).await;
        assert!(!s.recording);
        assert!(!s.analyzing);
        assert_eq!(s.flow_count, 0);
        assert!(!s.spec_ready);
        assert_eq!(s.candidate_count, 0);
    }

    #[tokio::test]
    async fn generate_rejects_empty_selection() {
        let state = AppState::new(PathBuf::from("/nonexistent"));
        let err = generate_export(
            &state,
            GenerateRequest {
                origin: None,
                selected: Vec::new(),
                ignore_patterns: Vec::new(),
                redact: false,
            },
        )
        .await
        .unwrap_err();
        assert!(matches!(err, SessionError::BadRequest(_)));
    }

    #[test]
    fn filter_rejects_empty_pattern() {
        let err = persist_filter_preference(FilterPreferenceRequest {
            pattern: "  ".into(),
            action: "ignore".into(),
        })
        .unwrap_err();
        assert!(matches!(err, SessionError::BadRequest(_)));
    }

    fn temp_sessions_dir() -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "spoor-ui-sessions-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn seed_auth_bearer(store_root: &std::path::Path) -> String {
        let src =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sessions/auth_bearer");
        let id = "fixture-auth-bearer";
        let dst = store_root.join(id);
        std::fs::create_dir_all(&dst).unwrap();
        std::fs::copy(src.join("meta.json"), dst.join("meta.json")).unwrap();
        std::fs::copy(src.join("flows.jsonl"), dst.join("flows.jsonl")).unwrap();
        id.to_string()
    }

    #[test]
    fn sessions_list_is_newest_first_and_reports_retention() {
        let dir = temp_sessions_dir();
        seed_auth_bearer(&dir);
        let store = SessionStore::at(&dir);
        let snap = sessions_snapshot_from(&store).unwrap();
        assert_eq!(snap.sessions.len(), 1);
        assert_eq!(snap.sessions[0].id, "fixture-auth-bearer");
        assert_eq!(snap.sessions[0].flow_count, 3);
        assert!(!snap.sessions[0].size_label.is_empty());
        assert_eq!(snap.store_path, dir.display().to_string());
        assert!(snap.keep > 0);
        assert!(snap.max_bytes > 0);
        assert!(snap.max_label.contains('B'));
    }

    #[tokio::test]
    async fn load_session_refused_while_recording() {
        let state = AppState::new(PathBuf::from("/nonexistent"));
        state
            .recording
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let store = SessionStore::at(temp_sessions_dir());
        let err = load_stored_session_from(&state, store, "fixture-auth-bearer")
            .await
            .unwrap_err();
        assert!(matches!(err, SessionError::Conflict(_)));
        assert!(err.to_string().contains("recording"));
    }

    #[test]
    fn delete_session_refused_while_recording() {
        let state = AppState::new(PathBuf::from("/nonexistent"));
        state
            .recording
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let err = delete_stored_session(&state, "fixture-auth-bearer").unwrap_err();
        assert!(matches!(err, SessionError::Conflict(_)));
        assert!(err.to_string().contains("recording"));
    }

    #[tokio::test]
    async fn load_stored_session_populates_candidates_unchecked() {
        crate::pipeline::disable_llm();
        let dir = temp_sessions_dir();
        let id = seed_auth_bearer(&dir);
        let store = SessionStore::at(&dir);
        let state = AppState::new(PathBuf::from("/nonexistent"));
        load_stored_session_from(&state, store, &id)
            .await
            .expect("load");
        assert_eq!(state.flows.read().await.len(), 3);
        assert!(state.export_bundle.read().await.is_none());
        run_discover_session(&state).await.expect("discover");
        let snap = candidates_snapshot(&state).await;
        assert_eq!(snap.candidates.len(), 3);
        assert!(
            snap.candidates.iter().all(|c| !c.default_selected),
            "patterns are pre-filled, never pre-selected"
        );
        let selected = snap
            .candidates
            .iter()
            .map(|c| crate::types::GenerateSelection {
                id: c.id.clone(),
                pattern: Some(c.guessed_pattern.clone()),
            })
            .collect();
        generate_export(
            &state,
            GenerateRequest {
                origin: None,
                selected,
                ignore_patterns: Vec::new(),
                redact: false,
            },
        )
        .await
        .expect("generate");
        assert!(!export_zip_bytes(&state).await.expect("zip").is_empty());
    }

    #[test]
    fn delete_one_and_delete_all() {
        let dir = temp_sessions_dir();
        let id = seed_auth_bearer(&dir);
        let store = SessionStore::at(&dir);
        assert_eq!(sessions_snapshot_from(&store).unwrap().sessions.len(), 1);
        delete_stored_session_from(&store, &id).unwrap();
        assert!(sessions_snapshot_from(&store).unwrap().sessions.is_empty());

        seed_auth_bearer(&dir);
        assert_eq!(delete_all_stored_sessions_from(&store).unwrap(), 1);
        assert!(sessions_snapshot_from(&store).unwrap().sessions.is_empty());
        assert_eq!(delete_all_stored_sessions_from(&store).unwrap(), 0);
    }

    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "launches Chromium; run: cargo test -p spoor -- --ignored session_start_stop"]
    async fn session_start_stop_without_http() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        let site = axum::Router::new()
            .route(
                "/",
                axum::routing::get(|| async {
                    axum::response::Html(
                        r#"<!doctype html><script>fetch('/api/thing').then(r=>r.json())</script>"#,
                    )
                }),
            )
            .route(
                "/api/thing",
                axum::routing::get(|| async { axum::Json(serde_json::json!({ "ok": true })) }),
            );
        tokio::spawn(async move {
            let _ = axum::serve(listener, site).await;
        });

        let chromium = crate::browser_util::ensure_chromium()
            .await
            .expect("chromium");
        let state = AppState::new(chromium);
        start_recording(&state).await.expect("start");
        assert!(state.is_recording());
        tokio::time::sleep(std::time::Duration::from_millis(800)).await;

        let url = format!("http://{addr}/");
        {
            let guard = state.session.lock().await;
            let browser = &guard.as_ref().expect("session").browser;
            let pages = browser.pages().await.expect("pages");
            pages
                .into_iter()
                .next()
                .expect("tab")
                .goto(url)
                .await
                .expect("navigate");
        }
        tokio::time::sleep(std::time::Duration::from_millis(1500)).await;

        let mut flows = 0usize;
        for _ in 0..20 {
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
            flows = state.flows.read().await.len();
            if flows > 0 {
                break;
            }
        }
        let urls: Vec<String> = state
            .flows
            .read()
            .await
            .iter()
            .map(|f| f.url.clone())
            .collect();
        assert!(
            flows > 0,
            "expected live flow count to rise during recording, got {urls:?}"
        );

        stop_recording(&state).await.expect("stop");
        assert!(!state.is_recording());
        run_discover_session(&state).await.expect("discover");
        let snap = candidates_snapshot(&state).await;
        assert!(
            !snap.candidates.is_empty(),
            "expected discovered candidates after JSON traffic"
        );
        let selected = snap
            .candidates
            .iter()
            .map(|c| crate::types::GenerateSelection {
                id: c.id.clone(),
                pattern: Some(c.guessed_pattern.clone()),
            })
            .collect();
        generate_export(
            &state,
            GenerateRequest {
                origin: None,
                selected,
                ignore_patterns: Vec::new(),
                redact: false,
            },
        )
        .await
        .expect("generate");
        let zip = export_zip_bytes(&state).await.expect("zip");
        assert!(!zip.is_empty());
        let path = std::env::temp_dir().join("spoor-session-smoke.zip");
        std::fs::write(&path, &zip).expect("write zip");
        assert!(path.metadata().expect("meta").len() > 0);
    }
}
