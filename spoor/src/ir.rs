use url::Url;

use crate::capture::{Body, CaptureRecord, Transport};

/// Normalized traffic entry for classification and export.
#[derive(Debug, Clone)]
pub struct TrafficEntry {
    pub flow: CaptureRecord,
    pub origin: String,
    pub path: String,
}

impl TrafficEntry {
    pub fn from_flow(flow: CaptureRecord) -> Option<Self> {
        let parsed = Url::parse(&flow.url).ok()?;
        let host = parsed.host_str()?;
        let scheme = parsed.scheme();
        let origin = format!("{scheme}://{host}");
        let mut path = parsed.path().to_string();
        if path.is_empty() {
            // ws://host with no path — keep a stable sentinel.
            path = "/".to_string();
        }
        Some(Self { flow, origin, path })
    }

    pub fn text_request(&self) -> Option<&str> {
        self.flow.text_request()
    }

    pub fn text_response(&self) -> Option<&str> {
        self.flow.text_response()
    }

    pub fn http_method(&self) -> &str {
        self.flow.http_method()
    }

    pub fn is_websocket(&self) -> bool {
        self.flow.transport == Transport::WebSocket
    }

    pub fn request_content_type(&self) -> Option<&str> {
        self.flow.request_content_type()
    }

    pub fn response_content_type(&self) -> Option<&str> {
        self.flow.response_content_type()
    }

    pub fn has_text_response(&self) -> bool {
        matches!(self.flow.response_body, Some(Body::Text(_)))
    }
}

pub fn entries_from_flows(flows: &[CaptureRecord]) -> Vec<TrafficEntry> {
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
