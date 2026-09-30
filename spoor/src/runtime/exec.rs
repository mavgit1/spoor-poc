//! `exec`: run a script inside a site's logged-in browser and return its result.
//!
//! The script is the body of an async function with `args` in scope:
//!
//! ```js
//! const r = await fetch('/api/me');
//! return { status: r.status, me: await r.json() };
//! ```
//!
//! Mechanics, per call:
//! 1. Open a tab in its own minimized window on the site's profile and load
//!    `url` — a normal page load, so cookies, SSO and the page's own JS behave
//!    exactly as for the human.
//! 2. Turn on `Fetch` interception for that tab only: every request the script
//!    makes is paced and written to the audit log before it is released.
//! 3. Start the script without awaiting it in CDP, then poll for completion.
//!    No CDP command runs long, so a 10-minute export never trips a CDP timeout.
//! 4. Close the tab.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use chromiumoxide::Page;
use chromiumoxide::cdp::browser_protocol::fetch::{
    ContinueRequestParams, EnableParams as FetchEnableParams, EventRequestPaused, RequestPattern,
};
use chromiumoxide::cdp::browser_protocol::target::{
    CreateTargetParams, WindowState as TargetWindowState,
};
use chromiumoxide::cdp::js_protocol::runtime::{
    EvaluateParams, EventConsoleApiCalled, RemoteObject,
};
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::Mutex;
use tokio::task::JoinHandle;

use super::guard::{Audit, Pacer};

pub const DEFAULT_TIMEOUT_SECS: u64 = 300;
const POLL_INTERVAL: Duration = Duration::from_millis(200);
const MAX_LOG_LINES: usize = 2_000;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ExecRequest {
    pub script: String,
    #[serde(default)]
    pub args: Value,
    /// Page to load before running. Defaults to the site's url.
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub timeout_secs: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecOutcome {
    pub exec_id: String,
    pub ok: bool,
    #[serde(default)]
    pub result: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// `console.*` output from the tab while the script ran.
    #[serde(default)]
    pub logs: Vec<String>,
    /// Requests the tab made after page load (the paced ones).
    pub requests: u64,
    pub duration_ms: u64,
}

pub(crate) struct ExecEnv<'a> {
    pub browser: &'a chromiumoxide::Browser,
    pub pacer: &'a Arc<Pacer>,
    pub audit: &'a Arc<Audit>,
    pub gap: Duration,
}

/// Wrap the user's function body. The result is kept in a page global that
/// [`poll_expression`] reads; `JSON.stringify` inside the page means anything
/// JSON-able round-trips, and `undefined` becomes `null`.
pub(crate) fn start_expression(key: &str, script: &str, args: &Value) -> String {
    let key = serde_json::to_string(key).unwrap_or_default();
    format!(
        r#"(() => {{
  const k = {key};
  window[k] = {{ done: false }};
  (async (args) => {{
{script}
  }})({args}).then(
    v => {{ window[k] = {{ done: true, ok: true, value: JSON.stringify(v === undefined ? null : v) }}; }},
    e => {{ window[k] = {{ done: true, ok: false, error: String((e && e.stack) || e) }}; }}
  );
  return true;
}})()"#
    )
}

pub(crate) fn poll_expression(key: &str) -> String {
    let key = serde_json::to_string(key).unwrap_or_default();
    format!(
        r#"(() => {{
  const s = window[{key}];
  if (!s) return JSON.stringify({{ lost: true }});
  if (!s.done) return null;
  delete window[{key}];
  return JSON.stringify(s);
}})()"#
    )
}

#[derive(Debug, Deserialize)]
struct PollState {
    #[serde(default)]
    lost: bool,
    #[serde(default)]
    ok: bool,
    #[serde(default)]
    value: Option<String>,
    #[serde(default)]
    error: Option<String>,
}

