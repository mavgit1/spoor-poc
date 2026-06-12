use std::collections::BTreeMap;

use serde::Serialize;
use serde_json::Value;

use crate::classify::{ClassifiedEntry, Protocol};

const MAX_VALUES_PER_PARAM: usize = 40;

#[derive(Debug, Clone, Serialize)]
pub struct FilterParamCatalog {
    pub param: String,
    pub source: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    pub values: Vec<FilterValue>,
}

#[derive(Debug, Clone, Serialize)]
pub struct FilterValue {
    pub value: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hits: Option<u64>,
}

/// Build a query-param value catalog from captured JSON (facets, aggregations, typeahead).
pub fn extract_filter_catalog(classified: &[ClassifiedEntry], origin: &str) -> Vec<FilterParamCatalog> {
    let mut by_param: BTreeMap<String, (String, BTreeMap<String, FilterValue>)> = BTreeMap::new();

    for item in classified
        .iter()
        .filter(|c| c.protocol == Protocol::Rest && c.entry.origin == origin)
    {
        let Some(body) = item.entry.flow.response_body.as_ref() else {
            continue;
        };
        let Ok(json) = serde_json::from_str::<Value>(body) else {
            continue;
        };
        let source = format!(
            "{} {}",
            item.entry.flow.method.to_uppercase(),
            item.entry.path
        );

        if let Some(facets) = json.get("facets").and_then(|v| v.as_object()) {
            for (param, facet_body) in facets {
                if let Some(map) = facet_body.as_object() {
                    if looks_like_count_map(map) {
                        merge_count_map(&mut by_param, param, &source, map);
                    }
                }
            }
        }

        if let Some(aggs) = json.get("aggregations").and_then(|v| v.as_object()) {
            for (agg_name, agg_body) in aggs {
                if let Some(buckets) = agg_body
                    .get("buckets")
                    .and_then(|v| v.as_array())
                {
                    merge_buckets(
                        &mut by_param,
                        agg_name,
                        &source,
                        buckets,
                        "bucket key from aggregations (may differ from search query param names)",
                    );
                }
            }
        }

        if let Some(results) = json.get("result").and_then(|v| v.as_array()) {
            merge_typeahead_results(&mut by_param, &source, results);
        }

        if let Some(items) = json
            .get("data")
            .and_then(|d| d.as_object())
            .and_then(|_| json.pointer("/data/items"))
            .or_else(|| json.get("items"))
            .and_then(|v| v.as_array())
        {
            merge_typeahead_results(&mut by_param, &source, items);
        }
    }

    let mut out: Vec<FilterParamCatalog> = by_param
        .into_iter()
        .map(|(param, (source, values))| {
            let mut values: Vec<FilterValue> = values.into_values().collect();
            values.sort_by(|a, b| {
                b.hits
                    .unwrap_or(0)
                    .cmp(&a.hits.unwrap_or(0))
                    .then_with(|| a.value.cmp(&b.value))
            });
            let truncated = values.len() > MAX_VALUES_PER_PARAM;
            values.truncate(MAX_VALUES_PER_PARAM);
            FilterParamCatalog {
                param,
                source,
                note: truncated.then(|| {
                    format!("showing top {MAX_VALUES_PER_PARAM} values by hit count; call source op for full set")
                }),
                values,
            }
        })
        .collect();

    out.sort_by(|a, b| a.param.cmp(&b.param));
    out
}

fn looks_like_count_map(map: &serde_json::Map<String, Value>) -> bool {
    map.len() >= 2
        && map
            .values()
            .all(|v| v.is_number() || v.as_str().is_some_and(|s| s.parse::<u64>().is_ok()))
}

fn merge_count_map(
    by_param: &mut BTreeMap<String, (String, BTreeMap<String, FilterValue>)>,
    param: &str,
    source: &str,
    map: &serde_json::Map<String, Value>,
) {
    let entry = by_param
        .entry(param.to_string())
        .or_insert_with(|| (source.to_string(), BTreeMap::new()));
    for (key, count) in map {
        let hits = json_as_u64(count);
        entry.1
            .entry(key.clone())
            .and_modify(|v| {
                if hits > v.hits.unwrap_or(0) {
                    v.hits = Some(hits);
                }
            })
            .or_insert(FilterValue {
                value: key.clone(),
                label: None,
                hits: Some(hits),
            });
    }
}

