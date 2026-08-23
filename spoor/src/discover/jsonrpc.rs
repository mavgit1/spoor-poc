use std::collections::BTreeMap;

use crate::classify::{ClassifiedEntry, Protocol};
use crate::discover::{confidence_str, protocol_str};
use crate::types::Candidate;

pub fn discover(classified: &[ClassifiedEntry]) -> Vec<Candidate> {
    let mut counts: BTreeMap<(String, String, String, String), usize> = BTreeMap::new();
    let mut examples: BTreeMap<(String, String, String, String), ClassifiedEntry> = BTreeMap::new();

    for item in classified
        .iter()
        .filter(|c| c.protocol == Protocol::JsonRpc)
    {
        let method = item
            .operation_name
            .clone()
            .unwrap_or_else(|| "anonymous".to_string());
        let http_method = item.entry.http_method().to_uppercase();
        let path = item.entry.path.clone();
        let key = (
            item.entry.origin.clone(),
            http_method.clone(),
            path.clone(),
            method.clone(),
        );
        *counts.entry(key.clone()).or_insert(0) += 1;
        examples.entry(key).or_insert_with(|| item.clone());
    }

    counts
        .into_iter()
        .filter_map(|(key, request_count)| {
            let item = examples.get(&key)?;
            let (origin, http_method, path, rpc_method) = key;
            let host = origin
                .trim_start_matches("https://")
                .trim_start_matches("http://")
                .to_string();
            let id = format!("jsonrpc|{origin}|{http_method}|{path}|{rpc_method}");
            Some(Candidate {
                id,
                label: format!("{http_method} {path} · {rpc_method}"),
                protocol: protocol_str(Protocol::JsonRpc).to_string(),
                guessed_pattern: rpc_method.clone(),
                example: item.entry.flow.url.clone(),
                host,
                methods: vec![http_method],
                confidence: confidence_str(item.confidence).to_string(),
                origin,
                request_count,
                default_selected: false,
                preference_ignored: false,
            })
        })
        .collect()
}
