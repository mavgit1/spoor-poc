use std::sync::Arc;

use anyhow::Context;
use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use chromiumoxide::browser::Browser;
use rust_embed::RustEmbed;
use serde::Serialize;
use tower_http::cors::CorsLayer;

use crate::browser_util::{self, spawn_handler};
use crate::capture;
use crate::classify::Protocol;
use crate::classify::filters::{self, FilterAction};
use crate::dump;
use crate::export;
use crate::ir;
use crate::log;
use crate::pipeline;
use crate::types::{AppState, BrowserSession, Candidate, FilterPreferenceRequest, GenerateRequest};

#[derive(RustEmbed)]
#[folder = "src/ui/"]
struct Assets;

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/", get(panel_handler))
        .route("/api/start", post(start_handler))
        .route("/api/stop", post(stop_handler))
        .route("/api/status", get(status_handler))
        .route("/api/candidates", get(candidates_handler))
        .route("/api/generate", post(generate_handler))
        .route("/api/ignore", post(filter_preference_handler))
        .route("/api/filter", post(filter_preference_handler))
        .route("/api/download", get(download_handler))
        .route("/api/dump", get(dump_handler))
        .layer(CorsLayer::permissive())
        .with_state(state)
}

async fn panel_handler() -> impl IntoResponse {
    match Assets::get("panel.html") {
        Some(content) => Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
            .body(axum::body::Body::from(content.data.into_owned()))
            .unwrap(),
        None => (StatusCode::NOT_FOUND, "panel.html not found").into_response(),
    }
}

#[derive(Serialize)]
struct StatusResponse {
    recording: bool,
    analyzing: bool,
    flow_count: usize,
    spec_ready: bool,
    candidate_count: usize,
    graphql_ops: usize,
    jsonrpc_ops: usize,
    rest_endpoints: usize,
    websocket_ops: usize,
    form_ops: usize,
    grpc_ops: usize,
    traffic_graphql: usize,
    traffic_jsonrpc: usize,
    traffic_rest: usize,
    traffic_websocket: usize,
    flows_classified: usize,
    flows_filtered: usize,
    flows_capped: bool,
    undecoded_binary: usize,
    websocket_frames: usize,
    grpc_or_protobuf: usize,
    filters_config: String,
}

async fn status_handler(State(state): State<AppState>) -> Json<StatusResponse> {
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
    Json(StatusResponse {
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
    })
}

#[derive(Serialize)]
struct CandidatesResponse {
    origins: Vec<String>,
    candidates: Vec<Candidate>,
}

async fn candidates_handler(State(state): State<AppState>) -> Json<CandidatesResponse> {
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
    Json(CandidatesResponse {
        origins,
        candidates,
    })
}

#[derive(Serialize)]
struct GenerateResponse {
    message: String,
    warnings: Vec<String>,
}

async fn generate_handler(
    State(state): State<AppState>,
    Json(req): Json<GenerateRequest>,
) -> impl IntoResponse {
    if req.selected.is_empty() {
        return (StatusCode::BAD_REQUEST, "No candidates selected").into_response();
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
            Json(GenerateResponse {
                message: "Export generated".to_string(),
                warnings: result.warnings,
            })
            .into_response()
        }
        Err(e) => {
            log::error(&format!("generate failed: {e:#}"));
            (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response()
        }
    }
}

#[derive(Serialize)]
struct FilterPreferenceResponse {
    message: String,
    config_path: String,
    action: String,
}

