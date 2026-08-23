use std::collections::BTreeMap;

use crate::classify::{ClassifiedEntry, Protocol};
use crate::discover::{confidence_str, protocol_str};
use crate::types::Candidate;

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
        let key = (item.entry.origin.clone(), msg);
        *counts.entry(key.clone()).or_insert(0) += 1;
        examples.entry(key).or_insert_with(|| item.clone());
    }

    counts
        .into_iter()
        .filter_map(|(key, request_count)| {
            let item = examples.get(&key)?;
            let (origin, msg) = key;
            let host = origin
                .trim_start_matches("https://")
                .trim_start_matches("http://")
                .trim_start_matches("wss://")
                .trim_start_matches("ws://")
                .to_string();
            let id = format!("ws|{origin}|{msg}");
            Some(Candidate {
                id,
                label: format!("WS · {msg}"),
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
