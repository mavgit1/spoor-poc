use std::collections::{BTreeMap, BTreeSet, HashSet};

use serde::Serialize;
use serde_json::Value;

use crate::classify::{ClassifiedEntry, Protocol};
use crate::export::auth::{self, AuthObservation};
use crate::export::example_pick;
use crate::export::facets::{self, FilterParamCatalog};
use crate::export::query_params::{self, QueryParamObservation};
use crate::export::trim;
use crate::redact::Redactor;
use crate::types::Candidate;

const REDACT_FIELDS: &[&str] = &[
    "token",
    "access_token",
    "refresh_token",
    "password",
    "secret",
    "authorization",
];

#[derive(Serialize)]
struct IntegrationBrief {
    spoor_version: u32,
    api: ApiMeta,
    transport: Transport,
    auth: Vec<AuthObservation>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    common_query_params: Vec<QueryParamObservation>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    filter_values: Vec<FilterParamCatalog>,
    operations: Vec<BriefOperation>,
    notes: Vec<String>,
    filters_applied: Vec<String>,
}

#[derive(Serialize)]
struct ApiMeta {
    id: String,
    origin: String,
    protocol: String,
}

#[derive(Serialize)]
struct Transport {
    method: String,
    url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    content_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    body_shape: Option<String>,
}

#[derive(Serialize)]
struct BriefOperation {
    name: String,
    method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    path_or_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    example_request: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    example_response: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    response_shape: Option<String>,
}

pub fn generate_brief_yaml(
    classified: &[ClassifiedEntry],
    origin: &str,
    protocol: &str,
    selected_patterns: &[String],
    candidates: &[Candidate],
    redact: bool,
) -> anyhow::Result<String> {
    let selected: HashSet<String> = selected_patterns.iter().cloned().collect();
    let auth = auth::observe_for_origin(classified, origin);
    let common_query_params = query_params::observe_for_origin(classified, origin);
    let filter_values = if protocol == "rest" {
        facets::extract_filter_catalog(classified, origin)
    } else {
        Vec::new()
    };
    let transport = infer_transport(classified, origin, protocol);
    let operations = if protocol == "graphql" {
        graphql_operations(classified, origin, &selected, redact)?
    } else {
        rest_operations(classified, origin, &selected, candidates, redact)?
    };

    let has_filter_values = !filter_values.is_empty();
    let notes = build_notes(
        protocol,
        has_filter_values,
        &common_query_params,
        classified,
        origin,
    );

    let brief = IntegrationBrief {
        spoor_version: 1,
        api: ApiMeta {
            id: host_slug(origin),
            origin: origin.to_string(),
            protocol: protocol.to_string(),
        },
        transport,
        auth,
        common_query_params,
        filter_values,
        operations,
        notes,
        filters_applied: default_filters_applied(),
    };

    Ok(serde_yaml_ng::to_string(&brief)?)
}

fn infer_transport(
    classified: &[ClassifiedEntry],
    origin: &str,
    protocol: &str,
) -> Transport {
    let sample = classified
        .iter()
        .find(|c| c.entry.origin == origin)
        .map(|c| &c.entry.flow);

    let (method, url, content_type) = if protocol == "graphql" {
        let url = classified
            .iter()
            .find(|c| c.protocol == Protocol::Graphql && c.entry.origin == origin)
            .map(|c| c.entry.flow.url.clone())
            .unwrap_or_else(|| origin.to_string());
        (
            "POST".to_string(),
            url,
            Some("application/json".to_string()),
        )
    } else if let Some(flow) = sample {
        (
            flow.method.to_uppercase(),
            origin.to_string(),
            flow
                .request_headers
                .get("content-type")
                .or_else(|| flow.request_headers.get("Content-Type"))
                .cloned(),
        )
    } else {
        ("GET".to_string(), origin.to_string(), None)
    };

    let body_shape = if protocol == "graphql" {
        Some("{ query: string, variables: object, operationName?: string }".to_string())
    } else {
        None
    };

    Transport {
        method,
        url,
        content_type,
        body_shape,
    }
}

