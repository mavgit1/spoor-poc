//! One live recording: capture on a site's browser plus a snapshot loop that
//! appends new flows to the session on disk.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Result, anyhow};
use chromiumoxide::Browser;
use tokio::sync::RwLock;
use tokio::task::JoinHandle;

use crate::capture::{self, CaptureRecord};
use crate::log;

use super::store::{BrowsingPage, SessionMeta, SessionStore, SessionWriter};

/// How often to snapshot while recording. Worst-case loss on crash = this window.
pub const SNAPSHOT_INTERVAL: Duration = Duration::from_secs(2);

struct Buffers {
    flows: Arc<RwLock<Vec<CaptureRecord>>>,
    page_urls: Arc<RwLock<Vec<BrowsingPage>>>,
    flows_capped: Arc<AtomicBool>,
    writer: Arc<Mutex<Option<SessionWriter>>>,
}

impl Buffers {
    /// Append everything captured since the last flush and refresh meta.
    async fn flush(&self) -> Result<()> {
        let flows = self.flows.read().await;
        let pages = self.page_urls.read().await.clone();
        let mut guard = self.writer.lock().map_err(|_| anyhow!("writer poisoned"))?;
        let Some(writer) = guard.as_mut() else {
            return Ok(());
        };
        let start = writer.written().min(flows.len());
        if start < flows.len() {
            writer.append(&flows[start..])?;
        }
        writer.update_snapshot(pages, self.flows_capped.load(Ordering::SeqCst))
    }
}

pub struct Recording {
    pub session_id: String,
    pub dir: PathBuf,
    buffers: Arc<Buffers>,
    store: SessionStore,
    capture_task: JoinHandle<()>,
    snapshot_task: JoinHandle<()>,
}

impl Recording {
    /// Start capturing every tab of `browser` into a new session for `site`.
    pub fn start(browser: Arc<Browser>, site: &str, store: SessionStore) -> Result<Self> {
        let mut writer = store.create()?;
        writer.set_site(site)?;
        let session_id = writer.id().to_string();
        let dir = writer.dir().to_path_buf();

        let buffers = Arc::new(Buffers {
            flows: Arc::new(RwLock::new(Vec::new())),
            page_urls: Arc::new(RwLock::new(Vec::new())),
            flows_capped: Arc::new(AtomicBool::new(false)),
            writer: Arc::new(Mutex::new(Some(writer))),
        });

        let capture_task = tokio::spawn({
            let (flows, capped, pages) = (
                Arc::clone(&buffers.flows),
                Arc::clone(&buffers.flows_capped),
                Arc::clone(&buffers.page_urls),
            );
            async move {
                if let Err(e) = capture::capture(browser, flows, capped, pages).await {
                    log::error(format!("capture failed: {e:#}"));
                }
            }
        });

        let snapshot_task = tokio::spawn({
            let buffers = Arc::clone(&buffers);
            async move {
                let mut tick = tokio::time::interval(SNAPSHOT_INTERVAL);
                loop {
                    tick.tick().await;
                    if let Err(e) = buffers.flush().await {
                        log::warn(format!("recording snapshot failed: {e:#}"));
                    }
                }
            }
        });

        log::info(format!("recording {site} → {}", dir.display()));
        Ok(Self {
            session_id,
            dir,
            buffers,
            store,
            capture_task,
            snapshot_task,
        })
    }

    pub async fn flow_count(&self) -> usize {
        self.buffers.flows.read().await.len()
    }

    /// Stop capture (the browser stays open), write the tail, gzip, prune.
    /// Returns `None` when nothing was captured (the empty session is removed).
    pub async fn stop(self) -> Result<Option<SessionMeta>> {
        self.capture_task.abort();
        let _ = self.capture_task.await;
        self.snapshot_task.abort();
        let _ = self.snapshot_task.await;

        self.buffers.flush().await?;
        let writer = self
            .buffers
            .writer
            .lock()
            .map_err(|_| anyhow!("writer poisoned"))?
            .take();
        let meta = match writer {
            Some(w) => w.finalize()?,
            None => None,
        };
        if let Err(e) = self.store.prune() {
            log::warn(format!("session prune failed: {e:#}"));
        }
        Ok(meta)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture::{Body, Transport};

    fn flow(seq: u64) -> CaptureRecord {
        CaptureRecord {
            id: format!("f-{seq}"),
            transport: Transport::Http,
            url: format!("https://api.example.test/v1/{seq}"),
            method: Some("GET".into()),
            request_headers: Default::default(),
            request_body: None,
            status: Some(200),
            response_headers: None,
            response_body: Some(Body::text(format!(r#"{{"seq":{seq}}}"#))),
            resource_type: Some("XHR".into()),
            sequence: seq,
            timestamp_ms: None,
            ws_request_id: None,
            ws_opcode: None,
            direction: None,
        }
    }

    #[tokio::test]
    async fn flush_appends_only_new_flows() {
        let root = std::env::temp_dir().join(format!(
            "spoor-recorder-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let store = SessionStore::at(&root);
        let mut writer = store.create().unwrap();
        writer.set_site("demo").unwrap();
        let id = writer.id().to_string();
        let buffers = Buffers {
            flows: Arc::new(RwLock::new(vec![flow(1), flow(2)])),
            page_urls: Arc::new(RwLock::new(Vec::new())),
            flows_capped: Arc::new(AtomicBool::new(false)),
            writer: Arc::new(Mutex::new(Some(writer))),
        };
        buffers.flush().await.unwrap();
        buffers.flows.write().await.push(flow(3));
        buffers.flush().await.unwrap();
        buffers.flush().await.unwrap();

        let writer = buffers.writer.lock().unwrap().take().unwrap();
        let meta = writer.finalize().unwrap().expect("non-empty session");
        assert_eq!(meta.flow_count, 3);
        assert_eq!(meta.site.as_deref(), Some("demo"));

        let loaded = store.load(&id).unwrap();
        let seqs: Vec<u64> = loaded.flows.iter().map(|f| f.sequence).collect();
        assert_eq!(seqs, vec![1, 2, 3], "no duplicates across flushes");
        let _ = std::fs::remove_dir_all(&root);
    }
}