pub(crate) async fn run(
    env: ExecEnv<'_>,
    exec_id: &str,
    url: &str,
    req: &ExecRequest,
) -> ExecOutcome {
    let started = Instant::now();
    let requests = Arc::new(AtomicU64::new(0));
    let logs = Arc::new(Mutex::new(Vec::new()));
    env.audit.write(
        exec_id,
        "exec",
        json!({ "url": url, "script_bytes": req.script.len() }),
    );

    let result = run_inner(&env, exec_id, url, req, &requests, &logs).await;
    let (ok, result, error) = match result {
        Ok(v) => (true, v, None),
        Err(e) => (false, Value::Null, Some(format!("{e:#}"))),
    };
    let outcome = ExecOutcome {
        exec_id: exec_id.to_string(),
        ok,
        result,
        error,
        logs: std::mem::take(&mut *logs.lock().await),
        requests: requests.load(Ordering::SeqCst),
        duration_ms: started.elapsed().as_millis() as u64,
    };
    env.audit.write(
        exec_id,
        "done",
        json!({
            "ok": outcome.ok,
            "requests": outcome.requests,
            "duration_ms": outcome.duration_ms,
            "error": outcome.error,
        }),
    );
    outcome
}

async fn run_inner(
    env: &ExecEnv<'_>,
    exec_id: &str,
    url: &str,
    req: &ExecRequest,
    requests: &Arc<AtomicU64>,
    logs: &Arc<Mutex<Vec<String>>>,
) -> Result<Value> {
    let timeout = Duration::from_secs(req.timeout_secs.unwrap_or(DEFAULT_TIMEOUT_SECS).max(1));
    let page = open_exec_tab(env.browser).await?;
    let mut tasks: Vec<JoinHandle<()>> = Vec::new();
    tasks.push(spawn_console_collector(&page, Arc::clone(logs)).await?);

    let outcome = tokio::time::timeout(timeout, async {
        env.audit.write(exec_id, "navigate", json!({ "url": url }));
        page.goto(url)
            .await
            .with_context(|| format!("load {url}"))?;
        tasks.push(
            spawn_pacing(
                &page,
                exec_id.to_string(),
                Arc::clone(env.pacer),
                Arc::clone(env.audit),
                env.gap,
                Arc::clone(requests),
            )
            .await?,
        );
        run_script(&page, exec_id, req).await
    })
    .await
    .unwrap_or_else(|_| Err(anyhow!("timed out after {}s", timeout.as_secs())));

    for task in tasks {
        task.abort();
    }
    let _ = page.close().await;
    outcome
}

/// Upper bound on opening the exec tab. A target chromiumoxide never attaches
/// to (e.g. CDP `hidden: true`) would otherwise make `new_page` wait forever.
const OPEN_TAB_TIMEOUT: Duration = Duration::from_secs(15);

/// Each exec gets its own minimized window: same profile and cookies, but it
/// never lands in the user's tab strip. Falls back to a background tab.
///
/// (CDP `hidden: true` targets would be nicer, but chromiumoxide does not
/// attach to them, so `new_page` never resolves.)
async fn open_exec_tab(browser: &chromiumoxide::Browser) -> Result<Page> {
    let minimized = CreateTargetParams::builder()
        .url("about:blank")
        .new_window(true)
        .window_state(TargetWindowState::Minimized)
        .background(true)
        .build()
        .map_err(|e| anyhow!(e))?;
    match tokio::time::timeout(OPEN_TAB_TIMEOUT, browser.new_page(minimized)).await {
        Ok(Ok(page)) => return Ok(page),
        Ok(Err(e)) => crate::log::debug(format!(
            "minimized exec window failed ({e}); using a background tab"
        )),
        Err(_) => crate::log::debug("minimized exec window timed out; using a background tab"),
    }
    let background = CreateTargetParams::builder()
        .url("about:blank")
        .background(true)
        .build()
        .map_err(|e| anyhow!(e))?;
    tokio::time::timeout(OPEN_TAB_TIMEOUT, browser.new_page(background))
        .await
        .map_err(|_| anyhow!("timed out opening the exec tab"))?
        .context("open exec tab")
}

