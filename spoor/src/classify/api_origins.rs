use std::collections::{HashMap, HashSet};

use crate::classify::filters;
use crate::classify::{ClassifiedEntry, Protocol};

/// Origins that look like real APIs from captured traffic — not a host blocklist.
pub fn from_classified(classified: &[ClassifiedEntry]) -> HashSet<String> {
    let mut scores: HashMap<String, u32> = HashMap::new();

    for item in classified {
        if filters::is_non_api_path(&item.entry.path) {
            continue;
        }
        let origin = &item.entry.origin;
        match item.protocol {
            Protocol::Graphql => {
                *scores.entry(origin.clone()).or_insert(0) += 10;
            }
            // Any classified REST counts — LLM-tagged traffic may not re-pass looks_like_rest.
            Protocol::Rest => {
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

