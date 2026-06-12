use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;
use url::Url;

use crate::classify::ClassifiedEntry;
use crate::export::auth::is_auth_query_param;

/// Max distinct values listed per query param (session may have more).
const MAX_EXAMPLES: usize = 8;

/// Non-auth query parameters observed on an origin (deduped by name).
#[derive(Debug, Clone, Serialize)]
pub struct QueryParamObservation {
    pub name: String,
    /// Distinct values seen in session (capped); sorted for stable output.
    pub examples: Vec<String>,
    pub seen_on_requests: usize,
    /// Set when more than [`MAX_EXAMPLES`] distinct values were seen.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub distinct_count: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

pub fn observe_for_origin(
    classified: &[ClassifiedEntry],
    origin: &str,
) -> Vec<QueryParamObservation> {
    let mut by_name: BTreeMap<String, ParamStats> = BTreeMap::new();

    for item in classified.iter().filter(|c| c.entry.origin == origin) {
        let Ok(url) = Url::parse(&item.entry.flow.url) else {
            continue;
        };
        for (k, v) in url.query_pairs() {
            let name = k.to_string();
            if is_auth_query_param(&name) {
                continue;
            }
            let stats = by_name.entry(name).or_default();
            stats.seen += 1;
            stats.values.insert(v.to_string());
        }
    }

    by_name
        .into_iter()
        .map(|(name, stats)| build_observation(name, stats))
        .collect()
}

#[derive(Default)]
struct ParamStats {
    values: BTreeSet<String>,
    seen: usize,
}

fn build_observation(name: String, stats: ParamStats) -> QueryParamObservation {
    let distinct = stats.values.len();
    let examples: Vec<String> = stats.values.into_iter().take(MAX_EXAMPLES).collect();
    let distinct_count = if distinct > MAX_EXAMPLES {
        Some(distinct)
    } else {
        None
    };
    let note = if distinct > MAX_EXAMPLES {
        Some(format!(
            "showing {} of {distinct} distinct values",
            MAX_EXAMPLES
        ))
    } else if distinct > 1 {
        Some("values varied across captures".to_string())
    } else {
        None
    };
    QueryParamObservation {
        name,
        examples,
        seen_on_requests: stats.seen,
        distinct_count,
        note,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use crate::classify::{ClassifiedEntry, Confidence, Protocol};
    use crate::ir::TrafficEntry;
    use crate::types::CapturedFlow;

    use super::*;

    fn entry_with_url(url: &str) -> ClassifiedEntry {
        let parsed = url::Url::parse(url).unwrap();
        let origin = format!(
            "{}://{}",
            parsed.scheme(),
            parsed.host_str().expect("host")
        );
        ClassifiedEntry {
            entry: TrafficEntry {
                flow: CapturedFlow {
                    id: "1".into(),
                    url: url.into(),
                    method: "GET".into(),
                    request_headers: HashMap::new(),
                    request_body: None,
                    status: Some(200),
                    response_headers: None,
                    response_body: None,
                    resource_type: None,
                },
                origin,
                path: parsed.path().into(),
            },
            protocol: Protocol::Rest,
            confidence: Confidence::Parser,
            operation_name: None,
        }
    }

    #[test]
    fn collects_distinct_examples() {
        let classified = vec![
            entry_with_url(
                "https://portal.example.test/jobadservice/api/jobAdvertisements/_search?page=0&size=20",
            ),
            entry_with_url(
                "https://portal.example.test/jobadservice/api/jobAdvertisements/_search?page=1&size=20",
            ),
            entry_with_url(
                "https://portal.example.test/jobadservice/api/jobAdvertisements/_search?page=2&size=20",
            ),
            entry_with_url(
                "https://portal.example.test/jobadservice/api/jobAdvertisements/_search?page=0&size=20",
            ),
        ];
        let params = observe_for_origin(&classified, "https://portal.example.test");
        let page = params.iter().find(|p| p.name == "page").expect("page param");
        assert_eq!(page.examples, vec!["0", "1", "2"]);
        assert_eq!(page.seen_on_requests, 4);
        assert!(page.note.as_deref().is_some_and(|n| n.contains("varied")));
        let size = params.iter().find(|p| p.name == "size").expect("size param");
        assert_eq!(size.examples, vec!["20"]);
        assert_eq!(size.seen_on_requests, 4);
        assert!(size.note.is_none());
    }

    #[test]
    fn includes_framework_params_when_present() {
        let classified = vec![entry_with_url(
            "https://portal.example.test/api/foo?_ng=ZGU=&page=0",
        )];
        let params = observe_for_origin(&classified, "https://portal.example.test");
        let ng = params.iter().find(|p| p.name == "_ng").expect("_ng param");
        assert_eq!(ng.examples, vec!["ZGU="]);
        assert!(params.iter().any(|p| p.name == "page"));
    }

    #[test]
    fn caps_distinct_values() {
        let mut classified = Vec::new();
        for i in 0..12 {
            classified.push(entry_with_url(&format!(
                "https://example.com/api/items?page={i}"
            )));
        }
        let page = observe_for_origin(&classified, "https://example.com")
            .into_iter()
            .find(|p| p.name == "page")
            .expect("page");
        assert_eq!(page.examples.len(), MAX_EXAMPLES);
        assert_eq!(page.distinct_count, Some(12));
        assert!(page.note.as_deref().is_some_and(|n| n.contains("showing")));
    }
}
