//! Evidence-only observations — never speculative API advice.
//! Every observation must be grounded in captured traffic.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use serde::Serialize;
use serde_json::Value;
use url::Url;

use crate::classify::ClassifiedEntry;

#[derive(Debug, Clone, Serialize)]
pub struct Observation {
    /// Closed vocabulary + extensible via `detail`.
    pub kind: String,
    pub detail: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evidence_count: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub op_id: Option<String>,
}

/// Derive surface-level observations from classified traffic for selected ops only.
pub fn for_entries(
    entries: &[&ClassifiedEntry],
    op_ids_by_entry: &HashMap<String, String>,
) -> Vec<Observation> {
    let mut out = Vec::new();
    if entries.is_empty() {
        return out;
    }

    out.extend(header_consistency(entries, op_ids_by_entry));
    out.extend(status_patterns(entries, op_ids_by_entry));
    out.extend(query_param_variety(entries));
    out.extend(body_field_presence(entries, op_ids_by_entry));
    out.extend(content_type_notes(entries));
    out.extend(binary_or_empty_bodies(entries, op_ids_by_entry));
    out.extend(sequence_clustering(entries));
    out.extend(response_catalog_keys(entries, op_ids_by_entry));

    out
}

fn op_id_for(entry: &ClassifiedEntry, map: &HashMap<String, String>) -> Option<String> {
    map.get(&entry.entry.flow.id).cloned()
}

fn header_consistency(
    entries: &[&ClassifiedEntry],
    op_ids: &HashMap<String, String>,
) -> Vec<Observation> {
    let n = entries.len();
    if n < 2 {
        return Vec::new();
    }
    let mut header_counts: BTreeMap<String, usize> = BTreeMap::new();
    for e in entries {
        let mut seen = BTreeSet::new();
        for k in e.entry.flow.request_headers.keys() {
            let lower = k.to_ascii_lowercase();
            if matches!(
                lower.as_str(),
                "user-agent"
                    | "accept"
                    | "accept-encoding"
                    | "accept-language"
                    | "connection"
                    | "host"
                    | "content-length"
                    | "origin"
                    | "referer"
                    | "sec-ch-ua"
                    | "sec-ch-ua-mobile"
                    | "sec-ch-ua-platform"
                    | "sec-fetch-dest"
                    | "sec-fetch-mode"
                    | "sec-fetch-site"
            ) {
                continue;
            }
            seen.insert(lower);
        }
        for k in seen {
            *header_counts.entry(k).or_insert(0) += 1;
        }
    }

    let mut out = Vec::new();
    for (name, count) in header_counts {
        if count == n {
            out.push(Observation {
                kind: "header_always_present".into(),
                detail: format!(
                    "Request header `{name}` present on all {n} captured call(s) in this surface"
                ),
                evidence_count: Some(count),
                op_id: None,
            });
        } else if count * 2 >= n && count < n {
            // Partial — still evidence, not noise
            let sample_op = entries
                .iter()
                .find(|e| {
                    e.entry
                        .flow
                        .request_headers
                        .keys()
                        .any(|k| k.eq_ignore_ascii_case(&name))
                })
                .and_then(|e| op_id_for(e, op_ids));
            out.push(Observation {
                kind: "header_sometimes_present".into(),
                detail: format!(
                    "Request header `{name}` on {count}/{n} call(s) — may be conditional"
                ),
                evidence_count: Some(count),
                op_id: sample_op,
            });
        }
    }
    out
}

fn status_patterns(
    entries: &[&ClassifiedEntry],
    op_ids: &HashMap<String, String>,
) -> Vec<Observation> {
    let mut by_status: BTreeMap<u16, usize> = BTreeMap::new();
    for e in entries {
        if let Some(s) = e.entry.flow.status {
            *by_status.entry(s).or_insert(0) += 1;
        }
    }
    let mut out = Vec::new();
    if by_status.len() > 1 {
        let summary: Vec<_> = by_status.iter().map(|(s, c)| format!("{s}×{c}")).collect();
        out.push(Observation {
            kind: "mixed_status_codes".into(),
            detail: format!("Observed status codes: {}", summary.join(", ")),
            evidence_count: Some(entries.len()),
            op_id: None,
        });
    }
    // Empty body on success
    for e in entries {
        if e.entry.flow.status == Some(200)
            && e.entry.text_response().is_some_and(|t| t.trim().is_empty())
        {
            out.push(Observation {
                kind: "empty_success_body".into(),
                detail: "HTTP 200 with empty response body observed".into(),
                evidence_count: Some(1),
                op_id: op_id_for(e, op_ids),
            });
            break;
        }
    }
    out
}

