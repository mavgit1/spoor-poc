//! Command line. Everything except `serve`, `site`, `sessions`, `flows` and
//! `trace` talks to a running `spoor serve` (started in the background on
//! first use).

use std::io::Write;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use serde_json::{Value, json};

use crate::client::Client;
use crate::inspect;
use crate::runtime::{ExecOutcome, RecordInfo};
use crate::server::DEFAULT_PORT;
use crate::session::{SessionStore, format_bytes, load_source};
use crate::site::{Site, Sites};

#[derive(Debug, Parser)]
#[command(
    name = "spoor",
    version,
    about = "Keep logged-in browser sessions for websites and run scripts in them"
)]
pub struct Cli {
    /// Verbose logging (browser lifecycle, CDP) for troubleshooting.
    #[arg(short, long, global = true)]
    pub verbose: bool,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Run the session service (localhost HTTP API) in the foreground.
    Serve {
        #[arg(long, default_value_t = DEFAULT_PORT, env = "SPOOR_PORT")]
        port: u16,
        /// Bearer token. Generated if omitted; always written to serve.json.
        #[arg(long)]
        token: Option<String>,
    },
    /// Stop every site browser and exit the running `spoor serve`.
    Shutdown,
    /// Manage the site registry (sites.toml).
    Site {
        #[command(subcommand)]
        command: SiteCommand,
    },
    /// Show a site's browser window — log in, or look around.
    Open {
        site: String,
        #[arg(long)]
        url: Option<String>,
        /// Block until the site's check script reports logged in.
        #[arg(long)]
        wait: bool,
        /// With --wait: give up after this many seconds.
        #[arg(long, default_value_t = 600)]
        timeout: u64,
    },
    /// Record traffic while you use the site. Press Enter (or Ctrl+C) to stop.
    Record {
        site: String,
        #[arg(long)]
        url: Option<String>,
    },
    /// Run a script in a minimized tab of the site's logged-in browser.
    ///
    /// The script is the body of an async function with `args` in scope; its
    /// return value is printed as JSON. console.* output goes to stderr.
    Exec {
        site: String,
        /// Script file (function body). Use -e for inline code instead.
        script: Option<PathBuf>,
        /// Inline script (function body).
        #[arg(short = 'e', long = "eval", conflicts_with = "script")]
        code: Option<String>,
        /// JSON passed as `args`, or @file.json.
        #[arg(long)]
        args: Option<String>,
        /// Page to load before running (defaults to the site's url).
        #[arg(long)]
        url: Option<String>,
        #[arg(long, default_value_t = crate::runtime::DEFAULT_TIMEOUT_SECS)]
        timeout: u64,
        /// Print the whole outcome (result, logs, request count, timing).
        #[arg(long)]
        full: bool,
    },
    /// Is the browser running, and does the site's check script pass?
    Status { site: String },
    /// Kill switch: close a site's browser (or every site's), ending all work on it.
    Stop { site: Option<String> },
    /// List recordings, newest first.
    Sessions {
        #[arg(long)]
        site: Option<String>,
    },
    /// List a recording's requests; with --grep only those mentioning TEXT.
    Flows {
        /// Session id, session directory or flows file.
        session: String,
        #[arg(long)]
        grep: Option<String>,
        /// Print matching flows as full JSON lines (headers and bodies).
        #[arg(long)]
        full: bool,
    },
    /// Where did VALUE first appear in a recording, and where was it sent back?
    Trace { session: String, value: String },
}

#[derive(Debug, Subcommand)]
pub enum SiteCommand {
    /// Add or replace a site.
    Add {
        name: String,
        /// Start page; exec tabs load it before running a script.
        url: String,
        /// Script that returns truthy when logged in (used by status / open --wait).
        #[arg(long)]
        check: Option<PathBuf>,
        /// Minimum milliseconds between requests an exec makes (default 1000).
        #[arg(long)]
        min_gap_ms: Option<u64>,
    },
    /// List registered sites.
    List,
    /// Remove a site from the registry (its browser profile is kept).
    Remove { name: String },
}

