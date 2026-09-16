use crate::capture::CaptureRecord;
use crate::classify;
use crate::classify::{ClassifiedEntry, CoverageReport};
use crate::discover;
use crate::ir;
use crate::types::AppState;
use crate::types::Candidate;

pub struct AnalysisResult {
    pub classified: Vec<ClassifiedEntry>,
    pub candidates: Vec<Candidate>,
    pub coverage: CoverageReport,
}

/// Classify + discover over an in-memory session. Pure with respect to
/// [`AppState`]: callers wire the result wherever they need it.
///
/// LLM classification runs only when `OPENROUTER_API_KEY` is set; otherwise
/// ambiguous traffic is treated as noise. Call [`disable_llm`] first for a
/// deterministic replay.
pub async fn analyze(flows: &[CaptureRecord], flows_capped: bool) -> AnalysisResult {
    let entries = ir::entries_from_flows(flows);
    let classified = classify::classify_entries(entries).await;
    let candidates = discover::discover_candidates(&classified);
    let coverage = classify::CoverageReport::from_session(flows, &classified, flows_capped);
    log_analysis(&candidates, &coverage);
    AnalysisResult {
        classified,
        candidates,
        coverage,
    }
}

pub async fn run_discover(state: &AppState) -> anyhow::Result<()> {
    let flows = state.flows.read().await.clone();
    let flows_capped = state.flows_capped.load(std::sync::atomic::Ordering::SeqCst);
    let result = analyze(&flows, flows_capped).await;

    *state.classified.write().await = result.classified;
    *state.candidates.write().await = result.candidates;
    *state.coverage.write().await = result.coverage;
    *state.export_bundle.write().await = None;

    Ok(())
}

fn log_analysis(candidates: &[Candidate], coverage: &CoverageReport) {
    let top: Vec<_> = candidates
        .iter()
        .take(5)
        .map(|c| format!("{} ({}×)", c.label, c.request_count))
        .collect();
    if !top.is_empty() {
        crate::log::info(format!(
            "discovered {} candidates — top: {}",
            candidates.len(),
            top.join(", ")
        ));
    }
    if coverage.is_partial() {
        crate::log::info(format!(
            "coverage: {} undecoded binary, {} ws frames, {} grpc/protobuf, capped={}",
            coverage.undecoded_binary,
            coverage.websocket_frames,
            coverage.grpc_or_protobuf,
            coverage.flows_capped
        ));
    }
}

/// Force the LLM classify step off for this process. `classify::llm` reads
/// `OPENROUTER_API_KEY` at call time; clearing it is the supported way to get
/// a reproducible replay without touching the classify crate.
pub fn disable_llm() {
    // SAFETY: replay/tests call this before `analyze`. The LLM client reads
    // the variable on each batch, not at process start. This process is the
    // only intended consumer.
    unsafe {
        std::env::remove_var("OPENROUTER_API_KEY");
    }
}