fn query_param_variety(entries: &[&ClassifiedEntry]) -> Vec<Observation> {
    // Report variety, not how to paginate. Constants only when seen on ≥2 calls.
    let mut names: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut call_counts: BTreeMap<String, usize> = BTreeMap::new();
    for e in entries {
        let Ok(url) = Url::parse(&e.entry.flow.url) else {
            continue;
        };
        let mut seen_here = BTreeSet::new();
        for (k, v) in url.query_pairs() {
            let key = k.to_string();
            names.entry(key.clone()).or_default().insert(v.to_string());
            if seen_here.insert(key.clone()) {
                *call_counts.entry(key).or_insert(0) += 1;
            }
        }
    }
    let mut out = Vec::new();
    for (name, values) in names {
        if values.len() > 1 {
            let preview: Vec<_> = values.iter().take(5).cloned().collect();
            out.push(Observation {
                kind: "query_param_varied".into(),
                detail: format!(
                    "Query param `{name}` took {} distinct value(s) in-session (e.g. {})",
                    values.len(),
                    preview.join(", ")
                ),
                evidence_count: Some(values.len()),
                op_id: None,
            });
        } else if values.len() == 1 {
            let calls = call_counts.get(&name).copied().unwrap_or(0);
            if calls < 2 {
                continue;
            }
            let name_l = name.to_ascii_lowercase();
            if matches!(
                name_l.as_str(),
                "_" | "t" | "cb" | "cachebust" | "timestamp"
            ) {
                continue;
            }
            out.push(Observation {
                kind: "query_param_constant".into(),
                detail: format!(
                    "Query param `{name}` constant across {calls} observed call(s) in this surface"
                ),
                evidence_count: Some(calls),
                op_id: None,
            });
        }
    }
    out
}

fn body_field_presence(
    entries: &[&ClassifiedEntry],
    op_ids: &HashMap<String, String>,
) -> Vec<Observation> {
    let mut field_counts: BTreeMap<String, usize> = BTreeMap::new();
    let mut json_calls = 0usize;
    for e in entries {
        let Some(body) = e.entry.text_request() else {
            continue;
        };
        let Ok(json) = serde_json::from_str::<Value>(body) else {
            continue;
        };
        json_calls += 1;
        let mut keys = BTreeSet::new();
        collect_top_keys(&json, &mut keys);
        for k in keys {
            *field_counts.entry(k).or_insert(0) += 1;
        }
    }
    if json_calls < 2 {
        return Vec::new();
    }
    let mut out = Vec::new();
    for (field, count) in field_counts {
        if count == json_calls {
            out.push(Observation {
                kind: "request_field_always_present".into(),
                detail: format!(
                    "JSON request field `{field}` present on all {json_calls} parsed request(s)"
                ),
                evidence_count: Some(count),
                op_id: None,
            });
        } else if count > 0 && count < json_calls {
            out.push(Observation {
                kind: "request_field_sometimes_present".into(),
                detail: format!(
                    "JSON request field `{field}` on {count}/{json_calls} parsed request(s)"
                ),
                evidence_count: Some(count),
                op_id: entries
                    .iter()
                    .find(|e| {
                        e.entry
                            .text_request()
                            .and_then(|b| serde_json::from_str::<Value>(b).ok())
                            .is_some_and(|j| {
                                let mut keys = BTreeSet::new();
                                collect_top_keys(&j, &mut keys);
                                keys.contains(&field)
                            })
                    })
                    .and_then(|e| op_id_for(e, op_ids)),
            });
        }
    }
    out
}

fn collect_top_keys(json: &Value, out: &mut BTreeSet<String>) {
    match json {
        Value::Object(map) => {
            for k in map.keys() {
                out.insert(k.clone());
            }
            // Also one level into common envelopes
            for nest in ["params", "variables", "data", "input"] {
                if let Some(Value::Object(inner)) = map.get(nest) {
                    for k in inner.keys() {
                        out.insert(format!("{nest}.{k}"));
                    }
                }
            }
        }
        Value::Array(arr) => {
            if let Some(first) = arr.first() {
                collect_top_keys(first, out);
            }
        }
        _ => {}
    }
}

