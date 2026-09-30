//! On-disk session store.
//!
//! Layout (`{cache}/sessions/{id}/`):
//! - `meta.json` — started/ended, pages, counts, crate version
//! - `flows.jsonl` — one [`CaptureRecord`] per line (append-only while recording)
//! - `flows.jsonl.gz` — gzipped JSONL written on finalize; uncompressed file is removed
//!
//! ## Pruning
//! Keep the `SPOOR_SESSION_KEEP` most recent sessions (default **20**). Additionally,
//! if total size exceeds `SPOOR_SESSION_MAX_MB` (default **512**), delete oldest
//! sessions until under the cap, never deleting the newest session.

use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow, bail};
use flate2::Compression;
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use serde::{Deserialize, Serialize};

use crate::cache_dir::sessions_dir;
use crate::capture::CaptureRecord;
use crate::log;

/// Top-level page the recording browser navigated to (main frame only).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrowsingPage {
    pub url: String,
    /// Registrable domain from CDP when available (e.g. `deepl.com`).
    pub domain: String,
}

/// Default number of sessions to keep on disk.
pub const DEFAULT_KEEP: usize = 20;
/// Default total size cap across all sessions.
pub const DEFAULT_MAX_MB: u64 = 512;

const META_FILE: &str = "meta.json";
const FLOWS_FILE: &str = "flows.jsonl";
const FLOWS_GZ_FILE: &str = "flows.jsonl.gz";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionMeta {
    pub id: String,
    /// Site the recording was made on (`spoor record <site>`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub site: Option<String>,
    pub started_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<String>,
    #[serde(default)]
    pub pages: Vec<BrowsingPage>,
    #[serde(default)]
    pub flows_capped: bool,
    #[serde(default)]
    pub flow_count: usize,
    /// Crate version that wrote the session (`CARGO_PKG_VERSION`).
    #[serde(default)]
    pub spoor_version: String,
    /// True after finalize gzipped `flows.jsonl` → `flows.jsonl.gz`.
    #[serde(default)]
    pub gzipped: bool,
}

#[derive(Debug, Clone)]
pub struct SessionSummary {
    pub meta: SessionMeta,
    pub path: PathBuf,
    pub size_bytes: u64,
}

#[derive(Debug, Clone)]
pub struct LoadedSession {
    pub meta: SessionMeta,
    pub flows: Vec<CaptureRecord>,
    pub path: PathBuf,
}

#[derive(Debug, Clone)]
pub struct SessionStore {
    root: PathBuf,
}

impl SessionStore {
    pub fn default_store() -> Self {
        Self {
            root: sessions_dir(),
        }
    }

