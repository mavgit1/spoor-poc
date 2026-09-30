//! The two safeguards Spoor enforces itself, because they need no knowledge of
//! the site: pacing (pure timing) and an audit log (records, never judges).
//! Whether a request is a read or a write is the scripts' business.

use std::io::Write;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::log;
use crate::session::utc_now_rfc3339;

/// Minimum gap between exec requests on one site, shared by every exec on it.
#[derive(Default)]
pub struct Pacer {
    last: tokio::sync::Mutex<Option<Instant>>,
}

impl Pacer {
    /// Wait until `gap` has passed since the previous request, then claim the slot.
    /// The lock is held while sleeping, so concurrent execs queue in order.
    pub async fn wait(&self, gap: Duration) {
        let mut last = self.last.lock().await;
        if let Some(prev) = *last {
            let elapsed = prev.elapsed();
            if elapsed < gap {
                tokio::time::sleep(gap - elapsed).await;
            }
        }
        *last = Some(Instant::now());
    }
}

/// Append-only JSONL: `{cache}/audit/{site}.jsonl`.
pub struct Audit {
    path: PathBuf,
    lock: std::sync::Mutex<()>,
}

impl Audit {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            lock: std::sync::Mutex::new(()),
        }
    }

    /// Best effort: an audit write failure is logged, never fatal to the exec.
    pub fn write(&self, exec_id: &str, event: &str, fields: Value) {
        let mut entry = json!({ "ts": utc_now_rfc3339(), "exec": exec_id, "event": event });
        if let (Some(obj), Value::Object(extra)) = (entry.as_object_mut(), fields) {
            obj.extend(extra);
        }
        let _guard = self.lock.lock();
        let result = (|| -> std::io::Result<()> {
            if let Some(parent) = self.path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let mut file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.path)?;
            writeln!(file, "{entry}")
        })();
        if let Err(e) = result {
            log::warn(format!(
                "audit write to {} failed: {e}",
                self.path.display()
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn pacer_spaces_requests() {
        let pacer = Pacer::default();
        let gap = Duration::from_millis(60);
        let start = Instant::now();
        for _ in 0..4 {
            pacer.wait(gap).await;
        }
        // First slot is free; the other three each wait a full gap.
        assert!(start.elapsed() >= gap * 3, "{:?}", start.elapsed());
    }

    #[test]
    fn audit_appends_json_lines() {
        let path = std::env::temp_dir()
            .join(format!("spoor-audit-{}", std::process::id()))
            .join("site.jsonl");
        let audit = Audit::new(path.clone());
        audit.write(
            "e1",
            "request",
            json!({ "method": "GET", "url": "https://x.test/" }),
        );
        audit.write("e1", "done", json!({ "ok": true }));
        let text = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<Value> = text
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0]["event"], "request");
        assert_eq!(lines[0]["method"], "GET");
        assert_eq!(lines[1]["exec"], "e1");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }
}