fn print_json(v: &impl serde::Serialize) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(v)?);
    Ok(())
}

fn site_path(site: &str) -> String {
    format!("/sites/{site}")
}

fn read_args(raw: Option<&str>) -> Result<Value> {
    let Some(raw) = raw else {
        return Ok(json!({}));
    };
    let text = match raw.strip_prefix('@') {
        Some(path) => std::fs::read_to_string(path).with_context(|| format!("read {path}"))?,
        None => raw.to_string(),
    };
    serde_json::from_str(&text).context("--args must be JSON")
}

pub async fn run(cli: Cli) -> Result<()> {
    match cli.command {
        Command::Serve { port, token } => crate::server::serve(port, token).await,
        Command::Shutdown => {
            let client = Client::connect(false).await?;
            let v: Value = client.post("/shutdown", &json!({})).await?;
            print_json(&v)
        }
        Command::Site { command } => run_site(command),
        Command::Open {
            site,
            url,
            wait,
            timeout,
        } => {
            let client = Client::connect(true).await?;
            let v: Value = client
                .post(
                    &format!("{}/open", site_path(&site)),
                    &json!({ "url": url, "wait": wait, "timeout_secs": timeout }),
                )
                .await?;
            print_json(&v)?;
            if wait && v.get("logged_in") == Some(&Value::Bool(false)) {
                bail!("timed out waiting for {site} to be logged in");
            }
            Ok(())
        }
        Command::Record { site, url } => {
            let client = Client::connect(true).await?;
            let info: RecordInfo = client
                .post(
                    &format!("{}/record/start", site_path(&site)),
                    &json!({ "url": url }),
                )
                .await?;
            eprintln!(
                "Recording {site} into {} — use the browser window, then press Enter to stop.",
                info.session_id
            );
            wait_for_enter_or_ctrl_c().await;
            let info: RecordInfo = client
                .post(&format!("{}/record/stop", site_path(&site)), &json!({}))
                .await?;
            print_json(&info)
        }
        Command::Exec {
            site,
            script,
            code,
            args,
            url,
            timeout,
            full,
        } => {
            let script = match (script, code) {
                (Some(path), None) => std::fs::read_to_string(&path)
                    .with_context(|| format!("read {}", path.display()))?,
                (None, Some(code)) => code,
                _ => bail!("give a script file or -e CODE"),
            };
            let body = json!({
                "script": script,
                "args": read_args(args.as_deref())?,
                "url": url,
                "timeout_secs": timeout,
            });
            let client = Client::connect(true).await?;
            let outcome: ExecOutcome = client
                .post(&format!("{}/exec", site_path(&site)), &body)
                .await?;
            if full {
                return print_json(&outcome);
            }
            let mut stderr = std::io::stderr().lock();
            for line in &outcome.logs {
                let _ = writeln!(stderr, "{line}");
            }
            drop(stderr);
            if !outcome.ok {
                bail!("{}", outcome.error.unwrap_or_else(|| "exec failed".into()));
            }
            print_json(&outcome.result)
        }
        Command::Status { site } => {
            let client = Client::connect(true).await?;
            let v: Value = client.get(&format!("{}/status", site_path(&site))).await?;
            print_json(&v)
        }
        Command::Stop { site } => {
            // Nothing to stop if the service is not running.
            let Ok(client) = Client::connect(false).await else {
                return print_json(&json!({ "stopped": [] }));
            };
            let v: Value = match site {
                Some(site) => {
                    client
                        .post(&format!("{}/stop", site_path(&site)), &json!({}))
                        .await?
                }
                None => client.post("/stop", &json!({})).await?,
            };
            print_json(&v)
        }
        Command::Sessions { site } => run_sessions(site.as_deref()),
        Command::Flows {
            session,
            grep,
            full,
        } => {
            let loaded = load_source(&session)?;
            let flows = inspect::matching_flows(&loaded.flows, grep.as_deref());
            let mut out = std::io::stdout().lock();
            for f in flows {
                if full {
                    writeln!(out, "{}", serde_json::to_string(f)?)?;
                } else {
                    writeln!(out, "{}", inspect::flow_line(f))?;
                }
            }
            Ok(())
        }
        Command::Trace { session, value } => {
            let loaded = load_source(&session)?;
            let t = inspect::trace(&loaded.flows, &value);
            let section = |title: &str, hits: &[inspect::TraceHit]| {
                println!("{title} ({}):", hits.len());
                for h in hits {
                    println!("  #{} {} {}", h.sequence, h.method, h.url);
                    println!("     {}: {}", h.place, h.snippet);
                }
            };
            section("appears in", &t.appears_in);
            section("used in", &t.used_in);
            Ok(())
        }
    }
}

