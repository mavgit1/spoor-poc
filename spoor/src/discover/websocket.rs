use std::collections::BTreeMap;

use crate::classify::websocket::{UNIDENTIFIED_PATTERN, is_unidentified_message};
use crate::classify::{ClassifiedEntry, Protocol};
use crate::discover::{confidence_str, protocol_str};
use crate::types::Candidate;

fn host_of(origin: &str) -> String {
    origin
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .trim_start_matches("wss://")
        .trim_start_matches("ws://")
        .to_string()
}

/// One candidate per named message type, plus **one** candidate per origin for
/// everything we could not name.
///
/// Frames whose type we could not read are labeled by encoding (`text`, `json`,
/// `binary`), which is not an operation. Emitting one candidate per encoding
/// presented "we don't know what this is" as three discovered operations, and
/// because they rank by frame count they crowded out real endpoints — a live
/// session showed the top five candidates all reading `WS · text (36×)`.
pub fn discover(classified: &[ClassifiedEntry]) -> Vec<Candidate> {
    let mut counts: BTreeMap<(String, String), usize> = BTreeMap::new();
    let mut examples: BTreeMap<(String, String), ClassifiedEntry> = BTreeMap::new();

    for item in classified
        .iter()
        .filter(|c| c.protocol == Protocol::WebSocket)
    {
        let msg = item
            .operation_name
            .clone()
            .unwrap_or_else(|| "frame".to_string());
        let msg = if is_unidentified_message(&msg) {
            UNIDENTIFIED_PATTERN.to_string()
        } else {
            msg
        };
        let key = (item.entry.origin.clone(), msg);
        *counts.entry(key.clone()).or_insert(0) += 1;
        examples.entry(key).or_insert_with(|| item.clone());
    }

    counts
        .into_iter()
        .filter_map(|(key, request_count)| {
            let item = examples.get(&key)?;
            let (origin, msg) = key;
            let host = host_of(&origin);
            let id = format!("ws|{origin}|{msg}");
            // Host belongs in the label: several sockets on different hosts can
            // all carry frames we could not name.
            let label = if msg == UNIDENTIFIED_PATTERN {
                format!("WS {host} · unidentified frames")
            } else {
                format!("WS {host} · {msg}")
            };
            Some(Candidate {
                id,
                label,
                protocol: protocol_str(Protocol::WebSocket).to_string(),
                guessed_pattern: msg,
                example: item.entry.flow.url.clone(),
                host,
                methods: vec!["WS".into()],
                confidence: confidence_str(item.confidence).to_string(),
                origin,
                request_count,
                default_selected: false,
                preference_ignored: false,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture::{Body, CaptureRecord, Transport};
    use crate::classify::Confidence;
    use crate::ir::TrafficEntry;

    fn ws_entry(url: &str, message: &str, sequence: u64) -> ClassifiedEntry {
        let entry = TrafficEntry::from_flow(CaptureRecord {
            id: format!("ws:{sequence}"),
            transport: Transport::WebSocket,
            url: url.into(),
            method: None,
            request_headers: Default::default(),
            request_body: Some(Body::text("{}")),
            status: None,
            response_headers: None,
            response_body: None,
            resource_type: Some("WebSocket".into()),
            sequence,
            timestamp_ms: None,
            ws_request_id: Some("1".into()),
            ws_opcode: Some("text".into()),
            direction: None,
        })
        .unwrap();
        ClassifiedEntry {
            entry,
            protocol: Protocol::WebSocket,
            confidence: Confidence::Parser,
            operation_name: Some(message.into()),
        }
    }

    #[test]
    fn unnamed_frames_collapse_to_one_candidate_per_origin() {
        let classified = vec![
            ws_entry("wss://a.example.test/socket", "text", 1),
            ws_entry("wss://a.example.test/socket", "json", 2),
            ws_entry("wss://a.example.test/socket", "binary", 3),
            ws_entry("wss://b.example.test/socket", "text", 4),
        ];
        let candidates = discover(&classified);

        assert_eq!(candidates.len(), 2, "one per origin, got: {candidates:?}");

        let a = candidates
            .iter()
            .find(|c| c.host == "a.example.test")
            .expect("origin a");
        assert_eq!(a.request_count, 3, "all unnamed frames counted together");
        assert_eq!(a.label, "WS a.example.test · unidentified frames");

        // Labels must distinguish the hosts — this is what the live session got wrong.
        let labels: Vec<_> = candidates.iter().map(|c| c.label.clone()).collect();
        assert_eq!(
            labels
                .iter()
                .collect::<std::collections::HashSet<_>>()
                .len(),
            labels.len(),
            "labels must be unique: {labels:?}"
        );
    }

    #[test]
    fn named_messages_stay_separate_and_keep_their_name() {
        let classified = vec![
            ws_entry("wss://a.example.test/socket", "subscribe", 1),
            ws_entry("wss://a.example.test/socket", "subscribe", 2),
            ws_entry("wss://a.example.test/socket", "heartbeat", 3),
            ws_entry("wss://a.example.test/socket", "text", 4),
        ];
        let candidates = discover(&classified);

        assert_eq!(candidates.len(), 3);
        let subscribe = candidates
            .iter()
            .find(|c| c.guessed_pattern == "subscribe")
            .expect("named op kept");
        assert_eq!(subscribe.request_count, 2);
        assert_eq!(subscribe.label, "WS a.example.test · subscribe");
        assert!(
            candidates
                .iter()
                .any(|c| c.guessed_pattern == UNIDENTIFIED_PATTERN),
            "unnamed frame still surfaces, separately from named ops"
        );
    }
}
