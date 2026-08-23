use std::path::PathBuf;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use chromiumoxide::Browser;
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, RwLock};
use tokio::task::JoinHandle;

use crate::capture::CaptureRecord;
use crate::classify::{ClassifiedEntry, CoverageReport};

pub use crate::capture::{Body, CapturedFlow, Direction, OmitReason, Transport};

/// Top-level page the recording browser navigated to (main frame only).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrowsingPage {
    pub url: String,
    /// Registrable domain from CDP when available (e.g. `deepl.com`).
    pub domain: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Candidate {
    pub id: String,
    pub label: String,
    pub protocol: String,
    pub guessed_pattern: String,
    pub example: String,
    pub host: String,
    pub methods: Vec<String>,
    pub confidence: String,
    pub origin: String,
    /// How many captured requests matched this candidate.
    pub request_count: usize,
    /// Panel checkbox default from saved ignore preferences (recomputed on fetch).
    /// Product law: pre-fill patterns, do not pre-select — default false.
    #[serde(default)]
    pub default_selected: bool,
    /// True when a saved ignore/host rule matches this op (shows Allow to undo).
    #[serde(default)]
    pub preference_ignored: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GenerateSelection {
    pub id: String,
    #[serde(default)]
    pub pattern: Option<String>,
}

fn default_redact() -> bool {
    // Off by default: agent packs keep observed evidence; user opts into redaction in the panel.
    false
}

#[derive(Debug, Clone, Deserialize)]
pub struct GenerateRequest {
    #[serde(default)]
    pub origin: Option<String>,
    pub selected: Vec<GenerateSelection>,
    #[serde(default)]
    pub ignore_patterns: Vec<String>,
    /// Redact session tokens / JWTs in brief examples (default false — panel opt-in).
    #[serde(default = "default_redact")]
    pub redact: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FilterPreferenceRequest {
    pub pattern: String,
    /// `ignore` (default) or `allow`
    #[serde(default = "default_filter_action")]
    pub action: String,
}

fn default_filter_action() -> String {
    "ignore".into()
}

/// Back-compat alias.
pub type IgnoreRequest = FilterPreferenceRequest;

#[derive(Debug, Clone, Default)]
pub struct ExportBundle {
    pub zip_bytes: Vec<u8>,
}

pub struct BrowserSession {
    pub browser: Browser,
    pub handler_task: JoinHandle<()>,
    pub capture_task: JoinHandle<()>,
}

#[derive(Clone)]
pub struct AppState {
    pub flows: Arc<RwLock<Vec<CaptureRecord>>>,
    pub recording: Arc<AtomicBool>,
    pub session: Arc<Mutex<Option<BrowserSession>>>,
    pub analyzing: Arc<AtomicBool>,
    pub chromium_executable: Arc<PathBuf>,
    pub classified: Arc<RwLock<Vec<ClassifiedEntry>>>,
    pub candidates: Arc<RwLock<Vec<Candidate>>>,
    pub export_bundle: Arc<RwLock<Option<ExportBundle>>>,
    pub flows_capped: Arc<AtomicBool>,
    pub coverage: Arc<RwLock<CoverageReport>>,
    pub page_urls: Arc<RwLock<Vec<BrowsingPage>>>,
}

impl AppState {
    pub fn new(chromium_executable: PathBuf) -> Self {
        Self {
            flows: Arc::new(RwLock::new(Vec::new())),
            recording: Arc::new(AtomicBool::new(false)),
            session: Arc::new(Mutex::new(None)),
            analyzing: Arc::new(AtomicBool::new(false)),
            chromium_executable: Arc::new(chromium_executable),
            classified: Arc::new(RwLock::new(Vec::new())),
            candidates: Arc::new(RwLock::new(Vec::new())),
            export_bundle: Arc::new(RwLock::new(None)),
            flows_capped: Arc::new(AtomicBool::new(false)),
            coverage: Arc::new(RwLock::new(CoverageReport::default())),
            page_urls: Arc::new(RwLock::new(Vec::new())),
        }
    }

    pub fn is_recording(&self) -> bool {
        self.recording.load(Ordering::SeqCst)
    }

    pub fn set_recording(&self, value: bool) {
        self.recording.store(value, Ordering::SeqCst);
    }

    pub fn is_analyzing(&self) -> bool {
        self.analyzing.load(Ordering::SeqCst)
    }

    pub fn set_analyzing(&self, value: bool) {
        self.analyzing.store(value, Ordering::SeqCst);
    }
}
