use std::path::PathBuf;
use std::time::Duration;

use serde::Serialize;
use spoor::types::{AppState, FilterPreferenceRequest, GenerateRequest, GenerateSelection};
use spoor::ui::{
    candidates_snapshot, capture_dump_gzip, export_zip_bytes, generate_export,
    persist_filter_preference, run_discover_session, start_recording, status_snapshot,
    stop_recording, CandidatesSnapshot, FilterOutcome, GenerateOutcome, StatusSnapshot,
};
use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_dialog::DialogExt;

const TRAY_ID: &str = "main";

#[derive(Clone, Serialize)]
struct DiscoverFinished {
    ok: bool,
    error: Option<String>,
    origins: Vec<String>,
    candidates: Vec<spoor::types::Candidate>,
}

fn load_env() {
    let mut candidates = vec![
        PathBuf::from(".env"),
        PathBuf::from("../.env"),
        PathBuf::from("../../.env"),
    ];
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join(".env"));
            if let Some(parent) = dir.parent() {
                candidates.push(parent.join(".env"));
            }
        }
    }
    for path in candidates {
        if path.exists() {
            let _ = dotenvy::from_path(&path);
            break;
        }
    }
}

fn init_tracing() {
    use tracing_subscriber::EnvFilter;
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        EnvFilter::new("warn,spoor=info,chromiumoxide=error,tungstenite=error")
    });
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .try_init()
        .ok();
}

async fn emit_status(app: &AppHandle, state: &AppState) {
    let snap = status_snapshot(state).await;
    let _ = app.emit("status", &snap);
    if let Some(tray) = app.tray_by_id(TRAY_ID) {
        let tooltip = if snap.recording {
            format!("Spoor — recording · {} captured", snap.flow_count)
        } else if snap.analyzing {
            "Spoor — discovering APIs…".to_string()
        } else {
            format!("Spoor — {} captured", snap.flow_count)
        };
        let _ = tray.set_tooltip(Some(&tooltip));
    }
}

async fn do_start(app: &AppHandle, state: &AppState) -> Result<(), String> {
    start_recording(state).await.map_err(|e| e.to_string())?;
    emit_status(app, state).await;
    Ok(())
}

async fn do_stop(app: &AppHandle, state: &AppState) -> Result<(), String> {
    stop_recording(state).await.map_err(|e| e.to_string())?;
    emit_status(app, state).await;

    let app = app.clone();
    let state = state.clone();
    tauri::async_runtime::spawn(async move {
        let result = run_discover_session(&state).await;
        let snap = candidates_snapshot(&state).await;
        let payload = DiscoverFinished {
            ok: result.is_ok(),
            error: result.err().map(|e| e.to_string()),
            origins: snap.origins,
            candidates: snap.candidates,
        };
        let _ = app.emit("discover-finished", &payload);
        emit_status(&app, &state).await;
    });
    Ok(())
}

fn show_main(app: &AppHandle) {
    if let Some(win) = app.get_webview_window("main") {
        let _ = win.show();
        let _ = win.unminimize();
        let _ = win.set_focus();
    }
}

async fn flow_watch(app: AppHandle, state: AppState) {
    let mut last = usize::MAX;
    loop {
        tokio::time::sleep(Duration::from_millis(400)).await;
        if !state.is_recording() {
            continue;
        }
        let n = state.flows.read().await.len();
        if n == last {
            continue;
        }
        last = n;
        let _ = app.emit("flow-count", n);
        emit_status(&app, &state).await;
    }
}

fn save_via_dialog(
    app: &AppHandle,
    bytes: Vec<u8>,
    title: &str,
    file_name: &str,
    filter_name: &str,
    ext: &str,
) -> Result<Option<String>, String> {
    let picked = app
        .dialog()
        .file()
        .set_title(title)
        .set_file_name(file_name)
        .add_filter(filter_name, &[ext])
        .blocking_save_file();
    let Some(file) = picked else {
        return Ok(None);
    };
    let path = file.into_path().map_err(|e| e.to_string())?;
    std::fs::write(&path, bytes).map_err(|e| e.to_string())?;
    Ok(Some(path.display().to_string()))
}

#[tauri::command]
async fn start(app: AppHandle, state: State<'_, AppState>) -> Result<StatusSnapshot, String> {
    do_start(&app, &state).await?;
    Ok(status_snapshot(&state).await)
}

