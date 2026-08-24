use serde_json::Value;

use crate::ir::TrafficEntry;

/// Classify WebSocket frames: JSON message type / method / event, or binary.
pub fn try_parse_message(entry: &TrafficEntry) -> Option<String> {
    if !entry.is_websocket() {
        return None;
    }

    let text = entry.text_request().or_else(|| entry.text_response());

    if let Some(text) = text {
        if let Ok(json) = serde_json::from_str::<Value>(text) {
            if let Some(name) = message_name(&json) {
                return Some(name);
            }
            return Some("json".into());
        }
        return Some("text".into());
    }

    if entry.flow.has_binary_payload()
        || entry
            .flow
            .ws_opcode
            .as_deref()
            .is_some_and(|o| o == "binary")
    {
        return Some("binary".into());
    }

    Some("frame".into())
}

fn message_name(json: &Value) -> Option<String> {
    for key in ["type", "method", "event", "action", "op"] {
        if let Some(s) = json
            .get(key)
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
        {
            return Some(s.to_string());
        }
    }
    // GraphQL subscription over WS: { type: "...", payload: { query } } already covered by type.
    // JSON-RPC over WS:
    if json.get("jsonrpc").is_some()
        && let Some(m) = json.get("method").and_then(|v| v.as_str())
    {
        return Some(m.to_string());
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture::{Body, CaptureRecord, Direction, Transport};

    #[test]
    fn parses_typed_json_frame() {
        let entry = TrafficEntry::from_flow(CaptureRecord {
            id: "ws:1:1".into(),
            transport: Transport::WebSocket,
            url: "wss://api.example.test/socket".into(),
            method: None,
            request_headers: Default::default(),
            request_body: Some(Body::text(r#"{"type":"subscribe","channel":"jobs"}"#)),
            status: None,
            response_headers: None,
            response_body: None,
            resource_type: Some("WebSocket".into()),
            sequence: 1,
            timestamp_ms: Some(0),
            ws_request_id: Some("1".into()),
            ws_opcode: Some("text".into()),
            direction: Some(Direction::Outbound),
        })
        .unwrap();
        assert_eq!(try_parse_message(&entry).as_deref(), Some("subscribe"));
    }
}
