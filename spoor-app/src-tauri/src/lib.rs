//! Desktop shell for Spoor: a tray app and a small site manager.
//!
//! The app does not drive browsers itself. On launch it attaches to a running
//! `spoor serve`, or runs one in-process, and every button is a call to that
//! service — the same API the CLI and integrations use.

use std::sync::Arc;
use std::time::Duration;

use serde::Serialize;
use serde_json::{Value, json};
use spoor::client::Client;
use spoor::runtime::{RecordInfo, SiteInfo, SiteStatus};
use spoor::session::{SessionStore, format_bytes};
use spoor::site::{Site, Sites};
use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager, State};
use tokio::sync::OnceCell;

const TRAY_ID: &str = "main";
const SERVE_START_TIMEOUT: Duration = Duration::from_secs(120);

/// Connection to the session service, established once in the background.
#[derive(Default)]
struct Service {
    client: OnceCell<Arc<Client>>,
}

impl Service {
    async fn client(&self) -> Result<Arc<Client>, String> {
        self.client
            .get_or_try_init(connect_or_serve)
            .await
            .cloned()
            .map_err(|e| format!("{e:#}"))
    }
}

/// Attach to a running `spoor serve`, or start one inside this process.
async fn connect_or_serve() -> anyhow::Result<Arc<Client>> {
    if let Ok(client) = Client::connect(false).await {
        spoor::log::info("attached to a running spoor serve");
        return Ok(Arc::new(client));
    }
    tokio::spawn(async {
        if let Err(e) = spoor::server::serve(spoor::server::DEFAULT_PORT, None).await {
            spoor::log::error(format!("spoor serve failed: {e:#}"));
        }
    });
    // First launch may download Chromium before the service answers.
    let deadline = tokio::time::Instant::now() + SERVE_START_TIMEOUT;
    loop {
        tokio::time::sleep(Duration::from_millis(300)).await;
        match Client::connect(false).await {
            Ok(client) => return Ok(Arc::new(client)),
            Err(e) if tokio::time::Instant::now() > deadline => return Err(e),
            Err(_) => {}
        }
    }
}

fn err(e: anyhow::Error) -> String {
    format!("{e:#}")
}

async fn post<T: serde::de::DeserializeOwned>(service: &Service, path: &str) -> Result<T, String> {
    service
        .client()
        .await?
        .post(path, &json!({}))
        .await
        .map_err(err)
}

#[derive(Serialize)]
struct SessionRow {
    id: String,
    site: Option<String>,
    started_at: String,
    flows: usize,
    size: String,
    path: String,
}

#[tauri::command]
async fn sites(service: State<'_, Service>) -> Result<Vec<SiteInfo>, String> {
    service.client().await?.get("/sites").await.map_err(err)
}

#[tauri::command]
async fn site_status(service: State<'_, Service>, site: String) -> Result<SiteStatus, String> {
    let path = format!("/sites/{site}/status");
    service.client().await?.get(&path).await.map_err(err)
}

#[tauri::command]
async fn open_site(service: State<'_, Service>, site: String) -> Result<Value, String> {
    post(&service, &format!("/sites/{site}/open")).await
}

#[tauri::command]
async fn record_start(service: State<'_, Service>, site: String) -> Result<RecordInfo, String> {
    post(&service, &format!("/sites/{site}/record/start")).await
}

#[tauri::command]
async fn record_stop(service: State<'_, Service>, site: String) -> Result<RecordInfo, String> {
    post(&service, &format!("/sites/{site}/record/stop")).await
}

#[tauri::command]
async fn stop_site(service: State<'_, Service>, site: String) -> Result<Value, String> {
    post(&service, &format!("/sites/{site}/stop")).await
}

#[tauri::command]
async fn stop_all(service: State<'_, Service>) -> Result<Value, String> {
    post(&service, "/stop").await
}

