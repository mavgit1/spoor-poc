use crate::classify;
use crate::discover;
use crate::ir;
use crate::types::AppState;

pub async fn run_discover(state: &AppState) -> anyhow::Result<()> {
    let flows = state.flows.read().await.clone();
    let flows_capped = state.flows_capped.load(std::sync::atomic::Ordering::SeqCst);
    let entries = ir::entries_from_flows(&flows);
    let classified = classify::classify_entries(entries).await;
    let candidates = discover::discover_candidates(&classified);
    let coverage = classify::CoverageReport::from_session(&flows, &classified, flows_capped);

    let top: Vec<_> = candidates
        .iter()
        .take(5)
        .map(|c| format!("{} ({}×)", c.label, c.request_count))
        .collect();
    if !top.is_empty() {
        crate::log::info(&format!(
            "discovered {} candidates — top: {}",
            candidates.len(),
            top.join(", ")
        ));
    }
    if coverage.is_partial() {
        crate::log::info(&format!(
            "coverage: {} undecoded binary, {} ws frames, {} grpc/protobuf, capped={}",
            coverage.undecoded_binary,
            coverage.websocket_frames,
            coverage.grpc_or_protobuf,
            coverage.flows_capped
        ));
    }

    *state.classified.write().await = classified;
    *state.candidates.write().await = candidates;
    *state.coverage.write().await = coverage;
    *state.export_bundle.write().await = None;

    Ok(())
}