async fn spawn_console_collector(
    page: &Page,
    logs: Arc<Mutex<Vec<String>>>,
) -> Result<JoinHandle<()>> {
    let mut events = page.event_listener::<EventConsoleApiCalled>().await?;
    Ok(tokio::spawn(async move {
        while let Some(ev) = events.next().await {
            let line = ev
                .args
                .iter()
                .map(remote_object_text)
                .collect::<Vec<_>>()
                .join(" ");
            let mut logs = logs.lock().await;
            if logs.len() < MAX_LOG_LINES {
                logs.push(format!("{}: {line}", ev.r#type.as_ref()));
            }
        }
    }))
}

fn remote_object_text(obj: &RemoteObject) -> String {
    match &obj.value {
        Some(Value::String(s)) => s.clone(),
        Some(v) => v.to_string(),
        None => obj.description.clone().unwrap_or_default(),
    }
}

/// Intercept every request of this tab: audit it, wait for the pacing slot,
/// release it. Redirect hops and `data:`/`blob:` URLs are released without
/// waiting — they are not new requests to the server.
async fn spawn_pacing(
    page: &Page,
    exec_id: String,
    pacer: Arc<Pacer>,
    audit: Arc<Audit>,
    gap: Duration,
    requests: Arc<AtomicU64>,
) -> Result<JoinHandle<()>> {
    let mut paused = page.event_listener::<EventRequestPaused>().await?;
    page.execute(FetchEnableParams {
        patterns: Some(vec![RequestPattern {
            url_pattern: Some("*".into()),
            resource_type: None,
            request_stage: None,
        }]),
        handle_auth_requests: None,
    })
    .await
    .context("enable request pacing")?;

    let page = page.clone();
    Ok(tokio::spawn(async move {
        while let Some(ev) = paused.next().await {
            let url = &ev.request.url;
            let local = url.starts_with("data:") || url.starts_with("blob:");
            if !local {
                audit.write(
                    &exec_id,
                    "request",
                    json!({
                        "method": ev.request.method,
                        "url": url,
                        "type": format!("{:?}", ev.resource_type),
                    }),
                );
                if ev.redirected_request_id.is_none() {
                    requests.fetch_add(1, Ordering::SeqCst);
                    pacer.wait(gap).await;
                }
            }
            if let Err(e) = page
                .execute(ContinueRequestParams::new(ev.request_id.clone()))
                .await
            {
                crate::log::debug(format!("continue request failed: {e}"));
            }
        }
    }))
}

async fn evaluate_string(page: &Page, expression: String) -> Result<Option<String>> {
    let params = EvaluateParams::builder()
        .expression(expression)
        .return_by_value(true)
        .await_promise(false)
        .build()
        .map_err(|e| anyhow!(e))?;
    let resp = page.execute(params).await.context("evaluate in page")?;
    if let Some(ex) = &resp.result.exception_details {
        let detail = ex
            .exception
            .as_ref()
            .and_then(|o| o.description.clone())
            .unwrap_or_else(|| ex.text.clone());
        bail!("script error: {detail}");
    }
    Ok(match &resp.result.result.value {
        Some(Value::String(s)) => Some(s.clone()),
        _ => None,
    })
}

async fn run_script(page: &Page, exec_id: &str, req: &ExecRequest) -> Result<Value> {
    let key = format!("__spoor_exec_{exec_id}");
    let args = if req.args.is_null() {
        json!({})
    } else {
        req.args.clone()
    };
    evaluate_string(page, start_expression(&key, &req.script, &args)).await?;
    loop {
        tokio::time::sleep(POLL_INTERVAL).await;
        let Some(raw) = evaluate_string(page, poll_expression(&key))
            .await
            .context("poll exec state (tab closed or crashed?)")?
        else {
            continue;
        };
        let state: PollState = serde_json::from_str(&raw).context("decode exec state")?;
        if state.lost {
            bail!("the page navigated or reloaded while the script ran; its state was lost");
        }
        if !state.ok {
            bail!("{}", state.error.unwrap_or_else(|| "script failed".into()));
        }
        let value = state.value.unwrap_or_else(|| "null".into());
        return serde_json::from_str(&value).context("decode script result");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expressions_quote_key_and_embed_args() {
        let start = start_expression("__spoor_exec_1", "return args.n + 1;", &json!({ "n": 1 }));
        assert!(start.contains(r#"const k = "__spoor_exec_1";"#), "{start}");
        assert!(start.contains(r#"})({"n":1})"#), "{start}");
        assert!(start.contains("return args.n + 1;"));
        let poll = poll_expression("__spoor_exec_1");
        assert!(poll.contains(r#"window["__spoor_exec_1"]"#), "{poll}");
    }

    #[test]
    fn poll_state_decodes() {
        let s: PollState =
            serde_json::from_str(r#"{"done":true,"ok":true,"value":"[1,2]"}"#).unwrap();
        assert!(s.ok && !s.lost);
        assert_eq!(s.value.as_deref(), Some("[1,2]"));
        let s: PollState = serde_json::from_str(r#"{"lost":true}"#).unwrap();
        assert!(s.lost);
    }
}