    pub fn at(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn session_dir(&self, id: &str) -> PathBuf {
        self.root.join(id)
    }

    pub fn create(&self) -> Result<SessionWriter> {
        self.create_with_id(&new_session_id())
    }

    pub fn create_with_id(&self, id: &str) -> Result<SessionWriter> {
        if !is_safe_session_id(id) {
            bail!("refusing unsafe session id {id:?}");
        }
        fs::create_dir_all(&self.root)
            .with_context(|| format!("create sessions dir {}", self.root.display()))?;
        let dir = self.session_dir(id);
        if dir.exists() {
            bail!("session {id} already exists");
        }
        fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
        let flows_path = dir.join(FLOWS_FILE);
        let file = OpenOptions::new()
            .create_new(true)
            .append(true)
            .open(&flows_path)
            .with_context(|| format!("create {}", flows_path.display()))?;
        let meta = SessionMeta {
            id: id.to_string(),
            site: None,
            started_at: utc_now_rfc3339(),
            ended_at: None,
            pages: Vec::new(),
            flows_capped: false,
            flow_count: 0,
            spoor_version: env!("CARGO_PKG_VERSION").to_string(),
            gzipped: false,
        };
        write_meta_atomic(&dir.join(META_FILE), &meta)?;
        Ok(SessionWriter {
            dir,
            meta,
            writer: BufWriter::new(file),
            written: 0,
        })
    }

    pub fn list(&self) -> Result<Vec<SessionSummary>> {
        if !self.root.exists() {
            return Ok(Vec::new());
        }
        let mut out = Vec::new();
        for entry in fs::read_dir(&self.root)
            .with_context(|| format!("read sessions dir {}", self.root.display()))?
        {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let dir = entry.path();
            let meta_path = dir.join(META_FILE);
            if !meta_path.is_file() {
                continue;
            }
            match load_meta(&meta_path) {
                Ok(meta) => {
                    let size_bytes = dir_size(&dir).unwrap_or(0);
                    out.push(SessionSummary {
                        meta,
                        path: dir,
                        size_bytes,
                    });
                }
                Err(e) => {
                    log::warn(format!(
                        "session: skipping unreadable meta {}: {e:#}",
                        meta_path.display()
                    ));
                }
            }
        }
        // Newest first: started_at is RFC3339 UTC, falling back to id (which
        // is itself a timestamp for sessions we create).
        out.sort_by(|a, b| {
            b.meta
                .started_at
                .cmp(&a.meta.started_at)
                .then_with(|| b.meta.id.cmp(&a.meta.id))
        });
        Ok(out)
    }

    pub fn load(&self, id: &str) -> Result<LoadedSession> {
        if !is_safe_session_id(id) {
            bail!("refusing unsafe session id {id:?}");
        }
        load_session_dir(&self.session_dir(id))
    }

    /// Keep the N newest sessions and enforce the total size cap.
    pub fn prune(&self) -> Result<Vec<String>> {
        prune_with(&self.list()?, keep_count(), max_bytes())
    }
}

pub struct SessionWriter {
    dir: PathBuf,
    meta: SessionMeta,
    writer: BufWriter<File>,
    written: usize,
}

impl SessionWriter {
    pub fn id(&self) -> &str {
        &self.meta.id
    }

    pub fn written(&self) -> usize {
        self.written
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Append `records` (already the slice since the last cursor). Crash-safe
    /// in the sense that a torn last line is skipped on read.
    pub fn append(&mut self, records: &[CaptureRecord]) -> Result<()> {
        if records.is_empty() {
            return Ok(());
        }
        for rec in records {
            serde_json::to_writer(&mut self.writer, rec)
                .context("serialize CaptureRecord to jsonl")?;
            self.writer.write_all(b"\n")?;
        }
        self.writer.flush().context("flush session jsonl")?;
        self.writer
            .get_ref()
            .sync_data()
            .context("fsync session jsonl")?;
        self.written += records.len();
        self.meta.flow_count = self.written;
        Ok(())
    }

    pub fn set_site(&mut self, site: &str) -> Result<()> {
        self.meta.site = Some(site.to_string());
        write_meta_atomic(&self.dir.join(META_FILE), &self.meta)
    }

    pub fn update_snapshot(&mut self, pages: Vec<BrowsingPage>, flows_capped: bool) -> Result<()> {
        self.meta.pages = pages;
        self.meta.flows_capped = flows_capped;
        self.meta.flow_count = self.written;
        write_meta_atomic(&self.dir.join(META_FILE), &self.meta)
    }

    /// Gzip `flows.jsonl`, drop the uncompressed file, stamp `ended_at`.
    /// Empty sessions are deleted instead of kept.
    pub fn finalize(mut self) -> Result<Option<SessionMeta>> {
        self.writer.flush().ok();
        let _ = self.writer.get_ref().sync_all();
        drop(self.writer);

        if self.written == 0 {
            let _ = fs::remove_dir_all(&self.dir);
            return Ok(None);
        }

        let jsonl = self.dir.join(FLOWS_FILE);
        let gz_path = self.dir.join(FLOWS_GZ_FILE);
        gzip_file(&jsonl, &gz_path)?;
        if let Err(e) = fs::remove_file(&jsonl) {
            log::warn(format!(
                "session: left uncompressed jsonl after gzip {}: {e}",
                jsonl.display()
            ));
        } else {
            self.meta.gzipped = true;
        }
        self.meta.ended_at = Some(utc_now_rfc3339());
        self.meta.flow_count = self.written;
        write_meta_atomic(&self.dir.join(META_FILE), &self.meta)?;
        Ok(Some(self.meta))
    }
}

/// Load a stored session directory, a jsonl/jsonl.gz file,
/// a JSON array of flows, or a session id under the default store.
pub fn load_source(spec: &str) -> Result<LoadedSession> {
    let path = Path::new(spec);
    if path.is_dir() {
        return load_session_dir(path);
    }
    if path.is_file() {
        return load_file(path);
    }
    let root = sessions_dir();
    SessionStore::default_store().load(spec).with_context(|| {
        format!(
            "no session {spec:?} (not a path, and not an id under {})",
            root.display()
        )
    })
}

pub fn load_session_dir(dir: &Path) -> Result<LoadedSession> {
    let meta_path = dir.join(META_FILE);
    let meta = if meta_path.is_file() {
        load_meta(&meta_path)?
    } else {
        SessionMeta {
            site: None,
            id: dir
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "unknown".into()),
            started_at: String::new(),
            ended_at: None,
            pages: Vec::new(),
            flows_capped: false,
            flow_count: 0,
            spoor_version: String::new(),
            gzipped: dir.join(FLOWS_GZ_FILE).is_file(),
        }
    };

