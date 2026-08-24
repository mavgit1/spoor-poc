//! Background snapshot of `AppState.flows` — no coupling to capture event loops.
//!
//! Cursor is the count already written. Capture only appends (or the vec is
//! cleared on a new recording), so the index is valid. Worst case on a crash:
//! the last snapshot interval (~2s) of flows is missing from disk.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock, Weak};
use std::time::{Duration, Instant};

use tokio::sync::{Notify, RwLock};

use crate::capture::CaptureRecord;
use crate::log;
use crate::types::BrowsingPage;

use super::store::{SessionStore, SessionWriter, format_bytes};

/// How often to snapshot while recording. Worst-case loss on crash = this window.
pub const SNAPSHOT_INTERVAL: Duration = Duration::from_secs(2);
/// Wait for `start_recording` to finish clearing in-memory state before opening
/// a new session directory. Browser launch is much slower than this.
const START_GRACE: Duration = Duration::from_millis(150);
/// After Stop, capture may still emit a few records until the browser closes.
const STOP_QUIET: Duration = Duration::from_millis(400);
const STOP_DRAIN_CAP: Duration = Duration::from_secs(3);

#[derive(Clone)]
struct PersistTargets {
    flows: Weak<RwLock<Vec<CaptureRecord>>>,
    page_urls: Weak<RwLock<Vec<BrowsingPage>>>,
    flows_capped: Weak<AtomicBool>,
    recording: Weak<AtomicBool>,
}

struct PersistInner {
    notify: Notify,
    targets: OnceLock<PersistTargets>,
    spawned: AtomicBool,
}

/// Shared handle stored on [`crate::types::AppState`]. Cheap to clone.
#[derive(Clone)]
pub struct PersistHandle {
    inner: Arc<PersistInner>,
}

impl PersistHandle {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(PersistInner {
                notify: Notify::new(),
                targets: OnceLock::new(),
                spawned: AtomicBool::new(false),
            }),
        }
    }

    pub fn arm(
        &self,
        flows: Arc<RwLock<Vec<CaptureRecord>>>,
        page_urls: Arc<RwLock<Vec<BrowsingPage>>>,
        flows_capped: Arc<AtomicBool>,
        recording: Arc<AtomicBool>,
    ) {
        let _ = self.inner.targets.set(PersistTargets {
            flows: Arc::downgrade(&flows),
            page_urls: Arc::downgrade(&page_urls),
            flows_capped: Arc::downgrade(&flows_capped),
            recording: Arc::downgrade(&recording),
        });
    }

    /// Kick the persister. Called from [`crate::types::AppState::set_recording`].
    pub fn notify_recording_changed(&self) {
        self.ensure_spawned();
        // notify_one stores a permit if the loop is not yet waiting, so a
        // start/stop that races the spawn is not lost.
        self.inner.notify.notify_one();
    }

    fn ensure_spawned(&self) {
        if self.inner.spawned.swap(true, Ordering::SeqCst) {
            return;
        }
        let Some(targets) = self.inner.targets.get().cloned() else {
            self.inner.spawned.store(false, Ordering::SeqCst);
            return;
        };
        let notify = Arc::clone(&self.inner);
        match tokio::runtime::Handle::try_current() {
            Ok(handle) => {
                handle.spawn(async move {
                    persist_loop(targets, notify, SessionStore::default_store()).await;
                });
            }
            Err(_) => {
                self.inner.spawned.store(false, Ordering::SeqCst);
                log::warn("session persist: no tokio runtime — recorded traffic will not be saved");
            }
        }
    }
}

impl Default for PersistHandle {
    fn default() -> Self {
        Self::new()
    }
}

async fn persist_loop(targets: PersistTargets, inner: Arc<PersistInner>, store: SessionStore) {
    let mut interval = tokio::time::interval(SNAPSHOT_INTERVAL);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    let mut writer: Option<SessionWriter> = None;
    let mut written: usize = 0;

    loop {
        let Some(recording) = targets.recording.upgrade() else {
            finalize_quiet(writer.take(), &store);
            return;
        };
        let is_recording = recording.load(Ordering::SeqCst);
        drop(recording);

        if is_recording {
            if writer.is_none() {
                tokio::select! {
                    _ = tokio::time::sleep(START_GRACE) => {}
                    _ = inner.notify.notified() => {}
                }
                let still = targets
                    .recording
                    .upgrade()
                    .map(|r| r.load(Ordering::SeqCst))
                    .unwrap_or(false);
                if still {
                    match store.create() {
                        Ok(w) => {
                            log::info(format!("session {} — persisting captured flows", w.id()));
                            written = 0;
                            writer = Some(w);
                        }
                        Err(e) => {
                            log::warn(format!("session persist: failed to create session: {e:#}"));
                        }
                    }
                }
            }
            snapshot_once(&targets, writer.as_mut(), &mut written);
        } else if writer.is_some() {
            drain_and_finalize(&targets, &mut writer, &mut written, &store).await;
        }

        tokio::select! {
            _ = interval.tick() => {}
            _ = inner.notify.notified() => {}
        }
    }
}

