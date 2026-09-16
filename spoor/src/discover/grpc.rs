use std::collections::BTreeMap;

use crate::classify::{ClassifiedEntry, Protocol};
use crate::discover::{confidence_str, protocol_str};
use crate::types::Candidate;

pub fn discover(classified: &[ClassifiedEntry]) -> Vec<Candidate> {
    let mut counts: BTreeMap<(String, Protocol, String), usize> = BTreeMap::new();
    let mut examples: BTreeMap<(String, Protocol, String), ClassifiedEntry> = BTreeMap::new();

    for item in classified
        .iter()
        .filter(|c| matches!(c.protocol, Protocol::GrpcWeb | Protocol::Protobuf))
    {
        let path = item.entry.path.clone();
        let key = (item.entry.origin.clone(), item.protocol, path);
        *counts.entry(key.clone()).or_insert(0) += 1;
        examples.entry(key).or_insert_with(|| item.clone());
    }

    counts
        .into_iter()
        .filter_map(|(key, request_count)| {
            let item = examples.get(&key)?;
            let (origin, protocol, path) = key;
            let host = origin
                .trim_start_matches("https://")
                .trim_start_matches("http://")
                .to_string();
            let prefix = match protocol {
                Protocol::GrpcWeb => "grpcweb",
                Protocol::Protobuf => "protobuf",
                _ => "protobuf",
            };
            let method = item.entry.http_method().to_uppercase();
            let method = if method.is_empty() {
                "POST".to_string()
            } else {
                method
            };
            let id = format!("{prefix}|{origin}|{method}|{path}");
            Some(Candidate {
                id,
                label: format!("{method} {path} · {prefix} (undecoded)"),
                protocol: protocol_str(protocol).to_string(),
                guessed_pattern: path.clone(),
                example: item.entry.flow.url.clone(),
                host,
                methods: vec![method],
                confidence: confidence_str(item.confidence).to_string(),
                origin,
                request_count,
                default_selected: false,
                preference_ignored: false,
            })
        })
        .collect()
}