async fn filter_preference_handler(Json(req): Json<FilterPreferenceRequest>) -> impl IntoResponse {
    if req.pattern.trim().is_empty() {
        return (StatusCode::BAD_REQUEST, "Empty pattern").into_response();
    }
    let action = match req.action.trim().to_ascii_lowercase().as_str() {
        "ignore" => FilterAction::Ignore,
        "allow" => FilterAction::Allow,
        _ => {
            return (StatusCode::BAD_REQUEST, "action must be ignore or allow").into_response();
        }
    };
    match filters::persist_preference(req.pattern.trim(), action) {
        Ok(path) => {
            let message = match action {
                FilterAction::Ignore => "Ignore pattern saved".to_string(),
                FilterAction::Allow => "Removed from ignore list".to_string(),
            };
            Json(FilterPreferenceResponse {
                message,
                config_path: path.to_string_lossy().into_owned(),
                action: req.action,
            })
            .into_response()
        }
        Err(e) => {
            log::error(&format!("filter preference persist failed: {e:#}"));
            (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response()
        }
    }
}

async fn dump_handler(State(state): State<AppState>) -> impl IntoResponse {
    if state.is_recording() {
        return (
            StatusCode::CONFLICT,
            "Stop recording before downloading capture",
        )
            .into_response();
    }

    let flows = state.flows.read().await;
    if flows.is_empty() {
        return (StatusCode::NOT_FOUND, "No captured traffic yet").into_response();
    }

    let classified = state.classified.read().await;
    let flows_capped = state.flows_capped.load(std::sync::atomic::Ordering::SeqCst);

    match dump::build_capture_dump_gzip(&flows, &classified, flows_capped) {
        Ok(bytes) => {
            log::info(&format!(
                "capture dump: {} flows ({} classified), {} bytes gzip",
                flows.len(),
                classified.len(),
                bytes.len()
            ));
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
        Err(e) => {
            log::error(&format!("capture dump failed: {e:#}"));
            (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response()
        }
    }
}

async fn download_handler(State(state): State<AppState>) -> impl IntoResponse {
    let bundle = state.export_bundle.read().await;
    match bundle.as_ref() {
        Some(b) if !b.zip_bytes.is_empty() => {
            let mut headers = HeaderMap::new();
            headers.insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/zip"),
            );
            headers.insert(
                header::CONTENT_DISPOSITION,
                HeaderValue::from_static("attachment; filename=\"spoor-export.zip\""),
            );
            (StatusCode::OK, headers, b.zip_bytes.clone()).into_response()
        }
        _ => (StatusCode::NOT_FOUND, "No export generated yet").into_response(),
    }
}

async fn start_handler(State(state): State<AppState>) -> impl IntoResponse {
    log::info("POST /api/start");
    if state.is_recording() {
        log::warn("start rejected: already recording");
        return (StatusCode::CONFLICT, "Already recording").into_response();
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

    let config = match browser_util::recording_config(state.chromium_executable.as_path()) {
        Ok(c) => c,
        Err(e) => {
            state.set_recording(false);
            log::error(&format!("recording browser config error: {e:#}"));
            return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
        }
    };

    log::info("launching recording browser (1280×800)");
    let (mut browser, handler) = match Browser::launch(config).await {
        Ok(b) => b,
        Err(e) => {
            state.set_recording(false);
            log::error(&format!("recording browser launch failed: {e:#}"));
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to launch browser: {e}"),
            )
                .into_response();
        }
    };
    log::info("recording browser launched");

    let handler_task = spawn_handler(handler, "recording");

    let page = match recording_page(&browser).await {
        Ok(p) => p,
        Err(e) => {
            state.set_recording(false);
            log::error(&format!("failed to get recording page: {e:#}"));
            let _ = browser.close().await;
            handler_task.abort();
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to open page: {e}"),
            )
                .into_response();
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
            Err(e) => log::error(&format!("capture task failed: {e:#}")),
        }
    });

    *state.session.lock().await = Some(BrowserSession {
        browser,
        handler_task,
        capture_task,
    });

    log::info(
        "recording started — panel window + recording browser (close recording with Stop, not ✕)",
    );
    (StatusCode::OK, "Recording started").into_response()
}

async fn recording_page(browser: &Browser) -> anyhow::Result<chromiumoxide::Page> {
    let pages = browser.pages().await.context("list browser tabs")?;
    let tab_count = pages.len();
    if let Some(page) = pages.into_iter().next() {
        log::info(&format!(
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

async fn stop_handler(State(state): State<AppState>) -> impl IntoResponse {
    log::info("POST /api/stop");
    if !state.is_recording() {
        log::warn("stop rejected: not recording");
        return (StatusCode::CONFLICT, "Not recording").into_response();
    }

    state.set_recording(false);
    let flow_count = state.flows.read().await.len();
    log::info(&format!("stopping recording ({flow_count} flows captured)"));

    let session = state.session.lock().await.take();
    let Some(mut session) = session else {
        log::error("stop failed: no active browser session");
        return (StatusCode::INTERNAL_SERVER_ERROR, "No active session").into_response();
    };

    log::info("closing recording browser");
    if let Err(e) = session.browser.close().await {
        log::error(&format!("failed to close recording browser: {e:#}"));
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to close browser: {e}"),
        )
            .into_response();
    }

    let _ = session.capture_task.await;
    let _ = session.handler_task.await;
    log::info("recording browser shut down");

    state.set_analyzing(true);
    let state_clone = state.clone();
    tokio::spawn(async move {
        log::info("discover pipeline started");
        let result = pipeline::run_discover(&state_clone).await;
        state_clone.set_analyzing(false);
        match result {
            Ok(()) => {
                let n = state_clone.candidates.read().await.len();
                log::info(&format!("discover finished — {n} candidates ready"));
            }
            Err(e) => log::error(&format!("discover failed: {e:#}")),
        }
    });

    (StatusCode::OK, "Recording stopped, discovering APIs…").into_response()
}
