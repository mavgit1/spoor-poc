use serde_json::Value;
use url::Url;

use crate::ir::TrafficEntry;

/// Parse JSON-RPC method(s) from body (object or batch array) or URL query.
pub fn try_parse_methods(entry: &TrafficEntry) -> Option<Vec<String>> {
    if entry.is_websocket() {
        return None;
    }
    if let Some(methods) = methods_from_body(entry) {
        return Some(methods);
    }
    query_method(entry).and_then(|m| {
        if body_looks_jsonrpc(entry) {
            Some(vec![m])
        } else {
            None
        }
    })
}

/// Back-compat: first method only.
pub fn try_parse_method(entry: &TrafficEntry) -> Option<String> {
    try_parse_methods(entry)?.into_iter().next()
}

fn methods_from_body(entry: &TrafficEntry) -> Option<Vec<String>> {
    let body = entry.text_request()?;
    let json: Value = serde_json::from_str(body).ok()?;
    if let Some(arr) = json.as_array() {
        let mut methods = Vec::new();
        for item in arr {
            if is_graphql_body(item) {
                continue;
            }
            if !body_looks_jsonrpc_value(item) {
                continue;
            }
            if let Some(m) = item
                .get("method")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
            {
                if !methods.iter().any(|x| x == m) {
                    methods.push(m.to_string());
                }
            }
        }
        return if methods.is_empty() {
            None
        } else {
            Some(methods)
        };
    }
    if is_graphql_body(&json) {
        return None;
    }
    if !body_looks_jsonrpc_value(&json) {
        return None;
    }
    json.get("method")
        .and_then(|m| m.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| vec![s.to_string()])
}

fn body_looks_jsonrpc(entry: &TrafficEntry) -> bool {
    entry
        .text_request()
        .and_then(|b| serde_json::from_str::<Value>(b).ok())
        .is_some_and(|json| {
            if let Some(arr) = json.as_array() {
                arr.iter()
                    .any(|item| !is_graphql_body(item) && body_looks_jsonrpc_value(item))
            } else {
                !is_graphql_body(&json) && body_looks_jsonrpc_value(&json)
            }
        })
}

fn body_looks_jsonrpc_value(json: &Value) -> bool {
    let Some(method) = json
        .get("method")
        .and_then(|m| m.as_str())
        .filter(|s| !s.is_empty())
    else {
        return false;
    };
    if json.get("query").is_some() {
        return false;
    }
    if json
        .get("jsonrpc")
        .is_some_and(|v| v.is_string() || v.is_number())
    {
        return true;
    }
    json.get("params").is_some() && json.get("id").is_some() && !method.contains(' ')
}

fn is_graphql_body(json: &Value) -> bool {
    json.get("query")
        .and_then(|q| q.as_str())
        .is_some_and(|q| !q.trim().is_empty())
}

fn query_method(entry: &TrafficEntry) -> Option<String> {
    Url::parse(&entry.flow.url)
        .ok()
        .and_then(|u| {
            u.query_pairs()
                .find(|(k, _)| k == "method")
                .map(|(_, v)| v.to_string())
        })
        .filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use crate::capture::{Body, CaptureRecord, Transport};

    use super::*;

    fn entry(url: &str, body: &str) -> TrafficEntry {
        TrafficEntry::from_flow(CaptureRecord {
            id: "1".into(),
            transport: Transport::Http,
            url: url.into(),
            method: Some("POST".into()),
            request_headers: HashMap::from([("content-type".into(), "application/json".into())]),
            request_body: Some(Body::text(body)),
            status: Some(200),
            response_headers: Some(HashMap::from([(
                "content-type".into(),
                "application/json".into(),
            )])),
            response_body: Some(Body::text(r#"{"jsonrpc":"2.0","result":{},"id":1}"#)),
            resource_type: Some("Fetch".into()),
            sequence: 0,
            timestamp_ms: None,
            ws_request_id: None,
            ws_opcode: None,
            direction: None,
        })
        .unwrap()
    }

    #[test]
    fn parses_method_from_jsonrpc_body() {
        let e = entry(
            "https://api.example.test/jsonrpc",
            r#"{"jsonrpc":"2.0","method":"Alpha","params":{"q":"x"},"id":1}"#,
        );
        assert_eq!(try_parse_method(&e).as_deref(), Some("Alpha"));
    }

    #[test]
    fn parses_batch_methods() {
        let e = entry(
            "https://api.example.test/jsonrpc",
            r#"[{"jsonrpc":"2.0","method":"Alpha","params":{},"id":1},{"jsonrpc":"2.0","method":"Beta","params":{},"id":2}]"#,
        );
        let methods = try_parse_methods(&e).unwrap();
        assert_eq!(methods, vec!["Alpha".to_string(), "Beta".to_string()]);
    }

    #[test]
    fn rejects_graphql_body() {
        let e = entry(
            "https://api.example.test/graphql",
            r#"{"query":"query Q { x }","operationName":"Q"}"#,
        );
        assert!(try_parse_method(&e).is_none());
    }
}
