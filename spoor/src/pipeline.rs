use crate::classify;
use crate::discover;
use crate::ir;
use crate::types::AppState;

pub async fn run_discover(state: &AppState) -> anyhow::Result<()> {
    let flows = state.flows.read().await.clone();
    let entries = ir::entries_from_flows(&flows);
    let classified = classify::classify_entries(entries).await;
    let candidates = discover::discover_candidates(&classified);

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

    *state.classified.write().await = classified;
    *state.candidates.write().await = candidates;
    *state.export_bundle.write().await = None;

    Ok(())
}
