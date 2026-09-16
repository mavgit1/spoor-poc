//! Library CLI: credential brokering (`auth` / `call`) and session replay.

use std::path::PathBuf;

use clap::{Parser, Subcommand};

mod auth_cmd;
mod call_cmd;
mod replay_cmd;

pub use auth_cmd::run_auth;
pub use call_cmd::run_call;
pub use replay_cmd::{run_replay, run_sessions};

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
    /// List stored capture sessions.
    Sessions,
    /// Classify + discover a stored session; optionally generate a pack.
    Replay {
        /// Session id, session directory, `flows.jsonl`, JSON array, or `.json.gz` dump.
        target: String,
        /// Candidate ids to include in the pack. Repeatable. Nothing is selected by default.
        #[arg(long = "select", value_name = "ID")]
        select: Vec<String>,
        /// Select every discovered candidate. Must be explicit.
        #[arg(long, conflicts_with = "select")]
        all: bool,
        /// Write a v3 agent pack zip. Requires `--all` or `--select`.
        #[arg(long, value_name = "PATH")]
        out: Option<PathBuf>,
        /// Skip the LLM classify step even if `OPENROUTER_API_KEY` is set.
        #[arg(long)]
        no_llm: bool,
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
        Command::Sessions => {
            run_sessions().await?;
        }
        Command::Replay {
            target,
            select,
            all,
            out,
            no_llm,
        } => {
            run_replay(&target, &select, all, out.as_deref(), no_llm).await?;
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

    #[test]
    fn parses_sessions_and_replay() {
        let cli = Cli::try_parse_from(["spoor", "sessions"]).unwrap();
        assert!(matches!(cli.command, Command::Sessions));

        let cli = Cli::try_parse_from([
            "spoor",
            "replay",
            "2026-01-01T00-00-00Z-abc",
            "--no-llm",
            "--all",
            "--out",
            "pack.zip",
        ])
        .unwrap();
        match cli.command {
            Command::Replay {
                target,
                all,
                no_llm,
                out,
                select,
            } => {
                assert_eq!(target, "2026-01-01T00-00-00Z-abc");
                assert!(all);
                assert!(no_llm);
                assert_eq!(out.as_deref(), Some(std::path::Path::new("pack.zip")));
                assert!(select.is_empty());
            }
            _ => panic!("expected replay"),
        }

        let cli = Cli::try_parse_from([
            "spoor",
            "replay",
            "./flows.jsonl",
            "--select",
            "rest|https://api.example.test|GET|/v1/me",
            "--select",
            "rest|https://api.example.test|GET|/v1/orders",
        ])
        .unwrap();
        match cli.command {
            Command::Replay { select, all, .. } => {
                assert!(!all);
                assert_eq!(select.len(), 2);
            }
            _ => panic!("expected replay"),
        }
    }

    #[test]
    fn replay_all_conflicts_with_select() {
        let err = Cli::try_parse_from([
            "spoor",
            "replay",
            "sid",
            "--all",
            "--select",
            "rest|x|GET|/",
        ])
        .unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("cannot be used with") || msg.contains("--select"),
            "{msg}"
        );
    }
}
