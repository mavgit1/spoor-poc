use clap::Parser;

use spoor::cli::{self, Cli};
use spoor::log;

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    init_tracing(cli.verbose);
    log::init(cli.verbose);
    if let Err(e) = cli::run(cli).await {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

fn init_tracing(verbose: bool) {
    use tracing_subscriber::EnvFilter;
    // chromiumoxide warns on CDP events it doesn't model; harmless noise.
    let filter = if verbose {
        EnvFilter::try_from_default_env().unwrap_or_else(|_| {
            EnvFilter::new("info,spoor=debug,chromiumoxide=error,tungstenite=error")
        })
    } else {
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("error"))
    };
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .with_writer(std::io::stderr)
        .try_init()
        .ok();
}
