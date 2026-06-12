use std::path::PathBuf;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use chromiumoxide::Browser;
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, RwLock};
use tokio::task::JoinHandle;

use crate::classify::ClassifiedEntry;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapturedFlow {
    pub id: String,
    pub url: String,
    pub method: String,
    pub request_headers: std::collections::HashMap<String, String>,
    pub request_body: Option<String>,
    pub status: Option<u16>,
    pub response_headers: Option<std::collections::HashMap<String, String>>,
    pub response_body: Option<String>,
    pub resource_type: Option<String>,
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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GenerateSelection {
    pub id: String,
    #[serde(default)]
    pub pattern: Option<String>,
}

fn default_redact() -> bool {
    true
}

#[derive(Debug, Clone, Deserialize)]
pub struct GenerateRequest {
    #[serde(default)]
    pub origin: Option<String>,
    pub selected: Vec<GenerateSelection>,
    #[serde(default)]
    pub ignore_patterns: Vec<String>,
    /// Redact session tokens / JWTs in brief examples (default true).
    #[serde(default = "default_redact")]
    pub redact: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct IgnoreRequest {
    pub pattern: String,
}

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
    pub flows: Arc<RwLock<Vec<CapturedFlow>>>,
    pub recording: Arc<AtomicBool>,
    pub session: Arc<Mutex<Option<BrowserSession>>>,
    pub analyzing: Arc<AtomicBool>,
    pub chromium_executable: Arc<PathBuf>,
    pub classified: Arc<RwLock<Vec<ClassifiedEntry>>>,
    pub candidates: Arc<RwLock<Vec<Candidate>>>,
    pub export_bundle: Arc<RwLock<Option<ExportBundle>>>,
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