fn run_site(command: SiteCommand) -> Result<()> {
    let mut sites = Sites::load()?;
    match command {
        SiteCommand::Add {
            name,
            url,
            check,
            min_gap_ms,
        } => {
            // Absolute, so the check still resolves when serve runs elsewhere.
            let check = check
                .map(|p| {
                    std::path::absolute(&p).with_context(|| format!("resolve {}", p.display()))
                })
                .transpose()?;
            if let Some(p) = &check
                && !p.is_file()
            {
                bail!("check script {} does not exist", p.display());
            }
            sites.add(
                &name,
                Site {
                    url,
                    check,
                    min_gap_ms,
                },
            )?;
            sites.save()?;
            print_json(&json!({ "added": name, "site": sites.get(&name)? }))
        }
        SiteCommand::List => print_json(&sites.sites),
        SiteCommand::Remove { name } => {
            let removed = sites.remove(&name);
            sites.save()?;
            print_json(&json!({ "removed": removed.then_some(name) }))
        }
    }
}

fn run_sessions(site: Option<&str>) -> Result<()> {
    let store = SessionStore::default_store();
    let rows: Vec<Value> = store
        .list()?
        .into_iter()
        .filter(|s| site.is_none() || s.meta.site.as_deref() == site)
        .map(|s| {
            json!({
                "id": s.meta.id,
                "site": s.meta.site,
                "started_at": s.meta.started_at,
                "flows": s.meta.flow_count,
                "size": format_bytes(s.size_bytes),
                "path": s.path,
            })
        })
        .collect();
    print_json(&rows)
}

async fn wait_for_enter_or_ctrl_c() {
    let enter = tokio::task::spawn_blocking(|| {
        let mut line = String::new();
        let _ = std::io::stdin().read_line(&mut line);
    });
    tokio::select! {
        _ = enter => {}
        _ = tokio::signal::ctrl_c() => {}
    }
    // Let a trailing Ctrl+C newline settle before printing.
    tokio::time::sleep(Duration::from_millis(50)).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_core_commands() {
        let cli = Cli::try_parse_from([
            "spoor",
            "exec",
            "cas",
            "-e",
            "return 1",
            "--args",
            "{\"a\":1}",
        ])
        .unwrap();
        match cli.command {
            Command::Exec {
                site,
                code,
                args,
                script,
                ..
            } => {
                assert_eq!(site, "cas");
                assert_eq!(code.as_deref(), Some("return 1"));
                assert_eq!(args.as_deref(), Some("{\"a\":1}"));
                assert!(script.is_none());
            }
            other => panic!("{other:?}"),
        }
        assert!(Cli::try_parse_from(["spoor", "exec", "cas", "x.js", "-e", "1"]).is_err());
        assert!(matches!(
            Cli::try_parse_from(["spoor", "stop"]).unwrap().command,
            Command::Stop { site: None }
        ));
        assert!(matches!(
            Cli::try_parse_from(["spoor", "site", "add", "cas", "https://x.test/"])
                .unwrap()
                .command,
            Command::Site {
                command: SiteCommand::Add { .. }
            }
        ));
    }

    #[test]
    fn args_accept_inline_json_only() {
        assert_eq!(read_args(None).unwrap(), json!({}));
        assert_eq!(read_args(Some(r#"{"n":2}"#)).unwrap(), json!({ "n": 2 }));
        assert!(read_args(Some("not json")).is_err());
    }
}
