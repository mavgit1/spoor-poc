use crate::classify::filters;
use crate::ir::TrafficEntry;

const API_RESOURCE_TYPES: &[&str] = &["Fetch", "XHR", "Document"];

pub fn looks_like_rest(entry: &TrafficEntry) -> bool {
    if entry.is_websocket() {
        return false;
    }
    if filters::is_non_api_path(&entry.path) {
        return false;
    }
    if !has_api_resource_type(entry) && !path_looks_like_api(&entry.path) {
        return false;
    }
    if entry.flow.response_body.is_none() && entry.text_response().is_none() {
        // Allow tRPC-ish paths even without response body during capture gaps.
        if trpc_operation_name(entry).is_none() {
            return false;
        }
    }
    let method = entry.http_method().to_uppercase();
    match method.as_str() {
        "POST" | "PUT" | "PATCH" | "DELETE" => {
            request_is_json(entry)
                || response_is_json(entry)
                || trpc_operation_name(entry).is_some()
        }
        "GET" => response_is_json(entry) || trpc_operation_name(entry).is_some(),
        _ => false,
    }
}

/// tRPC-style path/query labeling — still Protocol::Rest.
pub fn trpc_operation_name(entry: &TrafficEntry) -> Option<String> {
    let path = &entry.path;
    if let Some(rest) = path.strip_prefix("/trpc/") {
        let proc = rest.split('/').next().unwrap_or("").split('?').next()?;
        if !proc.is_empty() {
            return Some(format!("trpc:{proc}"));
        }
    }
    if path == "/trpc" || path.ends_with("/trpc") {
        if let Ok(url) = url::Url::parse(&entry.flow.url) {
            // ?batch=1&input=... — use path or first procedure hint from body keys
            if url.query_pairs().any(|(k, _)| k == "batch") {
                return Some("trpc:batch".into());
            }
        }
        return Some("trpc".into());
    }
    None
}

fn request_is_json(entry: &TrafficEntry) -> bool {
    entry
        .request_content_type()
        .is_some_and(|v| v.contains("application/json") || v.contains("+json"))
        || entry
            .text_request()
            .is_some_and(|b| serde_json::from_str::<serde_json::Value>(b).is_ok())
}

fn response_is_json(entry: &TrafficEntry) -> bool {
    if entry
        .response_content_type()
        .is_some_and(|ct| ct.contains("application/json") || ct.contains("+json"))
    {
        return true;
    }
    entry
        .text_response()
        .is_some_and(|b| serde_json::from_str::<serde_json::Value>(b).is_ok())
}

fn has_api_resource_type(entry: &TrafficEntry) -> bool {
    let rt = entry.flow.resource_type.as_deref().unwrap_or("");
    if rt.is_empty() || rt == "None" {
        return false;
    }
    let lower = rt.to_ascii_lowercase();
    API_RESOURCE_TYPES
        .iter()
        .any(|t| lower.contains(&t.to_ascii_lowercase()))
}

fn path_looks_like_api(path: &str) -> bool {
    path.contains("/api/")
        || path.contains("-service/")
        || path.ends_with("/api")
        || path.contains("/trpc")
}