    let jsonl = dir.join(FLOWS_FILE);
    let gz = dir.join(FLOWS_GZ_FILE);
    let flows = if jsonl.is_file() {
        read_jsonl_path(&jsonl)?
    } else if gz.is_file() {
        read_jsonl_gz_path(&gz)?
    } else {
        bail!(
            "session dir {} has neither {FLOWS_FILE} nor {FLOWS_GZ_FILE}",
            dir.display()
        );
    };

    let mut meta = meta;
    meta.flow_count = flows.len();
    Ok(LoadedSession {
        meta,
        flows,
        path: dir.to_path_buf(),
    })
}

fn load_file(path: &Path) -> Result<LoadedSession> {
    let bytes = fs::read(path).with_context(|| format!("read {}", path.display()))?;
    let name = path
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "file".into());

    if looks_like_gzip(&bytes) || path_ends_with_any(path, &[".jsonl.gz", ".json.gz", ".gz"]) {
        let raw = maybe_gunzip(&bytes)?;
        if let Ok(flows) = parse_json_array(&raw) {
            return Ok(file_session(name, flows, false));
        }
        let flows = parse_jsonl_bytes(&raw)
            .with_context(|| format!("not jsonl.gz or a JSON array: {}", path.display()))?;
        return Ok(file_session(name, flows, false));
    }

    if path_ends_with_any(path, &[".jsonl"]) {
        let flows = parse_jsonl_bytes(&bytes)?;
        return Ok(file_session(name, flows, false));
    }

    if let Ok(flows) = parse_json_array(&bytes) {
        return Ok(file_session(name, flows, false));
    }

    let flows = parse_jsonl_bytes(&bytes).with_context(|| {
        format!(
            "unrecognised capture file {} (tried session jsonl, JSON array)",
            path.display()
        )
    })?;
    Ok(file_session(name, flows, false))
}

fn file_session(name: String, flows: Vec<CaptureRecord>, flows_capped: bool) -> LoadedSession {
    let n = flows.len();
    LoadedSession {
        meta: SessionMeta {
            id: name,
            site: None,
            started_at: String::new(),
            ended_at: None,
            pages: Vec::new(),
            flows_capped,
            flow_count: n,
            spoor_version: String::new(),
            gzipped: false,
        },
        flows,
        path: PathBuf::new(),
    }
}

fn read_jsonl_path(path: &Path) -> Result<Vec<CaptureRecord>> {
    let file = File::open(path).with_context(|| format!("open {}", path.display()))?;
    parse_jsonl_reader(BufReader::new(file), &path.display().to_string())
}