#[tauri::command]
async fn stop(app: AppHandle, state: State<'_, AppState>) -> Result<StatusSnapshot, String> {
    do_stop(&app, &state).await?;
    Ok(status_snapshot(&state).await)
}

#[tauri::command]
async fn status(state: State<'_, AppState>) -> Result<StatusSnapshot, String> {
    Ok(status_snapshot(&state).await)
}

#[tauri::command]
async fn candidates(state: State<'_, AppState>) -> Result<CandidatesSnapshot, String> {
    Ok(candidates_snapshot(&state).await)
}

#[tauri::command]
async fn generate(
    app: AppHandle,
    state: State<'_, AppState>,
    selected: Vec<GenerateSelection>,
    redact: bool,
) -> Result<GenerateOutcome, String> {
    let req = GenerateRequest {
        origin: None,
        selected,
        ignore_patterns: Vec::new(),
        redact,
    };
    let outcome = generate_export(&state, req)
        .await
        .map_err(|e| e.to_string())?;
    emit_status(&app, &state).await;
    Ok(outcome)
}

#[tauri::command]
async fn save_export(app: AppHandle, state: State<'_, AppState>) -> Result<Option<String>, String> {
    let bytes = export_zip_bytes(&state).await.map_err(|e| e.to_string())?;
    let app2 = app.clone();
    tokio::task::spawn_blocking(move || {
        save_via_dialog(
            &app2,
            bytes,
            "Save Spoor export",
            "spoor-export.zip",
            "Zip archive",
            "zip",
        )
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn save_dump(app: AppHandle, state: State<'_, AppState>) -> Result<Option<String>, String> {
    let bytes = capture_dump_gzip(&state).await.map_err(|e| e.to_string())?;
    let app2 = app.clone();
    tokio::task::spawn_blocking(move || {
        save_via_dialog(
            &app2,
            bytes,
            "Save capture dump",
            "spoor-capture.json.gz",
            "Gzip JSON",
            "gz",
        )
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
fn set_filter(pattern: String, action: String) -> Result<FilterOutcome, String> {
    persist_filter_preference(FilterPreferenceRequest { pattern, action })
        .map_err(|e| e.to_string())
}

fn setup_tray(app: &tauri::App) -> tauri::Result<()> {
    let show = MenuItem::with_id(app, "show", "Show Spoor", true, None::<&str>)?;
    let record = MenuItem::with_id(app, "record", "Start recording", true, None::<&str>)?;
    let stop = MenuItem::with_id(app, "stop", "Stop", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&show, &record, &stop, &quit])?;

    let mut tray = TrayIconBuilder::with_id(TRAY_ID)
        .menu(&menu)
        .show_menu_on_left_click(true)
        .tooltip("Spoor")
        .on_menu_event(|app, event| {
            let id = event.id.as_ref().to_string();
            match id.as_str() {
                "show" => show_main(app),
                "quit" => app.exit(0),
                "record" | "stop" => {
                    let app = app.clone();
                    tauri::async_runtime::spawn(async move {
                        let Some(state) = app.try_state::<AppState>() else {
                            return;
                        };
                        let result = if id == "record" {
                            do_start(&app, state.inner()).await
                        } else {
                            do_stop(&app, state.inner()).await
                        };
                        if let Err(e) = result {
                            spoor::log::error(format!("tray {id} failed: {e}"));
                        }
                    });
                }
                _ => {}
            }
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::DoubleClick {
                button: MouseButton::Left,
                ..
            } = event
            {
                show_main(tray.app_handle());
            }
        });
    if let Some(icon) = app.default_window_icon() {
        tray = tray.icon(icon.clone());
    }
    tray.build(app)?;
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    load_env();
    init_tracing();
    spoor::log::init(cfg!(debug_assertions));

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let chromium = tauri::async_runtime::block_on(spoor::browser_util::ensure_chromium())?;
            let state = AppState::new(chromium);
            let watch_state = state.clone();
            let watch_app = app.handle().clone();
            app.manage(state);
            tauri::async_runtime::spawn(flow_watch(watch_app, watch_state));
            setup_tray(app)?;
            Ok(())
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .invoke_handler(tauri::generate_handler![
            start,
            stop,
            status,
            candidates,
            generate,
            save_export,
            save_dump,
            set_filter
        ])
        .build(tauri::generate_context!())
        .expect("error while building Spoor")
        .run(|app_handle, event| {
            if let tauri::RunEvent::Reopen { .. } = event {
                show_main(app_handle);
            }
        });
}
