//! HTTP client for a running `spoor serve` — what the CLI (and the desktop
//! app) use. Integrations in other languages do the same with any HTTP client:
//! read `serve.json`, send `Authorization: Bearer <token>`.

use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::cache_dir::serve_file_path;
use crate::server::ServeInfo;

const START_TIMEOUT: Duration = Duration::from_secs(90);

pub struct Client {
    base: String,
    token: String,
    http: reqwest::Client,
}

impl Client {
    fn new(base: String, token: String) -> Self {
        Self {
            base: base.trim_end_matches('/').to_string(),
            token,
            http: reqwest::Client::new(),
        }
    }

    /// `SPOOR_URL` + `SPOOR_TOKEN` if set, else `serve.json`. With `autostart`,
    /// launch `spoor serve` in the background when nothing answers.
    pub async fn connect(autostart: bool) -> Result<Self> {
        if let (Ok(url), Ok(token)) = (std::env::var("SPOOR_URL"), std::env::var("SPOOR_TOKEN")) {
            let client = Self::new(url, token);
            client.health().await.context("SPOOR_URL did not answer")?;
            return Ok(client);
        }
        if let Some(client) = Self::from_serve_file().await {
            return Ok(client);
        }
        if !autostart {
            bail!("spoor serve is not running (start it with `spoor serve`)");
        }
        spawn_background_serve()?;
        let deadline = tokio::time::Instant::now() + START_TIMEOUT;
        while tokio::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(300)).await;
            if let Some(client) = Self::from_serve_file().await {
                return Ok(client);
            }
        }
        bail!(
            "started `spoor serve` in the background but it did not come up within {}s — see the log at {}",
            START_TIMEOUT.as_secs(),
            crate::log::log_file_path().display()
        )
    }

    async fn from_serve_file() -> Option<Self> {
        let info = ServeInfo::read(&serve_file_path()).ok()?;
        let client = Self::new(info.url, info.token);
        client.health().await.ok()?;
        Some(client)
    }

    async fn health(&self) -> Result<()> {
        let resp = self
            .http
            .get(format!("{}/health", self.base))
            .bearer_auth(&self.token)
            .timeout(Duration::from_secs(3))
            .send()
            .await?;
        if !resp.status().is_success() {
            bail!("health check returned {}", resp.status());
        }
        Ok(())
    }

    pub async fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T> {
        let resp = self
            .http
            .get(format!("{}{path}", self.base))
            .bearer_auth(&self.token)
            .send()
            .await
            .with_context(|| format!("GET {path}"))?;
        decode(resp).await
    }

    pub async fn post<T: DeserializeOwned>(&self, path: &str, body: &impl Serialize) -> Result<T> {
        let resp = self
            .http
            .post(format!("{}{path}", self.base))
            .bearer_auth(&self.token)
            .json(body)
            .send()
            .await
            .with_context(|| format!("POST {path}"))?;
        decode(resp).await
    }
}

async fn decode<T: DeserializeOwned>(resp: reqwest::Response) -> Result<T> {
    let status = resp.status();
    let bytes = resp.bytes().await.context("read response")?;
    if !status.is_success() {
        let msg = serde_json::from_slice::<Value>(&bytes)
            .ok()
            .and_then(|v| v.get("error").and_then(Value::as_str).map(str::to_string))
            .unwrap_or_else(|| String::from_utf8_lossy(&bytes).into_owned());
        return Err(anyhow!("{msg}"));
    }
    serde_json::from_slice(&bytes).context("decode response")
}

/// Detached `spoor serve`: survives this process, no console window.
fn spawn_background_serve() -> Result<()> {
    let exe = std::env::current_exe().context("locate spoor executable")?;
    let mut cmd = std::process::Command::new(exe);
    cmd.arg("serve")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    cmd.spawn()
        .context("start `spoor serve` in the background")?;
    crate::log::info("started `spoor serve` in the background");
    Ok(())
}