async fn drain_and_finalize(
    targets: &PersistTargets,
    writer: &mut Option<SessionWriter>,
    written: &mut usize,
    store: &SessionStore,
) {
    let deadline = Instant::now() + STOP_DRAIN_CAP;
    let mut prev: Option<usize> = None;
    loop {
        snapshot_once(targets, writer.as_mut(), written);
        let still_recording = targets
            .recording
            .upgrade()
            .map(|r| r.load(Ordering::SeqCst))
            .unwrap_or(false);
        if still_recording {
            // New recording started before drain finished — close this session first.
            finalize_quiet(writer.take(), store);
            *written = 0;
            return;
        }
        let len = current_len(targets);
        let timed_out = Instant::now() >= deadline;
        if prev == Some(len) || timed_out {
            if timed_out && prev != Some(len) {
                tokio::time::sleep(STOP_QUIET).await;
                snapshot_once(targets, writer.as_mut(), written);
            }
            finalize_quiet(writer.take(), store);
            *written = 0;
            return;
        }
        prev = Some(len);
        tokio::time::sleep(STOP_QUIET).await;
    }
}

fn current_len(targets: &PersistTargets) -> usize {
    let Some(flows) = targets.flows.upgrade() else {
        return 0;
    };
    flows.try_read().map(|g| g.len()).unwrap_or(0)
}

fn snapshot_once(
    targets: &PersistTargets,
    writer: Option<&mut SessionWriter>,
    written: &mut usize,
) {
    let Some(writer) = writer else {
        return;
    };
    let Some(flows) = targets.flows.upgrade() else {
        return;
    };
    let Ok(guard) = flows.try_read() else {
        return;
    };
    let len = guard.len();
    if len < *written {
        // Vec was cleared for a new recording. The writer belongs to the new
        // session (created after the start grace); restart the cursor.
        *written = 0;
    }
    let new = if len > *written {
        guard[*written..].to_vec()
    } else {
        Vec::new()
    };
    drop(guard);

    if !new.is_empty()
        && let Err(e) = writer.append(&new)
    {
        log::warn(format!("session persist: append failed: {e:#}"));
        return;
    }
    *written = writer.written();

    let pages = targets
        .page_urls
        .upgrade()
        .and_then(|p| p.try_read().ok().map(|g| g.clone()))
        .unwrap_or_default();
    let capped = targets
        .flows_capped
        .upgrade()
        .map(|c| c.load(Ordering::SeqCst))
        .unwrap_or(false);
    if let Err(e) = writer.update_snapshot(pages, capped) {
        log::warn(format!("session persist: meta update failed: {e:#}"));
    }
}

fn finalize_quiet(writer: Option<SessionWriter>, store: &SessionStore) {
    let Some(writer) = writer else {
        return;
    };
    let id = writer.id().to_string();
    match writer.finalize() {
        Ok(Some(meta)) => {
            let dir = store.session_dir(&meta.id);
            let size = super::store::dir_size(&dir).unwrap_or(0);
            log::info(format!(
                "session {id} saved ({} flows, {})",
                meta.flow_count,
                format_bytes(size)
            ));
        }
        Ok(None) => {
            log::debug(format!("session {id} discarded (no flows)"));
        }
        Err(e) => {
            log::warn(format!("session persist: finalize {id} failed: {e:#}"));
        }
    }
    if let Err(e) = store.prune() {
        log::warn(format!("session persist: prune failed: {e:#}"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture::{Body, CaptureRecord, Transport};
    use crate::session::store::SessionStore;
    use std::collections::HashMap;
    use std::sync::atomic::AtomicU64;

    fn sample_flow(seq: u64) -> CaptureRecord {
        CaptureRecord {
            id: format!("f{seq}"),
            transport: Transport::Http,
            url: format!("https://api.example.test/{seq}"),
            method: Some("GET".into()),
            request_headers: HashMap::new(),
            request_body: None,
            status: Some(200),
            response_headers: None,
            response_body: Some(Body::text("{}")),
            resource_type: Some("XHR".into()),
            sequence: seq,
            timestamp_ms: None,
            ws_request_id: None,
            ws_opcode: None,
            direction: None,
        }
    }

    #[tokio::test]
    async fn snapshots_from_cursor_and_finalizes() {
        static N: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "spoor-persist-{}-{}",
            std::process::id(),
            N.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let store = SessionStore::at(&dir);

        let flows = Arc::new(RwLock::new(vec![sample_flow(0), sample_flow(1)]));
        let pages = Arc::new(RwLock::new(Vec::new()));
        let capped = Arc::new(AtomicBool::new(false));
        let recording = Arc::new(AtomicBool::new(true));

        let mut writer = store.create().unwrap();
        let mut written = 0usize;
        let targets = PersistTargets {
            flows: Arc::downgrade(&flows),
            page_urls: Arc::downgrade(&pages),
            flows_capped: Arc::downgrade(&capped),
            recording: Arc::downgrade(&recording),
        };

        snapshot_once(&targets, Some(&mut writer), &mut written);
        assert_eq!(written, 2);

        flows.write().await.push(sample_flow(2));
        snapshot_once(&targets, Some(&mut writer), &mut written);
        assert_eq!(written, 3);

        let meta = writer.finalize().unwrap().unwrap();
        let loaded = store.load(&meta.id).unwrap();
        assert_eq!(loaded.flows.len(), 3);
        assert_eq!(loaded.flows[2].sequence, 2);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
