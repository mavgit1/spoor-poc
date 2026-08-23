use std::collections::BTreeMap;

use crate::classify::{ClassifiedEntry, Protocol};
use crate::discover::{confidence_str, protocol_str};
use crate::types::Candidate;

pub fn discover(classified: &[ClassifiedEntry]) -> Vec<Candidate> {
    let mut counts: BTreeMap<(String, String, String), usize> = BTreeMap::new();
    let mut examples: BTreeMap<(String, String, String), ClassifiedEntry> = BTreeMap::new();

    for item in classified.iter().filter(|c| c.protocol == Protocol::Form) {
        let method = item.entry.http_method().to_uppercase();
        let path = item.entry.path.clone();
        let key = (item.entry.origin.clone(), method, path);
        *counts.entry(key.clone()).or_insert(0) += 1;
        examples.entry(key).or_insert_with(|| item.clone());
    }

    counts
        .into_iter()
        .filter_map(|(key, request_count)| {
            let item = examples.get(&key)?;
            let (origin, method, path) = key;
            let host = origin
                .trim_start_matches("https://")
                .trim_start_matches("http://")
                .to_string();
            let id = format!("form|{origin}|{method}|{path}");
            Some(Candidate {
                id,
                label: format!("{method} {path}"),
                protocol: protocol_str(Protocol::Form).to_string(),
                guessed_pattern: path,
                example: format!("{method} {}", item.entry.flow.url),
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
