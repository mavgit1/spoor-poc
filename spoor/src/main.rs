use std::io::Read;
use std::net::SocketAddr;
use std::path::PathBuf;

use anyhow::Context;
use axum::Router;
use clap::Parser;
use tokio::net::TcpListener;
use tokio::signal;

use spoor::log;
use spoor::types::AppState;
use spoor::ui::router;

#[derive(Parser)]
#[command(
    name = "spoor",
    about = "Capture browser API traffic and export integration packs"
)]
struct Cli {
    /// Credential brokering: `spoor auth` / `spoor call`.
    #[command(subcommand)]
    command: Option<spoor::cli::Command>,

    /// Bind the headless HTTP API. Requires a bearer token (see --token).
    /// The desktop app does not use this path — it talks to the library over IPC.
    #[arg(long)]
    serve: bool,

    /// Bearer token for --serve. Generated and printed if omitted.
    #[arg(long)]
    token: Option<String>,

    /// Deprecated: the Chromium control panel is gone. Use the Spoor desktop app,
    /// or --serve for the headless HTTP API.
    #[arg(long)]
    app: bool,

    /// Verbose logging (browser lifecycle, CDP, capture) for troubleshooting
    #[arg(short, long)]
    verbose: bool,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    // Brokering commands are self-contained: no server, no browser unless the
    // subcommand opens one itself.
    if let Some(command) = cli.command {
        init_tracing(cli.verbose);
        log::init(cli.verbose);
        load_env();
        return spoor::cli::run(command).await;
    }

    if cli.app && !cli.serve {
        eprintln!(
            "The Chromium control panel (--app) has been removed.\n\
             \n\
             Desktop UI:  cargo tauri dev   (from spoor-app/)\n\
             Headless API: spoor --serve [--token TOKEN] [--verbose]\n\
             \n\
             --serve binds 127.0.0.1 only and requires a bearer token on every request."
        );
        std::process::exit(2);
    }
    if !cli.serve {
        eprintln!(
            "Usage: spoor --serve [--token TOKEN] [--verbose]\n\
             \n\
             The control panel is the Spoor desktop app (cargo tauri dev / Spoor.app).\n\
             --serve is the opt-in headless HTTP API for agents. It does not start a UI."
        );
        std::process::exit(1);
    }

    init_tracing(cli.verbose);
    log::init(cli.verbose);
    load_env();

    let token = match cli.token.filter(|t| !t.trim().is_empty()) {
        Some(t) => t,
        None => {
            let generated = generate_bearer_token()?;
            log::info(format!(
                "generated bearer token (Authorization: Bearer {generated})"
            ));
            generated
        }
    };

    let port: u16 = std::env::var("SPOOR_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(3000);

    let chromium = spoor::browser_util::ensure_chromium().await?;
    let state = AppState::new(chromium);
    let app: Router = router(state, token);

    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let listener = TcpListener::bind(addr)
        .await
        .with_context(|| format!("bind {addr}"))?;

    log::info(format!(
        "headless API at http://127.0.0.1:{port}/ (Authorization: Bearer required)"
    ));
    if cli.verbose {
        log::info("press Ctrl+C to quit");
    } else {
        log::info("press Ctrl+C to quit (use --verbose for troubleshooting logs)");
    }

    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            shutdown_signal().await;
        })
        .await
        .context("server error")?;

    Ok(())
}

fn generate_bearer_token() -> anyhow::Result<String> {
    let mut bytes = [0u8; 32];
    fill_random(&mut bytes)?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

fn fill_random(buf: &mut [u8]) -> anyhow::Result<()> {
    #[cfg(unix)]
    {
        std::fs::File::open("/dev/urandom")
            .context("open /dev/urandom")?
            .read_exact(buf)
            .context("read /dev/urandom")?;
        Ok(())
    }
    #[cfg(not(unix))]
    {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        std::process::id().hash(&mut hasher);
        std::time::SystemTime::now().hash(&mut hasher);
        let n = hasher.finish();
        for (i, b) in buf.iter_mut().enumerate() {
            *b = n.rotate_left((i as u32 * 7) % 64).to_le_bytes()[i % 8];
        }
        Ok(())
    }
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
