mod browser_util;
mod capture;
mod classify;
mod discover;
mod export;
mod ir;
mod log;
mod pipeline;
mod rest;
mod types;
mod ui;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::Context;
use axum::Router;
use clap::Parser;
use tokio::net::TcpListener;
use tokio::signal;
use tokio::sync::broadcast;

use crate::types::AppState;
use crate::ui::router;

#[derive(Parser)]
#[command(name = "spoor", about = "Capture browser API traffic and export OpenAPI")]
struct Cli {
    /// Launch the chromeless control panel and server
    #[arg(long)]
    app: bool,

    /// Verbose logging (browser lifecycle, CDP, capture) for troubleshooting
    #[arg(short, long)]
    verbose: bool,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    if !cli.app {
        eprintln!("Usage: spoor --app [--verbose]");
        std::process::exit(1);
    }

    init_tracing(cli.verbose);
    log::init(cli.verbose);
    load_env();

    let port: u16 = std::env::var("SPOOR_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(3000);

    let chromium = browser_util::ensure_chromium().await?;
    let state = AppState::new(chromium.clone());
    let app: Router = router(state);

    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let listener = TcpListener::bind(addr)
        .await
        .with_context(|| format!("bind {addr}"))?;

    let panel_url = format!("http://127.0.0.1:{port}/");
    log::info(&format!("control panel at {panel_url}"));
    let (shutdown_tx, _) = broadcast::channel::<()>(1);
    let panel_shutdown = shutdown_tx.subscribe();
    let panel_exec = chromium;
    let panel_url_spawn = panel_url.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(400)).await;
        browser_util::run_panel_browser(panel_exec, panel_url_spawn, panel_shutdown).await;
    });
    if cli.verbose {
        log::info("press Ctrl+C to quit");
    } else {
        log::info("press Ctrl+C to quit (use --verbose for troubleshooting logs)");
    }

    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            shutdown_signal().await;
            let _ = shutdown_tx.send(());
        })
        .await
        .context("server error")?;

    Ok(())
}

fn init_tracing(verbose: bool) {
    use tracing_subscriber::EnvFilter;
    // Operational only: chromiumoxide warns on CDP events it doesn't model (harmless).
    // Not a product rule — does not affect capture/classification.
    let filter = if verbose {
        EnvFilter::try_from_default_env().unwrap_or_else(|_| {
            EnvFilter::new("info,spoor=debug,chromiumoxide=error,tungstenite=error")
        })
    } else {
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn"))
    };
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .try_init()
        .ok();
}

fn load_env() {
    let candidates = [PathBuf::from(".env"), PathBuf::from("../.env")];
    for path in candidates {
        if path.exists() {
            let _ = dotenvy::from_path(&path);
            break;
        }
    }
}

async fn shutdown_signal() {
    let _ = signal::ctrl_c().await;
    log::info("shutting down…");
}