fn graphql_operations(
    classified: &[ClassifiedEntry],
    origin: &str,
    selected: &HashSet<String>,
    redact: bool,
) -> anyhow::Result<Vec<BriefOperation>> {
    let mut by_op: BTreeMap<String, Vec<&ClassifiedEntry>> = BTreeMap::new();
    for item in classified
        .iter()
        .filter(|c| c.protocol == Protocol::Graphql && c.entry.origin == origin)
    {
        let name = item
            .operation_name
            .clone()
            .unwrap_or_else(|| "anonymous".to_string());
        if !selected.is_empty() && !selected.contains(&name) {
            continue;
        }
        by_op.entry(name).or_default().push(item);
    }

    Ok(by_op
        .into_iter()
        .filter_map(|(name, entries)| {
            let item = example_pick::richest_entry(entries.into_iter())?;
            Some((name, item))
        })
        .map(|(name, item)| {
            let mut req = parse_json_body(item.entry.flow.request_body.as_ref());
            let mut resp = parse_json_body(item.entry.flow.response_body.as_ref());
            if redact {
                redact_json(&mut req);
                redact_json(&mut resp);
            }
            req = req.map(|v| trim::trim_json(&v));
            resp = resp.map(|v| trim::trim_json(&v));
            BriefOperation {
                name,
                method: item.entry.flow.method.to_uppercase(),
                path_or_url: Some(item.entry.flow.url.clone()),
                example_request: req,
                example_response: resp,
                response_shape: None,
            }
        })
        .collect())
}

fn rest_operations(
    classified: &[ClassifiedEntry],
    origin: &str,
    selected: &HashSet<String>,
    candidates: &[Candidate],
    redact: bool,
) -> anyhow::Result<Vec<BriefOperation>> {
    let mut ops = Vec::new();
    for pattern in selected {
        let cand = candidates
            .iter()
            .find(|c| c.origin == origin && c.guessed_pattern == *pattern);
        let example_entry = example_pick::richest_entry(classified.iter().filter(|c| {
            c.protocol == Protocol::Rest
                && c.entry.origin == origin
                && path_matches_pattern(&c.entry.path, pattern)
        }));

        let (method, url, mut req, mut resp) = if let Some(item) = example_entry {
            (
                item.entry.flow.method.to_uppercase(),
                item.entry.flow.url.clone(),
                parse_json_body(item.entry.flow.request_body.as_ref()),
                parse_json_body(item.entry.flow.response_body.as_ref()),
            )
        } else {
            (
                cand.map(|c| c.methods.first().cloned().unwrap_or_else(|| "GET".to_string()))
                    .unwrap_or_else(|| "GET".to_string()),
                format!("{origin}{pattern}"),
                None,
                None,
            )
        };

        if redact {
            redact_json(&mut req);
            redact_json(&mut resp);
        }
        req = req.map(|v| trim::trim_json(&v));
        resp = resp.map(|v| trim::trim_json(&v));

        let had_facets = resp.as_ref().is_some_and(has_facets);
        if is_aggregations_path(pattern) && had_facets {
            if let Some(r) = resp.take() {
                resp = Some(compact_aggregations_summary(&r));
            }
        }

        let response_shape = if is_aggregations_path(pattern) && had_facets {
            Some("facet catalog — see filter_values".to_string())
        } else {
            resp.as_ref().map(|v| shorthand_shape(v))
        };

        ops.push(BriefOperation {
            name: format!("{method} {pattern}"),
            method,
            path_or_url: Some(url),
            example_request: req,
            example_response: resp,
            response_shape,
        });
    }
    Ok(ops)
}

fn path_matches_pattern(path: &str, pattern: &str) -> bool {
    if path == pattern {
        return true;
    }
    let path_segs: Vec<&str> = path.trim_matches('/').split('/').collect();
    let tmpl_segs: Vec<&str> = pattern.trim_matches('/').split('/').collect();
    if path_segs.len() != tmpl_segs.len() {
        return false;
    }
    path_segs.iter().zip(tmpl_segs.iter()).all(|(p, t)| {
        if t.starts_with('{') && t.ends_with('}') {
            !p.is_empty()
        } else {
            p == t
        }
    })
}

fn parse_json_body(body: Option<&String>) -> Option<Value> {
    body.and_then(|b| serde_json::from_str(b).ok())
}

fn redact_json(value: &mut Option<Value>) {
    let Some(v) = value.as_mut() else {
        return;
    };
    let fields: Vec<String> = REDACT_FIELDS.iter().map(|s| s.to_string()).collect();
    let patterns = vec![r"^eyJ[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+$".to_string()];
    if let Ok(r) = Redactor::new(&patterns, &fields) {
        r.redact(v);
    }
}