fn merge_buckets(
    by_param: &mut BTreeMap<String, (String, BTreeMap<String, FilterValue>)>,
    param: &str,
    source: &str,
    buckets: &[Value],
    _note: &str,
) {
    let entry = by_param
        .entry(param.to_string())
        .or_insert_with(|| (source.to_string(), BTreeMap::new()));
    for bucket in buckets {
        let Some(obj) = bucket.as_object() else {
            continue;
        };
        let key = obj
            .get("key")
            .map(value_to_string)
            .filter(|s| !s.is_empty());
        let Some(key) = key else {
            continue;
        };
        let hits = obj.get("docCount").map(json_as_u64);
        entry.1.entry(key.clone()).or_insert(FilterValue {
            value: key,
            label: None,
            hits,
        });
    }
}

fn merge_typeahead_results(
    by_param: &mut BTreeMap<String, (String, BTreeMap<String, FilterValue>)>,
    source: &str,
    results: &[Value],
) {
    if results.is_empty() {
        return;
    }
    let param = "typeahead";
    let entry = by_param
        .entry(param.to_string())
        .or_insert_with(|| (source.to_string(), BTreeMap::new()));
    for item in results {
        let Some(obj) = item.as_object() else {
            continue;
        };
        let id = obj
            .get("id")
            .or_else(|| obj.get("key"))
            .map(value_to_string);
        let label = obj
            .get("name_display")
            .or_else(|| obj.get("name"))
            .or_else(|| obj.get("title"))
            .map(value_to_string);
        let Some(id) = id.filter(|s| !s.is_empty()) else {
            if let Some(label) = label.filter(|s| !s.is_empty()) {
                entry.1.entry(label.clone()).or_insert(FilterValue {
                    value: label.clone(),
                    label: Some(label),
                    hits: None,
                });
            }
            continue;
        };
        entry.1.entry(id.clone()).or_insert(FilterValue {
            value: id,
            label: label.filter(|l| !l.is_empty()),
            hits: None,
        });
    }
}

fn json_as_u64(v: &Value) -> u64 {
    v.as_u64()
        .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
        .unwrap_or(0)
}

fn value_to_string(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::classify::{Confidence, Protocol};
    use crate::ir::TrafficEntry;
    use crate::types::CapturedFlow;

    fn rest_entry(origin: &str, path: &str, body: &str) -> ClassifiedEntry {
        ClassifiedEntry {
            entry: TrafficEntry {
                flow: CapturedFlow {
                    id: "test".into(),
                    url: format!("{origin}{path}"),
                    method: "GET".into(),
                    request_headers: Default::default(),
                    request_body: None,
                    status: Some(200),
                    response_headers: None,
                    response_body: Some(body.into()),
                    resource_type: Some("Fetch".into()),
                },
                origin: origin.into(),
                path: path.into(),
            },
            protocol: Protocol::Rest,
            confidence: Confidence::Parser,
            operation_name: None,
        }
    }

    #[test]
    fn extracts_facets_map() {
        let body = r#"{"facets":{"companySegments":{"kmu":10,"gu":5},"regionIds":{"7":100,"11":50}}}"#;
        let entries = vec![rest_entry(
            "https://search-api.example.test",
            "/aggregations",
            body,
        )];
        let catalog = extract_filter_catalog(&entries, "https://search-api.example.test");
        assert!(catalog.iter().any(|c| c.param == "companySegments"));
        let segs = catalog.iter().find(|c| c.param == "companySegments").unwrap();
        assert!(segs.values.iter().any(|v| v.value == "kmu" && v.hits == Some(10)));
    }

    #[test]
    fn extracts_aggregation_buckets() {
        let body = r#"{"aggregations":{"place":{"buckets":[{"key":"Zürich","docCount":42}]}}}"#;
        let entries = vec![rest_entry(
            "https://search-api.example.test",
            "/aggregations",
            body,
        )];
        let catalog = extract_filter_catalog(&entries, "https://search-api.example.test");
        let place = catalog.iter().find(|c| c.param == "place").unwrap();
        assert_eq!(place.values[0].value, "Zürich");
        assert_eq!(place.values[0].hits, Some(42));
    }
}
