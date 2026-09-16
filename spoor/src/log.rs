//! User-visible logging.
//!
//! Messages go to stderr, to a rotating file, and to an in-memory ring buffer.
//!
//! The file and the ring exist because a bundled `Spoor.app` has no terminal:
//! without them every warning and error — a browser that failed to launch, a
//! capture task that died, a session that failed to persist — is written to a
//! stream nobody can read, which makes the product undiagnosable in the field.
//!
//! Lines are written unbuffered. The whole point is surviving a crash, and a
//! buffered writer would discard exactly the lines that explain one. Volume is
//! low unless `--verbose` is set.

use std::collections::VecDeque;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

static VERBOSE: AtomicBool = AtomicBool::new(false);

/// Rotate at this size, keeping one previous generation.
const MAX_LOG_BYTES: u64 = 4 * 1024 * 1024;

/// Lines kept in memory for the app to display without touching disk.
const RING_CAPACITY: usize = 500;

struct Sink {
    file: Option<File>,
    written: u64,
    ring: VecDeque<String>,
}

fn sink() -> &'static Mutex<Sink> {
    static SINK: OnceLock<Mutex<Sink>> = OnceLock::new();
    SINK.get_or_init(|| {
        Mutex::new(Sink {
            file: None,
            written: 0,
            ring: VecDeque::with_capacity(RING_CAPACITY),
        })
    })
}

pub fn log_dir() -> PathBuf {
    crate::cache_dir::spoor_cache_dir().join("logs")
}

pub fn log_file_path() -> PathBuf {
    log_dir().join("spoor.log")
}

fn rotated_path() -> PathBuf {
    log_dir().join("spoor.log.1")
}

pub fn init(verbose: bool) {
    VERBOSE.store(verbose, Ordering::SeqCst);
    open_log_file();
    if verbose {
        info("verbose logging enabled");
    }
}

fn open_log_file() {
    let path = log_file_path();
    if std::fs::create_dir_all(log_dir()).is_err() {
        return;
    }
    let existing = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
    if let Ok(file) = OpenOptions::new().create(true).append(true).open(&path) {
        let mut guard = match sink().lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        guard.file = Some(file);
        guard.written = existing;
    }
}

/// Most recent lines, oldest first. Always available even if the file could not
/// be opened, so the app can still show something useful.
pub fn recent_lines(limit: usize) -> Vec<String> {
    let guard = match sink().lock() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    };
    let skip = guard.ring.len().saturating_sub(limit);
    guard.ring.iter().skip(skip).cloned().collect()
}

fn record(level: &str, msg: &str) {
    // Readable UTC, because a user may be asked to send this file.
    let line = format!("{} [{level}] {msg}", crate::session::utc_now_rfc3339());

    let mut guard = match sink().lock() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    };

    if guard.ring.len() == RING_CAPACITY {
        guard.ring.pop_front();
    }
    guard.ring.push_back(line.clone());

    // Logging must never break the caller: a full disk or a read-only cache
    // directory degrades to stderr-and-ring only.
    if guard.written >= MAX_LOG_BYTES {
        guard.file = None;
        let _ = std::fs::rename(log_file_path(), rotated_path());
        if let Ok(file) = OpenOptions::new()
            .create(true)
            .append(true)
            .open(log_file_path())
        {
            guard.file = Some(file);
            guard.written = 0;
        }
    }
    if let Some(file) = guard.file.as_mut() {
        if writeln!(file, "{line}").is_ok() {
            guard.written += line.len() as u64 + 1;
        } else {
            guard.file = None;
        }
    }
}

pub fn info(msg: impl AsRef<str>) {
    let msg = msg.as_ref();
    eprintln!("[spoor] {msg}");
    record("info", msg);
}

pub fn warn(msg: impl AsRef<str>) {
    let msg = msg.as_ref();
    eprintln!("[spoor:warn] {msg}");
    record("warn", msg);
}

pub fn error(msg: impl AsRef<str>) {
    let msg = msg.as_ref();
    eprintln!("[spoor:error] {msg}");
    record("error", msg);
}

pub fn debug(msg: impl AsRef<str>) {
    if VERBOSE.load(Ordering::SeqCst) {
        let msg = msg.as_ref();
        eprintln!("[spoor:debug] {msg}");
        record("debug", msg);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ring_keeps_most_recent_lines_and_survives_no_file() {
        // No log file is opened here; the ring must still work, because that is
        // the fallback when the cache directory is not writable.
        for i in 0..(RING_CAPACITY + 25) {
            record("info", &format!("line-{i}"));
        }
        let recent = recent_lines(5);
        assert_eq!(recent.len(), 5);
        assert!(
            recent
                .last()
                .unwrap()
                .contains(&format!("line-{}", RING_CAPACITY + 24)),
            "newest line must be last: {recent:?}"
        );
        assert!(
            recent.iter().all(|l| l.contains("[info]")),
            "level must be recorded: {recent:?}"
        );

        let all = recent_lines(usize::MAX);
        assert_eq!(all.len(), RING_CAPACITY, "ring must be bounded");
    }

    #[test]
    fn debug_is_suppressed_unless_verbose() {
        VERBOSE.store(false, Ordering::SeqCst);
        let before = recent_lines(usize::MAX).len();
        debug("should-not-appear");
        let after = recent_lines(usize::MAX);
        assert!(
            !after.iter().any(|l| l.contains("should-not-appear")),
            "debug must not be recorded when quiet"
        );
        assert!(after.len() >= before.saturating_sub(1));
    }
}