#[tauri::command]
fn add_site(name: String, url: String, min_gap_ms: Option<u64>) -> Result<(), String> {
    let mut sites = Sites::load().map_err(err)?;
    // Keep an existing check script when re-adding a site from the app.
    let check = sites.sites.get(&name).and_then(|s| s.check.clone());
    sites
        .add(
            &name,
            Site {
                url,
                check,
                min_gap_ms,
            },
        )
        .map_err(err)?;
    sites.save().map_err(err)
}

#[tauri::command]
fn remove_site(name: String) -> Result<(), String> {
    let mut sites = Sites::load().map_err(err)?;
    sites.remove(&name);
    sites.save().map_err(err)
}

#[tauri::command]
fn sessions() -> Result<Vec<SessionRow>, String> {
    let rows = SessionStore::default_store()
        .list()
        .map_err(err)?
        .into_iter()
        .map(|s| SessionRow {
            id: s.meta.id,
            site: s.meta.site,
            started_at: s.meta.started_at,
            flows: s.meta.flow_count,
            size: format_bytes(s.size_bytes),
            path: s.path.display().to_string(),
        })
        .collect();
    Ok(rows)
}

fn show_main(app: &AppHandle) {
    if let Some(win) = app.get_webview_window("main") {
        let _ = win.show();
        let _ = win.unminimize();
        let _ = win.set_focus();
    }
}

/// Close every site browser and the service, then exit.
fn quit(app: &AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        if let Some(service) = app.try_state::<Service>()
            && let Some(client) = service.client.get()
        {
            let _ = tokio::time::timeout(
                Duration::from_secs(5),
                client.post::<Value>("/shutdown", &json!({})),
            )
            .await;
            // Let the service close its browsers before the process goes.
            tokio::time::sleep(Duration::from_millis(800)).await;
        }
        app.exit(0);
    });
}

fn setup_tray(app: &tauri::App) -> tauri::Result<()> {
    let show = MenuItem::with_id(app, "show", "Show Spoor", true, None::<&str>)?;
    let stop = MenuItem::with_id(app, "stop_all", "Stop all sites", true, None::<&str>)?;
    let quit_item = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&show, &stop, &quit_item])?;

    let mut tray = TrayIconBuilder::with_id(TRAY_ID)
        .menu(&menu)
        .show_menu_on_left_click(true)
        .tooltip("Spoor")
        .on_menu_event(|app, event| match event.id.as_ref() {
            "show" => show_main(app),
            "quit" => quit(app),
            "stop_all" => {
                let app = app.clone();
                tauri::async_runtime::spawn(async move {
                    if let Some(service) = app.try_state::<Service>()
                        && let Ok(client) = service.client().await
                        && let Err(e) = client.post::<Value>("/stop", &json!({})).await
                    {
                        spoor::log::error(format!("tray stop failed: {e:#}"));
                    }
                });
            }
            _ => {}
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

fn init_tracing() {
    use tracing_subscriber::EnvFilter;
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("warn,chromiumoxide=error,tungstenite=error"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .try_init()
        .ok();
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    init_tracing();
    spoor::log::init(cfg!(debug_assertions));

    tauri::Builder::default()
        .manage(Service::default())
        .setup(|app| {
            setup_tray(app)?;
            // Connect (or start the service) right away, not on first click.
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                if let Err(e) = handle.state::<Service>().client().await {
                    spoor::log::error(format!("session service unavailable: {e}"));
                }
            });
            Ok(())
        })
        .on_window_event(|window, event| {
            // Closing the window hides to the tray; Quit is in the tray menu.
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .invoke_handler(tauri::generate_handler![
            sites,
            site_status,
            open_site,
            record_start,
            record_stop,
            stop_site,
            stop_all,
            add_site,
            remove_site,
            sessions
        ])
        .build(tauri::generate_context!())
        .expect("error while building Spoor")
        .run(|_app_handle, _event| {
            // Dock-icon click (macOS only) brings the hidden window back.
            #[cfg(target_os = "macos")]
            if let tauri::RunEvent::Reopen { .. } = _event {
                show_main(_app_handle);
            }
        });
}
