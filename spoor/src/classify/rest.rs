use crate::classify::path_rules;
use crate::ir::TrafficEntry;

const API_RESOURCE_TYPES: &[&str] = &["Fetch", "XHR", "Document"];

pub fn looks_like_rest(entry: &TrafficEntry) -> bool {
    if path_rules::is_static_asset_path(&entry.path) {
        return false;
    }
    let rt = entry.flow.resource_type.as_deref().unwrap_or("");
    if !API_RESOURCE_TYPES.iter().any(|t| rt.contains(t)) {
        return false;
    }
    if entry.flow.response_body.is_none() {
        return false;
    }
    let method = entry.flow.method.to_uppercase();
    match method.as_str() {
        "POST" | "PUT" | "PATCH" | "DELETE" => request_is_json(entry) || response_is_json(entry),
        "GET" => response_is_json(entry),
        _ => false,
    }
}

fn request_is_json(entry: &TrafficEntry) -> bool {
    entry
        .flow
        .request_headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("content-type"))
        .map(|(_, v)| v.contains("application/json") || v.contains("+json"))
        .unwrap_or(false)
        || entry
            .flow
            .request_body
            .as_ref()
            .is_some_and(|b| serde_json::from_str::<serde_json::Value>(b).is_ok())
}

fn response_is_json(entry: &TrafficEntry) -> bool {
    entry
        .flow
        .response_headers
        .as_ref()
        .and_then(|h| {
            h.iter()
                .find(|(k, _)| k.eq_ignore_ascii_case("content-type"))
                .map(|(_, v)| v.as_str())
        })
        .is_some_and(|ct| ct.contains("application/json") || ct.contains("+json"))
}
