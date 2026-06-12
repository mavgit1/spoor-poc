use url::Url;

use crate::types::CapturedFlow;

/// Normalized traffic entry for classification and export.
#[derive(Debug, Clone)]
pub struct TrafficEntry {
    pub flow: CapturedFlow,
    pub origin: String,
    pub path: String,
}

impl TrafficEntry {
    pub fn from_flow(flow: CapturedFlow) -> Option<Self> {
        let parsed = Url::parse(&flow.url).ok()?;
        let host = parsed.host_str()?;
        let scheme = parsed.scheme();
        let origin = format!("{scheme}://{host}");
        let path = parsed.path().to_string();
        if path.is_empty() {
            return None;
        }
        Some(Self {
            flow,
            origin,
            path,
        })
    }
}

pub fn entries_from_flows(flows: &[CapturedFlow]) -> Vec<TrafficEntry> {
    flows
        .iter()
        .filter_map(|f| TrafficEntry::from_flow(f.clone()))
        .collect()
}

pub fn unique_origins(entries: &[TrafficEntry]) -> Vec<String> {
    let mut origins: Vec<String> = entries.iter().map(|e| e.origin.clone()).collect();
    origins.sort();
    origins.dedup();
    origins
}
