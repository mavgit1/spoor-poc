use std::collections::{BTreeSet, HashMap, HashSet};

use serde::Serialize;
use serde_json::Value;

use crate::classify::{ClassifiedEntry, CoverageReport};

#[derive(Debug, Clone, Serialize)]
pub struct DependsOnEdge {
    pub from: String,
    pub to: String,
    pub via: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub cross_origin: bool,
}

/// Session-wide id handoffs — same- and cross-origin (e.g. https API → wss).
pub fn infer_depends_on_session(
    classified: &[ClassifiedEntry],
    selected_ops: &HashSet<String>,
) -> Vec<DependsOnEdge> {
    infer_depends_on_filtered(classified, selected_ops, None)
}

fn infer_depends_on_filtered(
    classified: &[ClassifiedEntry],
    selected_ops: &HashSet<String>,
    origin_filter: Option<&str>,
) -> Vec<DependsOnEdge> {
    let mut items: Vec<&ClassifiedEntry> = classified
        .iter()
        .filter(|c| origin_filter.is_none_or(|o| c.entry.origin == o))
        .filter(|c| {
            if selected_ops.is_empty() {
                return true;
            }
            let name = c
                .operation_name
                .clone()
                .unwrap_or_else(|| c.entry.path.clone());
            selected_ops.contains(&name)
                || selected_ops.contains(&c.entry.path)
                || c.operation_name
                    .as_ref()
                    .is_some_and(|n| selected_ops.contains(n))
        })
        .collect();
    items.sort_by_key(|c| c.entry.flow.sequence);

    // Map value -> (op_name, sequence, origin) first seen in a response
    let mut produced: HashMap<String, (String, u64, String)> = HashMap::new();
    let mut edges: Vec<DependsOnEdge> = Vec::new();
    let mut seen_edges: BTreeSet<(String, String, String)> = BTreeSet::new();

    for item in &items {
        let op = op_label(item);
        if let Some(resp) = item.entry.text_response() {
            if let Ok(json) = serde_json::from_str::<Value>(resp) {
                for (key, val) in collect_id_values(&json) {
                    produced.entry(val).or_insert_with(|| {
                        (
                            op.clone(),
                            item.entry.flow.sequence,
                            item.entry.origin.clone(),
                        )
                    });
                    let _ = key;
                }
            }
        }
        // Also scan URL path/query for id-like consumption
        if let Ok(url) = url::Url::parse(&item.entry.flow.url) {
            for (via, val) in url.query_pairs() {
                let val = val.to_string();
                if looks_like_id_value(&val) || looks_like_id_key(&via) {
                    if let Some((from, seq, from_origin)) = produced.get(&val) {
                        if *seq < item.entry.flow.sequence && from != &op {
                            let key = (from.clone(), op.clone(), via.to_string());
                            if seen_edges.insert(key.clone()) {
                                edges.push(DependsOnEdge {
                                    from: from.clone(),
                                    to: op.clone(),
                                    via: via.to_string(),
                                    cross_origin: from_origin != &item.entry.origin,
                                });
                            }
                        }
                    }
                }
            }
        }
        if let Some(req) = item.entry.text_request() {
            if let Ok(json) = serde_json::from_str::<Value>(req) {
                for (via, val) in collect_id_values(&json) {
                    if let Some((from, seq, from_origin)) = produced.get(&val) {
                        if *seq < item.entry.flow.sequence && from != &op {
                            let key = (from.clone(), op.clone(), via.clone());
                            if seen_edges.insert(key.clone()) {
                                edges.push(DependsOnEdge {
                                    from: from.clone(),
                                    to: op.clone(),
                                    via,
                                    cross_origin: from_origin != &item.entry.origin,
                                });
                            }
                        }
                    }
                }
            }
        }
    }

    edges
}

fn op_label(item: &ClassifiedEntry) -> String {
    item.operation_name
        .clone()
        .unwrap_or_else(|| format!("{} {}", item.entry.http_method(), item.entry.path))
}

pub(crate) fn collect_id_values(json: &Value) -> Vec<(String, String)> {
    let mut out = Vec::new();
    walk_ids(json, &mut out);
    out
}

fn walk_ids(json: &Value, out: &mut Vec<(String, String)>) {
    match json {
        Value::Object(map) => {
            for (k, v) in map {
                if let Some(s) = v.as_str() {
                    if looks_like_id_key(k) || looks_like_id_value(s) {
                        out.push((k.clone(), s.to_string()));
                    }
                } else {
                    walk_ids(v, out);
                }
            }
        }
        Value::Array(arr) => {
            for v in arr {
                walk_ids(v, out);
            }
        }
        _ => {}
    }
}

fn looks_like_id_key(key: &str) -> bool {
    key == "id"
        || key == "processId"
        || key.ends_with("Id")
        || key.ends_with("_id")
        || key.eq_ignore_ascii_case("uuid")
}

pub(crate) fn looks_like_id_value(s: &str) -> bool {
    if s.len() < 8 || s.len() > 64 {
        return false;
    }
    // UUID-ish
    let hexish = s
        .chars()
        .filter(|c| c.is_ascii_hexdigit() || *c == '-')
        .count();
    hexish == s.len() && s.contains('-')
}

pub fn coverage_for_origin(
    flows: &[crate::capture::CaptureRecord],
    classified: &[ClassifiedEntry],
    origin: &str,
    flows_capped: bool,
) -> CoverageReport {
    let origin_flows: Vec<_> = flows
        .iter()
        .filter(|f| {
            url::Url::parse(&f.url)
                .ok()
                .and_then(|u| {
                    let host = u.host_str()?;
                    Some(format!("{}://{host}", u.scheme()) == origin)
                })
                .unwrap_or(false)
        })
        .cloned()
        .collect();
    let origin_classified: Vec<_> = classified
        .iter()
        .filter(|c| c.entry.origin == origin)
        .cloned()
        .collect();
    CoverageReport::from_session(&origin_flows, &origin_classified, flows_capped)
}
