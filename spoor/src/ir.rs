use url::Url;

use crate::rest::types::CapturedRequest;
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

/// In-memory request for the vendored REST engine.
pub struct MemoryRequest {
    url: String,
    method: String,
    request_headers: Vec<(String, String)>,
    request_body: Option<Vec<u8>>,
    response_status: Option<u16>,
    response_reason: String,
    response_headers: Vec<(String, String)>,
    response_body: Option<Vec<u8>>,
    response_content_type: Option<String>,
}

impl MemoryRequest {
    pub fn from_entry(entry: &TrafficEntry) -> Self {
        let flow = &entry.flow;
        let req_headers: Vec<(String, String)> = flow
            .request_headers
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        let resp_headers: Vec<(String, String)> = flow
            .response_headers
            .as_ref()
            .map(|h| h.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
            .unwrap_or_default();
        let response_content_type = resp_headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("content-type"))
            .map(|(_, v)| v.clone());
        Self {
            url: flow.url.clone(),
            method: flow.method.clone(),
            request_headers: req_headers,
            request_body: flow.request_body.as_ref().map(|b| b.as_bytes().to_vec()),
            response_status: flow.status,
            response_reason: String::new(),
            response_headers: resp_headers,
            response_body: flow.response_body.as_ref().map(|b| b.as_bytes().to_vec()),
            response_content_type,
        }
    }
}

impl CapturedRequest for MemoryRequest {
    fn get_url(&self) -> &str {
        &self.url
    }

    fn get_method(&self) -> &str {
        &self.method
    }

    fn get_request_headers(&self) -> &[(String, String)] {
        &self.request_headers
    }

    fn get_request_body(&self) -> Option<&[u8]> {
        self.request_body.as_deref()
    }

    fn get_response_status_code(&self) -> Option<u16> {
        self.response_status
    }

    fn get_response_reason(&self) -> Option<&str> {
        Some(&self.response_reason)
    }

    fn get_response_headers(&self) -> Option<&[(String, String)]> {
        Some(&self.response_headers)
    }

    fn get_response_body(&self) -> Option<&[u8]> {
        self.response_body.as_deref()
    }

    fn get_response_content_type(&self) -> Option<&str> {
        self.response_content_type.as_deref()
    }
}

pub fn entries_from_flows(flows: &[CapturedFlow]) -> Vec<TrafficEntry> {
    flows.iter().filter_map(|f| TrafficEntry::from_flow(f.clone())).collect()
}

pub fn unique_origins(entries: &[TrafficEntry]) -> Vec<String> {
    let mut origins: Vec<String> = entries.iter().map(|e| e.origin.clone()).collect();
    origins.sort();
    origins.dedup();
    origins
}