fn read_jsonl_gz_path(path: &Path) -> Result<Vec<CaptureRecord>> {
    let file = File::open(path).with_context(|| format!("open {}", path.display()))?;
    let decoder = GzDecoder::new(file);
    parse_jsonl_reader(BufReader::new(decoder), &path.display().to_string())
}

fn parse_jsonl_bytes(bytes: &[u8]) -> Result<Vec<CaptureRecord>> {
    parse_jsonl_reader(BufReader::new(bytes), "buffer")
}

fn parse_jsonl_reader<R: BufRead>(reader: R, origin: &str) -> Result<Vec<CaptureRecord>> {
    let mut flows = Vec::new();
    for (i, line) in reader.lines().enumerate() {
        let line = line.with_context(|| format!("read {origin} line {}", i + 1))?;
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<CaptureRecord>(&line) {
            Ok(rec) => flows.push(rec),
            Err(e) => {
                log::warn(format!(
                    "session: skipping malformed jsonl line {} in {origin}: {e}",
                    i + 1
                ));
            }
        }
    }
    Ok(flows)
}

fn parse_json_array(bytes: &[u8]) -> Result<Vec<CaptureRecord>> {
    serde_json::from_slice(bytes).map_err(|e| anyhow!(e))
}

fn looks_like_gzip(bytes: &[u8]) -> bool {
    bytes.starts_with(&[0x1f, 0x8b])
}

fn maybe_gunzip(bytes: &[u8]) -> Result<Vec<u8>> {
    if looks_like_gzip(bytes) {
        let mut raw = Vec::new();
        GzDecoder::new(bytes).read_to_end(&mut raw)?;
        Ok(raw)
    } else {
        Ok(bytes.to_vec())
    }
}

fn path_ends_with_any(path: &Path, suffixes: &[&str]) -> bool {
    let s = path.as_os_str().to_string_lossy();
    suffixes.iter().any(|suf| s.ends_with(suf))
}

fn load_meta(path: &Path) -> Result<SessionMeta> {
    let bytes = fs::read(path).with_context(|| format!("read {}", path.display()))?;
    serde_json::from_slice(&bytes).with_context(|| format!("parse {}", path.display()))
}

fn write_meta_atomic(path: &Path, meta: &SessionMeta) -> Result<()> {
    let tmp = path.with_extension("json.tmp");
    let data = serde_json::to_vec_pretty(meta)?;
    {
        let mut f = File::create(&tmp).with_context(|| format!("create {}", tmp.display()))?;
        f.write_all(&data)?;
        f.sync_all()?;
    }
    fs::rename(&tmp, path).with_context(|| format!("rename {} → {}", tmp.display(), path.display()))
}

fn gzip_file(src: &Path, dst: &Path) -> Result<()> {
    let input = fs::read(src).with_context(|| format!("read {}", src.display()))?;
    let tmp = dst.with_extension("gz.tmp");
    {
        let file = File::create(&tmp).with_context(|| format!("create {}", tmp.display()))?;
        let mut enc = GzEncoder::new(file, Compression::default());
        enc.write_all(&input)?;
        let file = enc.finish()?;
        file.sync_all()?;
    }
    fs::rename(&tmp, dst).with_context(|| format!("rename {} → {}", tmp.display(), dst.display()))
}

pub(crate) fn dir_size(dir: &Path) -> Result<u64> {
    let mut total = 0u64;
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let meta = entry.metadata()?;
        if meta.is_file() {
            total += meta.len();
        } else if meta.is_dir() {
            total += dir_size(&entry.path())?;
        }
    }
    Ok(total)
}

pub fn keep_count() -> usize {
    std::env::var("SPOOR_SESSION_KEEP")
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|&n| n > 0)
        .unwrap_or(DEFAULT_KEEP)
}

pub fn max_bytes() -> u64 {
    let mb = std::env::var("SPOOR_SESSION_MAX_MB")
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|&n: &u64| n > 0)
        .unwrap_or(DEFAULT_MAX_MB);
    mb.saturating_mul(1024 * 1024)
}