fn shorthand_shape(value: &Value) -> String {
    match value {
        Value::Object(map) if map.len() > 12 => {
            format!("object with {} top-level keys", map.len())
        }
        Value::Array(arr) if arr.len() > 8 => format!("array[{}]", arr.len()),
        _ => "json".to_string(),
    }
}

fn is_aggregations_path(pattern: &str) -> bool {
    pattern.contains("aggregations")
}

fn has_facets(value: &Value) -> bool {
    value.get("facets").is_some() || value.get("aggregations").is_some()
}

fn compact_aggregations_summary(value: &Value) -> Value {
    let mut out = serde_json::Map::new();
    if let Some(hits) = value.get("totalHits") {
        out.insert("totalHits".into(), hits.clone());
    }
    if let Some(companies) = value.get("companies") {
        out.insert("companies".into(), companies.clone());
    }
    if let Some(facets) = value.get("facets") {
        let keys: Vec<Value> = facets
            .as_object()
            .map(|m| m.keys().map(|k| Value::String(k.clone())).collect())
            .unwrap_or_default();
        out.insert(
            "facet_params".into(),
            Value::Array(keys),
        );
    }
    if let Some(aggs) = value.get("aggregations") {
        let keys: Vec<Value> = aggs
            .as_object()
            .map(|m| m.keys().map(|k| Value::String(k.clone())).collect())
            .unwrap_or_default();
        out.insert("aggregation_groups".into(), Value::Array(keys));
    }
    out.insert(
        "note".into(),
        Value::String("Full facet keys and values are in filter_values at the top of this brief.".into()),
    );
    Value::Object(out)
}

fn build_notes(
    protocol: &str,
    has_filter_values: bool,
    query_params: &[QueryParamObservation],
    classified: &[ClassifiedEntry],
    origin: &str,
) -> Vec<String> {
    let mut notes = base_notes(protocol, has_filter_values);
    if let Some(page) = query_params.iter().find(|p| p.name == "page") {
        if page.examples.len() > 1 || page.distinct_count.is_some() {
            notes.push(
                "Pagination: increment the page query param with the same request body until a page returns fewer items than size (see common_query_params)."
                    .to_string(),
            );
        }
    }
    if protocol == "graphql" {
        if let Some(dep) = graphql_depends_on_note(classified, origin) {
            notes.push(dep);
        }
    }
    notes
}

fn base_notes(protocol: &str, has_filter_values: bool) -> Vec<String> {
    if protocol == "graphql" {
        vec![
            "Feed this brief to an LLM to generate scripts that call these operations.".to_string(),
            "All operations share the transport URL; use operationName + variables from examples."
                .to_string(),
        ]
    } else {
        let mut notes = vec![
            "REST endpoints inferred from captured traffic; see example URLs and query params in each operation."
                .to_string(),
        ];
        if has_filter_values {
            notes.push(
                "filter_values lists allowed query-param keys seen in facet/aggregation responses (query-dependent)."
                    .to_string(),
            );
            notes.push(
                "For search APIs: call GET /aggregations (or similar) with your query to refresh valid filter values."
                    .to_string(),
            );
        }
        notes
    }
}

fn graphql_depends_on_note(classified: &[ClassifiedEntry], origin: &str) -> Option<String> {
    let mut vars = BTreeSet::new();
    for item in classified
        .iter()
        .filter(|c| c.protocol == Protocol::Graphql && c.entry.origin == origin)
    {
        let Some(body) = item.entry.flow.request_body.as_ref() else {
            continue;
        };
        let Ok(json) = serde_json::from_str::<Value>(body) else {
            continue;
        };
        let Some(obj) = json.get("variables").and_then(|v| v.as_object()) else {
            continue;
        };
        for key in obj.keys() {
            if key.ends_with("Id") || key == "processId" {
                vars.insert(key.clone());
            }
        }
    }
    if vars.is_empty() {
        None
    } else {
        Some(format!(
            "Session state variables seen in captures (may require a prior step): {}.",
            vars.into_iter().collect::<Vec<_>>().join(", ")
        ))
    }
}

fn default_filters_applied() -> Vec<String> {
    vec![
        "Static assets: svg, png, js, css, fonts, map tiles (.pbf)".to_string(),
        "Locale bundles: /translations, /i18n, /locales paths".to_string(),
        "Resource types: Image, Font, Stylesheet, Script, Media".to_string(),
        "Methods: HEAD, OPTIONS skipped".to_string(),
    ]
}

fn host_slug(origin: &str) -> String {
    origin
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .replace('.', "-")
}
