//! Library CLI for credential brokering. The binary is wired from `main.rs` later.

use std::path::PathBuf;

use clap::{Parser, Subcommand};

mod auth_cmd;
mod call_cmd;

pub use auth_cmd::run_auth;
pub use call_cmd::run_call;

#[derive(Debug, Parser)]
#[command(name = "spoor", about = "Capture API traffic and export an agent pack")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Open the recording browser, wait for the observed credential, store it in the OS keychain.
    Auth {
        #[arg(long)]
        surface: String,
        /// Unpacked pack directory (defaults to the current directory).
        #[arg(long)]
        pack: Option<PathBuf>,
    },
    /// Replay one HTTP operation, injecting the keychain credential at request time.
    Call {
        #[arg(long)]
        surface: String,
        #[arg(long)]
        op: String,
        #[arg(long)]
        pack: Option<PathBuf>,
        /// Allow POST/PUT/PATCH/DELETE. Default is GET/HEAD/OPTIONS only.
        #[arg(long)]
        allow_mutating: bool,
    },
}

pub async fn run(command: Command) -> anyhow::Result<()> {
    match command {
        Command::Auth { surface, pack } => {
            run_auth(&surface, pack.as_deref()).await?;
        }
        Command::Call {
            surface,
            op,
            pack,
            allow_mutating,
        } => {
            run_call(&surface, &op, pack.as_deref(), allow_mutating).await?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn parses_auth_and_call() {
        let cli =
            Cli::try_parse_from(["spoor", "auth", "--surface", "api-example-test_rest"]).unwrap();
        match cli.command {
            Command::Auth { surface, pack } => {
                assert_eq!(surface, "api-example-test_rest");
                assert!(pack.is_none());
            }
            _ => panic!("expected auth"),
        }
        let cli = Cli::try_parse_from([
            "spoor",
            "call",
            "--surface",
            "api-example-test_rest",
            "--op",
            "rest|https://api.example.test|GET|/v1/me",
            "--allow-mutating",
        ])
        .unwrap();
        match cli.command {
            Command::Call {
                surface,
                op,
                allow_mutating,
                ..
            } => {
                assert_eq!(surface, "api-example-test_rest");
                assert!(op.contains("/v1/me"));
                assert!(allow_mutating);
            }
            _ => panic!("expected call"),
        }
    }
}