fn prune_with(sessions: &[SessionSummary], keep: usize, max_bytes: u64) -> Result<Vec<String>> {
    let mut deleted = Vec::new();
    // `sessions` is newest-first.
    for old in sessions.iter().skip(keep) {
        if let Err(e) = fs::remove_dir_all(&old.path) {
            log::warn(format!(
                "session: failed to prune {}: {e}",
                old.path.display()
            ));
        } else {
            deleted.push(old.meta.id.clone());
        }
    }

    let remaining: Vec<&SessionSummary> = sessions
        .iter()
        .filter(|s| !deleted.iter().any(|id| id == &s.meta.id))
        .collect();
    let mut total: u64 = remaining.iter().map(|s| s.size_bytes).sum();
    // Delete oldest (end of newest-first list) while over cap; keep at least one.
    for old in remaining.iter().rev().skip(1) {
        if total <= max_bytes {
            break;
        }
        if deleted.iter().any(|id| id == &old.meta.id) {
            continue;
        }
        match fs::remove_dir_all(&old.path) {
            Ok(()) => {
                total = total.saturating_sub(old.size_bytes);
                deleted.push(old.meta.id.clone());
            }
            Err(e) => log::warn(format!(
                "session: failed to prune {}: {e}",
                old.path.display()
            )),
        }
    }
    Ok(deleted)
}

pub fn is_safe_session_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() < 200
        && !id.contains('/')
        && !id.contains('\\')
        && !id.contains("..")
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | 'T' | 'Z'))
}

pub fn new_session_id() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let (y, m, d, hh, mm, ss) = unix_secs_to_utc(secs);
    format!(
        "{y:04}-{m:02}-{d:02}T{hh:02}-{mm:02}-{ss:02}Z-{}",
        random_suffix()
    )
}

pub fn utc_now_rfc3339() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let (y, m, d, hh, mm, ss) = unix_secs_to_utc(secs);
    format!("{y:04}-{m:02}-{d:02}T{hh:02}:{mm:02}:{ss:02}Z")
}

/// Howard Hinnant civil_from_days — UTC calendar from Unix seconds.
pub fn unix_secs_to_utc(secs: u64) -> (i32, u8, u8, u8, u8, u8) {
    let z = secs as i64 / 86400 + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u32;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u8;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    let rem = (secs % 86400) as u32;
    (
        y as i32,
        m as u8,
        d,
        (rem / 3600) as u8,
        ((rem % 3600) / 60) as u8,
        (rem % 60) as u8,
    )
}