fn content_type_notes(entries: &[&ClassifiedEntry]) -> Vec<Observation> {
    let mut req: BTreeSet<String> = BTreeSet::new();
    let mut resp: BTreeSet<String> = BTreeSet::new();
    for e in entries {
        if let Some(ct) = e.entry.request_content_type() {
            req.insert(ct.split(';').next().unwrap_or(ct).trim().to_string());
        }
        if let Some(ct) = e.entry.response_content_type() {
            resp.insert(ct.split(';').next().unwrap_or(ct).trim().to_string());
        }
    }
    let mut out = Vec::new();
    if req.len() > 1 {
        out.push(Observation {
            kind: "mixed_request_content_types".into(),
            detail: format!(
                "Multiple request Content-Types observed: {}",
                req.into_iter().collect::<Vec<_>>().join(", ")
            ),
            evidence_count: None,
            op_id: None,
        });
    }
    if resp.len() > 1 {
        out.push(Observation {
            kind: "mixed_response_content_types".into(),
            detail: format!(
                "Multiple response Content-Types observed: {}",
                resp.into_iter().collect::<Vec<_>>().join(", ")
            ),
            evidence_count: None,
            op_id: None,
        });
    }
    out
}

fn binary_or_empty_bodies(
    entries: &[&ClassifiedEntry],
    op_ids: &HashMap<String, String>,
) -> Vec<Observation> {
    let mut out = Vec::new();
    for e in entries {
        if e.entry.flow.has_binary_payload() {
            out.push(Observation {
                kind: "binary_body_undecoded".into(),
                detail: "Binary body retained; fields not decoded".into(),
                evidence_count: Some(1),
                op_id: op_id_for(e, op_ids),
            });
        }
    }
    // Dedupe by kind+op
    let mut seen = BTreeSet::new();
    out.retain(|o| seen.insert((o.kind.clone(), o.op_id.clone())));
    out
}

fn sequence_clustering(entries: &[&ClassifiedEntry]) -> Vec<Observation> {
    // Only flag tight, same-path bursts — interactive UIs (translate typing) have
    // varied paths / larger gaps and must not be labeled as polling.
    if entries.len() < 8 {
        return Vec::new();
    }
    let mut by_path: BTreeMap<&str, Vec<u64>> = BTreeMap::new();
    for e in entries {
        by_path
            .entry(e.entry.path.as_str())
            .or_default()
            .push(e.entry.flow.sequence);
    }
    let mut out = Vec::new();
    for (path, mut seqs) in by_path {
        if seqs.len() < 8 {
            continue;
        }
        seqs.sort_unstable();
        let gaps: Vec<u64> = seqs
            .windows(2)
            .filter_map(|w| w[1].checked_sub(w[0]))
            .collect();
        if gaps.len() < 7 {
            continue;
        }
        let avg = gaps.iter().sum::<u64>() as f64 / gaps.len() as f64;
        let tight = gaps.iter().filter(|g| **g <= 2).count();
        if avg <= 1.5 && tight * 100 / gaps.len() >= 70 {
            out.push(Observation {
                kind: "high_frequency_calls".into(),
                detail: format!(
                    "{} calls to `{path}` with very small sequence gaps (avg {:.1}) — possible polling",
                    seqs.len(),
                    avg
                ),
                evidence_count: Some(seqs.len()),
                op_id: None,
            });
        }
    }
    out
}

fn response_catalog_keys(
    entries: &[&ClassifiedEntry],
    op_ids: &HashMap<String, String>,
) -> Vec<Observation> {
    // If a response contains object maps that look like catalogs, record keys — no usage advice.
    let mut best: BTreeMap<String, (Observation, usize)> = BTreeMap::new();
    for e in entries {
        let Some(body) = e.entry.text_response() else {
            continue;
        };
        let Ok(json) = serde_json::from_str::<Value>(body) else {
            continue;
        };
        for catalog_key in ["facets", "aggregations", "filters", "options", "enums"] {
            if let Some(obj) = json.get(catalog_key).and_then(|v| v.as_object()) {
                let keys: Vec<_> = obj.keys().take(20).cloned().collect();
                if keys.is_empty() {
                    continue;
                }
                let entry = best.entry(catalog_key.to_string()).or_insert_with(|| {
                    (
                        Observation {
                            kind: "response_catalog".into(),
                            detail: format!(
                                "Response contains `{catalog_key}` with keys: {}",
                                keys.join(", ")
                            ),
                            evidence_count: Some(1),
                            op_id: op_id_for(e, op_ids),
                        },
                        1,
                    )
                });
                entry.1 += 1;
                entry.0.evidence_count = Some(entry.1);
            }
        }
    }
    best.into_values().map(|(o, _)| o).collect()
}
