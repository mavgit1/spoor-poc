use crate::classify::Protocol;
use crate::ir::TrafficEntry;

/// Detect gRPC-Web / protobuf by content-type (no field decode).
pub fn try_detect(entry: &TrafficEntry) -> Option<Protocol> {
    if entry.is_websocket() {
        return None;
    }
    let req_ct = entry.request_content_type().unwrap_or("");
    let resp_ct = entry.response_content_type().unwrap_or("");

    if is_grpc_web(req_ct) || is_grpc_web(resp_ct) {
        return Some(Protocol::GrpcWeb);
    }
    if is_protobuf_ct(req_ct) || is_protobuf_ct(resp_ct) {
        return Some(Protocol::Protobuf);
    }
    if entry
        .flow
        .response_headers
        .as_ref()
        .is_some_and(|h| header_has(h, "grpc-status") || header_has(h, "grpc-message"))
    {
        return Some(Protocol::GrpcWeb);
    }
    None
}

fn header_has(headers: &std::collections::HashMap<String, String>, name: &str) -> bool {
    headers.iter().any(|(k, _)| k.eq_ignore_ascii_case(name))
}

fn is_grpc_web(ct: &str) -> bool {
    let base = ct
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    base.starts_with("application/grpc")
}

fn is_protobuf_ct(ct: &str) -> bool {
    let base = ct
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    base == "application/x-protobuf"
        || base == "application/protobuf"
        || base == "application/vnd.google.protobuf"
        || base.ends_with("+protobuf")
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::capture::{Body, CaptureRecord, Transport};
    use crate::ir::TrafficEntry;

    fn entry_with_ct(req_ct: &str) -> TrafficEntry {
        TrafficEntry::from_flow(CaptureRecord {
            id: "1".into(),
            transport: Transport::Http,
            url: "https://api.example.test/v1/Translate".into(),
            method: Some("POST".into()),
            request_headers: HashMap::from([("content-type".into(), req_ct.into())]),
            request_body: Some(Body::Bytes {
                data: vec![0, 1, 2],
                content_type: Some(req_ct.into()),
            }),
            status: Some(200),
            response_headers: Some(HashMap::from([("content-type".into(), req_ct.into())])),
            response_body: Some(Body::Bytes {
                data: vec![3, 4],
                content_type: Some(req_ct.into()),
            }),
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
    fn detects_grpc_web() {
        assert_eq!(
            try_detect(&entry_with_ct("application/grpc-web+proto")),
            Some(Protocol::GrpcWeb)
        );
    }

    #[test]
    fn detects_protobuf() {
        assert_eq!(
            try_detect(&entry_with_ct("application/x-protobuf")),
            Some(Protocol::Protobuf)
        );
    }
}