fn random_suffix() -> String {
    let mut buf = [0u8; 3];
    fill_random(&mut buf);
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

fn fill_random(buf: &mut [u8]) {
    #[cfg(unix)]
    {
        use std::io::Read;
        if let Ok(mut f) = File::open("/dev/urandom")
            && f.read_exact(buf).is_ok()
        {
            return;
        }
    }
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    std::process::id().hash(&mut hasher);
    SystemTime::now().hash(&mut hasher);
    let n = hasher.finish();
    let bytes = n.to_le_bytes();
    let n = buf.len().min(bytes.len());
    buf[..n].copy_from_slice(&bytes[..n]);
}

pub fn format_bytes(n: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = 1024.0 * 1024.0;
    if n as f64 >= MB {
        format!("{:.1} MB", n as f64 / MB)
    } else if n as f64 >= KB {
        format!("{:.1} KB", n as f64 / KB)
    } else {
        format!("{n} B")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture::{Body, CaptureRecord, Transport};
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn sample_flow(id: &str, seq: u64) -> CaptureRecord {
        CaptureRecord {
            id: id.into(),
            transport: Transport::Http,
            url: format!("https://api.example.test/v1/{id}"),
            method: Some("GET".into()),
            request_headers: HashMap::new(),
            request_body: None,
            status: Some(200),
            response_headers: Some(HashMap::from([(
                "content-type".into(),
                "application/json".into(),
            )])),
            response_body: Some(Body::text(r#"{"ok":true}"#)),
            resource_type: Some("Fetch".into()),
            sequence: seq,
            timestamp_ms: None,
            ws_request_id: None,
            ws_opcode: None,
            direction: None,
        }
    }

    fn temp_store() -> (PathBuf, SessionStore) {
        static N: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "spoor-session-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        (dir.clone(), SessionStore::at(dir))
    }

    #[test]
    fn unix_epoch_known_instant() {
        let (y, m, d, hh, mm, ss) = unix_secs_to_utc(1_700_000_000);
        assert_eq!((y, m, d, hh, mm, ss), (2023, 11, 14, 22, 13, 20));
    }

    #[test]
    fn jsonl_roundtrip_and_gzip() {
        let (dir, store) = temp_store();
        let mut w = store.create_with_id("2026-01-01T00-00-00Z-test1").unwrap();
        w.append(&[sample_flow("a", 0), sample_flow("b", 1)])
            .unwrap();
        w.update_snapshot(vec![], false).unwrap();
        let meta = w.finalize().unwrap().unwrap();
        assert!(meta.gzipped);
        assert_eq!(meta.flow_count, 2);
        assert!(store.session_dir(&meta.id).join(FLOWS_GZ_FILE).is_file());
        assert!(!store.session_dir(&meta.id).join(FLOWS_FILE).exists());

        let loaded = store.load(&meta.id).unwrap();
        assert_eq!(loaded.flows.len(), 2);
        assert_eq!(loaded.flows[0].id, "a");
        assert_eq!(loaded.flows[1].url, "https://api.example.test/v1/b");
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn truncated_last_line_is_skipped() {
        let (dir, store) = temp_store();
        let id = "2026-01-01T00-00-00Z-trunc";
        let mut w = store.create_with_id(id).unwrap();
        w.append(&[sample_flow("ok", 0)]).unwrap();
        drop(w);

        let jsonl = store.session_dir(id).join(FLOWS_FILE);
        let mut f = OpenOptions::new().append(true).open(&jsonl).unwrap();
        f.write_all(b"{\"id\":\"torn\",\"url\":\"https://x")
            .unwrap();
        drop(f);

        let loaded = load_session_dir(&store.session_dir(id)).unwrap();
        assert_eq!(loaded.flows.len(), 1);
        assert_eq!(loaded.flows[0].id, "ok");
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn prune_keeps_newest_n() {
        let (dir, store) = temp_store();
        for i in 0..5 {
            let id = format!("2026-01-0{}T00-00-00Z-p", i + 1);
            let mut w = store.create_with_id(&id).unwrap();
            w.append(&[sample_flow(&format!("f{i}"), i as u64)])
                .unwrap();
            w.finalize().unwrap();
        }
        let listed = store.list().unwrap();
        let deleted = prune_with(&listed, 2, u64::MAX).unwrap();
        assert_eq!(deleted.len(), 3);
        let left: Vec<_> = store
            .list()
            .unwrap()
            .into_iter()
            .map(|s| s.meta.id)
            .collect();
        assert_eq!(left.len(), 2);
        assert!(left[0].contains("2026-01-05"));
        assert!(left[1].contains("2026-01-04"));
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn load_json_array_fixture() {
        let path = Path::new("tests/fixtures/auth_bearer.json");
        if !path.exists() {
            return;
        }
        let loaded = load_file(path).unwrap();
        assert_eq!(loaded.flows.len(), 3);
        assert_eq!(loaded.flows[0].id, "oauth-1");
    }

    #[test]
    fn empty_session_is_discarded() {
        let (dir, store) = temp_store();
        let w = store.create_with_id("2026-01-01T00-00-00Z-empty").unwrap();
        assert!(w.finalize().unwrap().is_none());
        assert!(!store.session_dir("2026-01-01T00-00-00Z-empty").exists());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn session_id_is_filesystem_safe() {
        let id = new_session_id();
        assert!(is_safe_session_id(&id));
        assert!(!id.contains(':'));
        assert!(id.contains('T'));
        assert!(id.contains('Z'));
    }
}
