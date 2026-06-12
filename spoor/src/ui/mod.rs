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
use crate::export;
use crate::ir;
use crate::log;
use crate::pipeline;
use crate::types::{AppState, BrowserSession, Candidate, GenerateRequest};

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
        .route("/api/download", get(download_handler))
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
    rest_endpoints: usize,
    traffic_graphql: usize,
    traffic_rest: usize,
}

async fn status_handler(State(state): State<AppState>) -> Json<StatusResponse> {
    let flow_count = state.flows.read().await.len();
    let candidates = state.candidates.read().await;
    let candidate_count = candidates.len();
    let graphql_ops = candidates
        .iter()
        .filter(|c| c.protocol == "graphql")
        .count();
    let rest_endpoints = candidates
        .iter()
        .filter(|c| c.protocol == "rest")
        .count();
    let classified = state.classified.read().await;
    let traffic_graphql = classified
        .iter()
        .filter(|c| c.protocol == Protocol::Graphql)
        .count();
    let traffic_rest = classified
        .iter()
        .filter(|c| c.protocol == Protocol::Rest)
        .count();
    let spec_ready = state.export_bundle.read().await.is_some();
    Json(StatusResponse {
        recording: state.is_recording(),
        analyzing: state.is_analyzing(),
        flow_count,
        spec_ready,
        candidate_count,
        graphql_ops,
        rest_endpoints,
        traffic_graphql,
        traffic_rest,
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
    let candidates = state.candidates.read().await.clone();
    Json(CandidatesResponse {
        origins,
        candidates,
    })
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

    match export::generate_bundle(&classified, &candidates, &req) {
        Ok(bundle) => {
            *state.export_bundle.write().await = Some(bundle);
            (StatusCode::OK, "Spec generated").into_response()
        }
        Err(e) => {
            log::error(&format!("generate failed: {e:#}"));
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
    let capture_task = tokio::spawn(async move {
        log::debug("capture task started");
        match capture::capture(capture_page, flows).await {
            Ok(()) => log::info("capture task ended (event listeners closed)"),
            Err(e) => log::error(&format!("capture task failed: {e:#}")),
        }
    });

    *state.session.lock().await = Some(BrowserSession {
        browser,
        handler_task,
        capture_task,
    });

    log::info("recording started — use the recording browser window");
    (StatusCode::OK, "Recording started").into_response()
}

async fn recording_page(browser: &Browser) -> anyhow::Result<chromiumoxide::Page> {
    let pages = browser.pages().await.context("list browser tabs")?;
    let tab_count = pages.len();
    if let Some(page) = pages.into_iter().next() {
        log::info(&format!("capture attached to main tab ({tab_count} tab(s) open)"));
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
