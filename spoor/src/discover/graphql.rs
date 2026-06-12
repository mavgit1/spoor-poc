use std::collections::BTreeMap;

use crate::classify::{ClassifiedEntry, Protocol};
use crate::discover::{confidence_str, protocol_str};
use crate::types::Candidate;

pub fn discover(classified: &[ClassifiedEntry]) -> Vec<Candidate> {
    let mut counts: BTreeMap<(String, String), usize> = BTreeMap::new();
    let mut examples: BTreeMap<(String, String), ClassifiedEntry> = BTreeMap::new();

    for item in classified.iter().filter(|c| c.protocol == Protocol::Graphql) {
        let op = item
            .operation_name
            .clone()
            .unwrap_or_else(|| "anonymous".to_string());
        let key = (item.entry.origin.clone(), op);
        *counts.entry(key.clone()).or_insert(0) += 1;
        examples.entry(key).or_insert_with(|| item.clone());
    }

    counts
        .into_iter()
        .filter_map(|(key, request_count)| {
            let item = examples.get(&key)?;
            let (origin, op) = key;
            let host = origin
                .trim_start_matches("https://")
                .trim_start_matches("http://")
                .to_string();
            let id = format!("graphql|{origin}|{op}");
            Some(Candidate {
                id,
                label: format!("GraphQL {op}"),
                protocol: protocol_str(Protocol::Graphql).to_string(),
                guessed_pattern: op.clone(),
                example: item.entry.flow.url.clone(),
                host,
                methods: vec![item.entry.flow.method.to_uppercase()],
                confidence: confidence_str(item.confidence).to_string(),
                origin,
                request_count,
            })
        })
        .collect()
}
