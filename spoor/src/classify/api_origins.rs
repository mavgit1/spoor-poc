use std::collections::{HashMap, HashSet};

use crate::classify::path_rules;
use crate::classify::{ClassifiedEntry, Protocol};
use crate::ir::TrafficEntry;

/// Origins that look like real APIs from captured traffic — not a host blocklist.
pub fn from_classified(classified: &[ClassifiedEntry]) -> HashSet<String> {
    let mut scores: HashMap<String, u32> = HashMap::new();

    for item in classified {
        if path_rules::is_static_asset_path(&item.entry.path) {
            continue;
        }
        let origin = &item.entry.origin;
        match item.protocol {
            Protocol::Graphql => {
                *scores.entry(origin.clone()).or_insert(0) += 10;
            }
            Protocol::Rest if is_api_like_rest(&item.entry) => {
                *scores.entry(origin.clone()).or_insert(0) += 5;
            }
            _ => {}
        }
    }

    scores
        .into_iter()
        .filter(|(_, score)| *score > 0)
        .map(|(origin, _)| origin)
        .collect()
}

fn is_api_like_rest(entry: &TrafficEntry) -> bool {
    crate::classify::rest::looks_like_rest(entry)
}

