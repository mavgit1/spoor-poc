//! Agent pack v3 — observation-first, protocol-agnostic export.
//!
//! Layout:
//!   MANIFEST.yaml
//!   relations.yaml
//!   surfaces/{surface_id}/surface.yaml
//!   surfaces/{surface_id}/operations.yaml
//!   surfaces/{surface_id}/observations.yaml
//!   surfaces/{surface_id}/ops/{op_slug}.yaml

use std::collections::{BTreeMap, HashMap, HashSet};

use serde::Serialize;
use serde_json::Value;

use crate::capture::CaptureRecord;
use crate::classify::{ClassifiedEntry, CoverageReport, Protocol};
use crate::export::auth::{self, AuthObservation};
use crate::export::example_pick;
use crate::export::facets::{self, FilterParamCatalog};
use crate::export::observations::{self, Observation};
use crate::export::query_params::{self, QueryParamObservation};
use crate::export::session;
use crate::export::trim;
use crate::path;
use crate::redact::Redactor;
use crate::types::{BrowsingPage, Candidate, GenerateRequest};

// Redaction fields/patterns live in `crate::redact`.

#[derive(Serialize)]
struct Manifest {
    spoor_version: u32,
    purpose: String,
    selection_mode: String,
    coverage: CoverageReport,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    browser_pages: Vec<BrowsingPage>,
    surfaces: Vec<ManifestSurface>,
    read_order: Vec<String>,
    /// Facts only — no how-to advice.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    limitations: Vec<String>,
}

#[derive(Serialize)]
struct ManifestSurface {
    id: String,
    origin: String,
    protocol: String,
    path: String,
    operation_count: usize,
    why_included: String,
    /// Relative to main-frame browsing domains captured in-session.
    #[serde(skip_serializing_if = "Option::is_none")]
    browsing_affinity: Option<String>,
}

#[derive(Serialize)]
struct SurfaceDoc {
    id: String,
    origin: String,
    protocol: String,
    /// Observed addressing from traffic — not an invented contract.
    addressing: Addressing,
    auth: Vec<AuthObservation>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    common_query_params: Vec<QueryParamObservation>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    filter_catalog: Vec<FilterParamCatalog>,
    coverage: CoverageReport,
}

#[derive(Serialize)]
struct Addressing {
    #[serde(skip_serializing_if = "Option::is_none")]
    sample_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    sample_method: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    content_type: Option<String>,
    /// Shape inferred from an observed example body only (absent if unknown).
    #[serde(skip_serializing_if = "Option::is_none")]
    observed_request_shape: Option<String>,
}

#[derive(Serialize)]
struct OperationsIndex {
    surface_id: String,
    operations: Vec<OpIndexEntry>,
}

#[derive(Serialize)]
struct OpIndexEntry {
    id: String,
    file: String,
    label: String,
    request_count: usize,
}

#[derive(Serialize)]
struct OpDoc {
    id: String,
    label: String,
    protocol: String,
    request_count: usize,
    addressing: OpAddressing,
    #[serde(skip_serializing_if = "Option::is_none")]
    example_request: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    example_response: Option<Value>,
    /// Sidecar with fuller payload when inline trim drops material structure.
    #[serde(skip_serializing_if = "Option::is_none")]
    example_request_ref: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    example_response_ref: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    observed_response_shape: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    observations: Vec<Observation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    sequence_range: Option<[u64; 2]>,
}

#[derive(Serialize)]
struct OpAddressing {
    #[serde(skip_serializing_if = "Option::is_none")]
    method: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    rpc_method: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    message_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    path_pattern: Option<String>,
}

#[derive(Serialize)]
struct RelationsDoc {
    spoor_version: u32,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    browser_pages: Vec<BrowsingPage>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    depends_on: Vec<RelationEdge>,
}

#[derive(Serialize)]
struct RelationEdge {
    from_op_id: String,
    to_op_id: String,
    via: String,
    /// How this was inferred — always evidence-based.
    basis: String,
}

struct SelectedOp {
    candidate: Candidate,
    pattern: String,
    protocol: String,
}

fn browsing_affinity(origin: &str, browsing_domains: &[String]) -> Option<String> {
    if browsing_domains.is_empty() {
        return None;
    }
    let host = origin
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .trim_start_matches("wss://")
        .trim_start_matches("ws://")
        .split('/')
        .next()
        .unwrap_or(origin)
        .split(':')
        .next()
        .unwrap_or(origin);
    if browsing_domains
        .iter()
        .any(|d| host == d || host.ends_with(&format!(".{d}")))
    {
        Some("primary_product".into())
    } else {
        Some("other_tab_or_third_party".into())
    }
}

pub fn build_pack_files(
    classified: &[ClassifiedEntry],
    candidates: &[Candidate],
    req: &GenerateRequest,
    flows: &[CaptureRecord],
    coverage: &CoverageReport,
    page_urls: &[BrowsingPage],
) -> anyhow::Result<Vec<(String, String)>> {
    let redact = req.redact;
    let selected = resolve_selection(candidates, req);
    if selected.is_empty() {
        anyhow::bail!("no export artifacts produced for selection");
    }

    let selection_mode = if selected.len() <= 3 {
        "minimal"
    } else {
        "rich"
    };

    let browsing_domains: Vec<String> = {
        let mut d: Vec<_> = page_urls
            .iter()
            .map(|p| p.domain.clone())
            .filter(|s| !s.is_empty())
            .collect();
        d.sort();
        d.dedup();
        d
    };

    // Group by (origin, protocol) → surface
    let mut by_surface: BTreeMap<(String, String), Vec<SelectedOp>> = BTreeMap::new();
    for op in selected {
        by_surface
            .entry((op.candidate.origin.clone(), op.protocol.clone()))
            .or_default()
            .push(op);
    }

    let mut files: Vec<(String, String)> = Vec::new();
    let mut manifest_surfaces = Vec::new();
    let mut read_order = vec!["MANIFEST.yaml".into(), "relations.yaml".into()];
    let mut all_relation_edges = Vec::new();
    let mut limitations = coverage_limitations(coverage);
    let mut selected_ops_flat: Vec<SelectedOp> = Vec::new();

    // Map flow id → op id for observations
    let mut flow_to_op: HashMap<String, String> = HashMap::new();

    for ((origin, protocol), ops) in &by_surface {
        let surface_id = surface_id(origin, protocol);
        let surface_dir = format!("surfaces/{surface_id}");

        let proto_enum = protocol_to_enum(protocol);
        let surface_entries: Vec<&ClassifiedEntry> = classified
            .iter()
            .filter(|c| c.entry.origin == *origin && matches_protocol(c.protocol, proto_enum))
            .filter(|c| entry_matches_any_op(c, ops, protocol))
            .collect();

        for op in ops {
            selected_ops_flat.push(SelectedOp {
                candidate: op.candidate.clone(),
                pattern: op.pattern.clone(),
                protocol: op.protocol.clone(),
            });
            for e in classified.iter().filter(|c| {
                c.entry.origin == *origin
                    && matches_protocol(c.protocol, proto_enum)
                    && entry_matches_op(c, op, protocol)
            }) {
                flow_to_op.insert(e.entry.flow.id.clone(), op.candidate.id.clone());
            }
        }

        let cov = if flows.is_empty() {
            coverage.clone()
        } else {
            session::coverage_for_origin(flows, classified, origin, coverage.flows_capped)
        };

        let auth = {
            let mut auth = auth::observe_for_origin(classified, origin);
            if redact {
                for a in &mut auth {
                    if let Some(ex) = a.example.as_mut() {
                        *ex = "[REDACTED]".into();
                    }
                }
            }
            auth
        };
        let common_query_params = query_params::observe_for_origin(classified, origin);
        let filter_catalog = facets::extract_filter_catalog(classified, origin);
        let addressing = build_addressing(&surface_entries, protocol);
        let surface_obs = observations::for_entries(&surface_entries, &flow_to_op);

        let surface_doc = SurfaceDoc {
            id: surface_id.clone(),
            origin: origin.clone(),
            protocol: protocol.clone(),
            addressing,
            auth,
            common_query_params,
            filter_catalog,
            coverage: cov,
        };
        files.push((
            format!("{surface_dir}/surface.yaml"),
            serde_yaml_ng::to_string(&surface_doc)?,
        ));

        let mut index_ops = Vec::new();
        let mut op_read_paths = Vec::new();
        for op in ops {
            let matching: Vec<&ClassifiedEntry> = classified
                .iter()
                .filter(|c| {
                    c.entry.origin == *origin
                        && matches_protocol(c.protocol, proto_enum)
                        && entry_matches_op(c, op, protocol)
                })
                .collect();

            let slug = op_file_slug(&op.candidate.id);
            let file = format!("ops/{slug}.yaml");
            let (op_doc, sidecars) =
                build_op_doc(op, &matching, protocol, redact, &flow_to_op, &slug)?;
            op_read_paths.push(format!("{surface_dir}/{file}"));
            if let Some(ref r) = op_doc.example_request_ref {
                op_read_paths.push(format!("{surface_dir}/{r}"));
            }
            if let Some(ref r) = op_doc.example_response_ref {
                op_read_paths.push(format!("{surface_dir}/{r}"));
            }
            for (name, body) in sidecars {
                files.push((format!("{surface_dir}/{name}"), body));
            }
            files.push((
                format!("{surface_dir}/{file}"),
                serde_yaml_ng::to_string(&op_doc)?,
            ));
            index_ops.push(OpIndexEntry {
                id: op.candidate.id.clone(),
                file,
                label: op.candidate.label.clone(),
                request_count: op.candidate.request_count.max(matching.len()),
            });
        }

        files.push((
            format!("{surface_dir}/operations.yaml"),
            serde_yaml_ng::to_string(&OperationsIndex {
                surface_id: surface_id.clone(),
                operations: index_ops,
            })?,
        ));
        files.push((
            format!("{surface_dir}/observations.yaml"),
            serde_yaml_ng::to_string(&surface_obs)?,
        ));

        read_order.push(format!("{surface_dir}/surface.yaml"));
        read_order.push(format!("{surface_dir}/operations.yaml"));
        read_order.push(format!("{surface_dir}/observations.yaml"));
        read_order.extend(op_read_paths);

        manifest_surfaces.push(ManifestSurface {
            id: surface_id,
            origin: origin.clone(),
            protocol: protocol.clone(),
            path: format!("{surface_dir}/"),
            operation_count: ops.len(),
            why_included: "user_selected".into(),
            browsing_affinity: browsing_affinity(origin, &browsing_domains),
        });
    }

    // Session-wide id handoffs (same- and cross-origin / http↔ws).
    let selected_patterns: HashSet<String> = selected_ops_flat
        .iter()
        .map(|o| o.pattern.clone())
        .collect();
    let edges = session::infer_depends_on_session(classified, &selected_patterns);
    for edge in edges {
        let from_id = selected_ops_flat
            .iter()
            .find(|o| label_matches_op(&edge.from, o))
            .map(|o| o.candidate.id.clone());
        let to_id = selected_ops_flat
            .iter()
            .find(|o| label_matches_op(&edge.to, o))
            .map(|o| o.candidate.id.clone());
        if let (Some(from_op_id), Some(to_op_id)) = (from_id, to_id) {
            if from_op_id == to_op_id {
                continue;
            }
            all_relation_edges.push(RelationEdge {
                from_op_id,
                to_op_id,
                via: edge.via,
                basis: if edge.cross_origin {
                    "shared_id_like_value_across_origins_or_schemes".into()
                } else {
                    "shared_id_like_value_seen_in_earlier_response_then_later_request".into()
                },
            });
        }
    }

    // Prefer primary-product surfaces first in the agent read path.
    manifest_surfaces.sort_by(|a, b| {
        let rank = |s: &ManifestSurface| match s.browsing_affinity.as_deref() {
            Some("primary_product") => 0,
            Some(_) => 1,
            None => 2,
        };
        rank(a)
            .cmp(&rank(b))
            .then_with(|| b.operation_count.cmp(&a.operation_count))
            .then_with(|| a.id.cmp(&b.id))
    });

    // Rebuild read_order: manifest/relations, then surfaces in sorted affinity order.
    let surface_dirs: Vec<String> = manifest_surfaces.iter().map(|s| s.path.clone()).collect();
    let mut new_read_order = vec!["MANIFEST.yaml".into(), "relations.yaml".into()];
    for dir in &surface_dirs {
        let prefix = dir.trim_end_matches('/');
        for path in &read_order {
            if path.starts_with(prefix) && !new_read_order.contains(path) {
                new_read_order.push(path.clone());
            }
        }
    }
    for path in &read_order {
        if !new_read_order.contains(path) {
            new_read_order.push(path.clone());
        }
    }
    let read_order = new_read_order;

    if coverage.is_partial() {
        limitations.push(
            "Session coverage is partial (binary/WS/gRPC undecoded and/or flow cap). Selected ops are still valid evidence."
                .into(),
        );
    }
    if !page_urls.is_empty() {
        limitations.push(format!(
            "Browser navigated {} main-frame page(s) during capture — use browser_pages for product context",
            page_urls.len()
        ));
    }
    if !redact {
        limitations.push(
            "Examples are not redacted (panel option off) — treat pack as sensitive evidence"
                .into(),
        );
    }

    let manifest = Manifest {
        spoor_version: 3,
        purpose: "agent_integration".into(),
        selection_mode: selection_mode.into(),
        coverage: coverage.clone(),
        browser_pages: page_urls.to_vec(),
        surfaces: manifest_surfaces,
        read_order,
        limitations,
    };
    files.insert(
        0,
        ("MANIFEST.yaml".into(), serde_yaml_ng::to_string(&manifest)?),
    );
    files.insert(
        1,
        (
            "relations.yaml".into(),
            serde_yaml_ng::to_string(&RelationsDoc {
                spoor_version: 3,
                browser_pages: page_urls.to_vec(),
                depends_on: all_relation_edges,
            })?,
        ),
    );

    Ok(files)
}

fn resolve_selection(candidates: &[Candidate], req: &GenerateRequest) -> Vec<SelectedOp> {
    let mut out = Vec::new();
    for sel in &req.selected {
        let Some(cand) = candidates.iter().find(|c| c.id == sel.id) else {
            continue;
        };
        if let Some(filter) = &req.origin {
            if &cand.origin != filter {
                continue;
            }
        }
        let pattern = sel
            .pattern
            .clone()
            .filter(|p| !p.is_empty())
            .unwrap_or_else(|| cand.guessed_pattern.clone());
        let protocol = protocol_from_candidate_id(&cand.id).unwrap_or(cand.protocol.as_str());
        out.push(SelectedOp {
            candidate: cand.clone(),
            pattern,
            protocol: protocol.to_string(),
        });
    }
    out
}

fn protocol_from_candidate_id(id: &str) -> Option<&str> {
    let prefix = id.split('|').next()?;
    Some(match prefix {
        "rest" => "rest",
        "graphql" => "graphql",
        "jsonrpc" => "jsonrpc",
        "ws" => "websocket",
        "form" => "form",
        "grpcweb" => "grpcweb",
        "protobuf" => "protobuf",
        other => other,
    })
}

fn protocol_to_enum(protocol: &str) -> Protocol {
    match protocol {
        "graphql" => Protocol::Graphql,
        "jsonrpc" => Protocol::JsonRpc,
        "websocket" => Protocol::WebSocket,
        "form" => Protocol::Form,
        "grpcweb" => Protocol::GrpcWeb,
        "protobuf" => Protocol::Protobuf,
        _ => Protocol::Rest,
    }
}

fn matches_protocol(p: Protocol, want: Protocol) -> bool {
    p == want
}

fn surface_id(origin: &str, protocol: &str) -> String {
    let host = origin
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .trim_start_matches("wss://")
        .trim_start_matches("ws://")
        .replace('.', "-")
        .replace(':', "-");
    format!("{host}_{protocol}")
}

fn op_file_slug(op_id: &str) -> String {
    let mut s: String = op_id
        .chars()
        .map(|c| match c {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '_' => c,
            _ => '_',
        })
        .collect();
    if s.len() > 120 {
        s.truncate(120);
    }
    s
}

fn entry_matches_any_op(entry: &ClassifiedEntry, ops: &[SelectedOp], protocol: &str) -> bool {
    ops.iter().any(|op| entry_matches_op(entry, op, protocol))
}

fn entry_matches_op(entry: &ClassifiedEntry, op: &SelectedOp, protocol: &str) -> bool {
    if !method_compatible(entry, op) {
        return false;
    }
    match protocol {
        "graphql" | "jsonrpc" | "websocket" => {
            entry.operation_name.as_deref() == Some(op.pattern.as_str())
                || entry.operation_name.as_deref() == Some(op.candidate.guessed_pattern.as_str())
        }
        "form" | "rest" | "grpcweb" | "protobuf" => {
            path::path_matches_template(&entry.entry.path, &op.pattern)
                || entry.entry.path == op.pattern
                || entry.operation_name.as_deref() == Some(op.pattern.as_str())
        }
        _ => path::path_matches_template(&entry.entry.path, &op.pattern),
    }
}

fn method_compatible(entry: &ClassifiedEntry, op: &SelectedOp) -> bool {
    if op.candidate.methods.is_empty() {
        return true;
    }
    let m = entry.entry.http_method().to_uppercase();
    if m.is_empty() {
        return true;
    }
    op.candidate
        .methods
        .iter()
        .any(|allowed| allowed.eq_ignore_ascii_case(&m))
}

fn label_matches_op(label: &str, op: &SelectedOp) -> bool {
    label == op.candidate.label
        || label == op.pattern
        || label == op.candidate.guessed_pattern
        || op.candidate.label.contains(label)
        || label.contains(&op.pattern)
}

fn build_addressing(entries: &[&ClassifiedEntry], protocol: &str) -> Addressing {
    let sample = example_pick::richest_entry(entries.iter().copied());
    let Some(item) = sample else {
        return Addressing {
            sample_url: None,
            sample_method: None,
            content_type: None,
            observed_request_shape: None,
        };
    };
    let method = match protocol {
        "websocket" => Some("WS".into()),
        _ => {
            let m = item.entry.http_method();
            if m.is_empty() {
                None
            } else {
                Some(m.to_uppercase())
            }
        }
    };
    let observed_request_shape = item
        .entry
        .text_request()
        .and_then(|b| serde_json::from_str::<Value>(b).ok())
        .map(|v| observed_shape(&v));

    Addressing {
        sample_url: Some(item.entry.flow.url.clone()),
        sample_method: method,
        content_type: item.entry.request_content_type().map(|s| s.to_string()),
        observed_request_shape,
    }
}

fn build_op_doc(
    op: &SelectedOp,
    matching: &[&ClassifiedEntry],
    protocol: &str,
    redact: bool,
    flow_to_op: &HashMap<String, String>,
    slug: &str,
) -> anyhow::Result<(OpDoc, Vec<(String, String)>)> {
    let item = example_pick::richest_entry(matching.iter().copied());
    let mut raw_request = item.and_then(|e| parse_body_value(e.entry.text_request()));
    let mut raw_response = item.and_then(|e| parse_body_value(e.entry.text_response()));
    if redact {
        redact_json(&mut raw_request);
        redact_json(&mut raw_response);
    }

    let mut sidecars = Vec::new();
    let mut example_request_ref = None;
    let mut example_response_ref = None;

    let example_request = if let Some(v) = raw_request.as_ref() {
        let trimmed = trim::trim_json(v);
        if trim::needs_handoff(v, &trimmed) {
            let rel = format!("ops/examples/{slug}_request.json");
            sidecars.push((
                rel.clone(),
                serde_json::to_string_pretty(&trim::handoff_json(v))?,
            ));
            example_request_ref = Some(rel);
        }
        Some(trimmed)
    } else {
        None
    };
    let example_response = if let Some(v) = raw_response.as_ref() {
        let trimmed = trim::trim_json(v);
        if trim::needs_handoff(v, &trimmed) {
            let rel = format!("ops/examples/{slug}_response.json");
            sidecars.push((
                rel.clone(),
                serde_json::to_string_pretty(&trim::handoff_json(v))?,
            ));
            example_response_ref = Some(rel);
        }
        Some(trimmed)
    } else {
        None
    };

    let observed_response_shape = example_response.as_ref().map(observed_shape);

    let addressing = if let Some(item) = item {
        let mut url = item.entry.flow.url.clone();
        if redact {
            url = Redactor::for_agent_pack().redact_url(&url);
        }
        OpAddressing {
            method: {
                let m = item.entry.http_method();
                if protocol == "websocket" {
                    Some("WS".into())
                } else if m.is_empty() {
                    None
                } else {
                    Some(m.to_uppercase())
                }
            },
            url: Some(url),
            path: Some(item.entry.path.clone()),
            rpc_method: if protocol == "jsonrpc" {
                item.operation_name.clone()
            } else {
                None
            },
            message_type: if protocol == "websocket" {
                item.operation_name.clone()
            } else {
                None
            },
            path_pattern: if matches!(protocol, "rest" | "form" | "grpcweb" | "protobuf") {
                Some(op.pattern.clone())
            } else {
                None
            },
        }
    } else {
        OpAddressing {
            method: op.candidate.methods.first().cloned(),
            url: Some(op.candidate.example.clone()),
            path: None,
            rpc_method: if protocol == "jsonrpc" {
                Some(op.pattern.clone())
            } else {
                None
            },
            message_type: if protocol == "websocket" {
                Some(op.pattern.clone())
            } else {
                None
            },
            path_pattern: Some(op.pattern.clone()),
        }
    };

    let seqs: Vec<u64> = matching.iter().map(|e| e.entry.flow.sequence).collect();
    let sequence_range = if seqs.is_empty() {
        None
    } else {
        Some([*seqs.iter().min().unwrap(), *seqs.iter().max().unwrap()])
    };

    let op_obs = observations::for_entries(matching, flow_to_op)
        .into_iter()
        .filter(|o| o.op_id.as_deref() == Some(op.candidate.id.as_str()) || o.op_id.is_none())
        .take(12)
        .collect();

    Ok((
        OpDoc {
            id: op.candidate.id.clone(),
            label: op.candidate.label.clone(),
            protocol: protocol.to_string(),
            request_count: op.candidate.request_count.max(matching.len()),
            addressing,
            example_request,
            example_response,
            example_request_ref,
            example_response_ref,
            observed_response_shape,
            observations: op_obs,
            sequence_range,
        },
        sidecars,
    ))
}

fn parse_body_value(body: Option<&str>) -> Option<Value> {
    let body = body?;
    if let Ok(v) = serde_json::from_str::<Value>(body) {
        return Some(v);
    }
    // Form bodies as object when key=value
    if body.contains('=') && !body.trim_start().starts_with('{') {
        let mut map = serde_json::Map::new();
        for pair in body.split('&') {
            let mut parts = pair.splitn(2, '=');
            let Some(k) = parts.next() else { continue };
            if k.is_empty() {
                continue;
            }
            let v = parts.next().unwrap_or("").to_string();
            map.insert(k.to_string(), Value::String(v));
        }
        if !map.is_empty() {
            return Some(Value::Object(map));
        }
    }
    Some(Value::String(body.to_string()))
}

fn observed_shape(value: &Value) -> String {
    match value {
        Value::Object(map) => {
            let keys: Vec<_> = map.keys().take(16).cloned().collect();
            format!("object{{{}}}", keys.join(","))
        }
        Value::Array(arr) => format!("array[len={}]", arr.len()),
        Value::String(_) => "string".into(),
        Value::Number(_) => "number".into(),
        Value::Bool(_) => "bool".into(),
        Value::Null => "null".into(),
    }
}

fn redact_json(value: &mut Option<Value>) {
    let Some(v) = value.as_mut() else {
        return;
    };
    Redactor::for_agent_pack().redact(v);
}

fn coverage_limitations(coverage: &CoverageReport) -> Vec<String> {
    let mut out = Vec::new();
    if coverage.undecoded_binary > 0 {
        out.push(format!(
            "{} undecoded binary body/bodies retained (no field decode)",
            coverage.undecoded_binary
        ));
    }
    if coverage.grpc_or_protobuf > 0 {
        out.push(format!(
            "{} gRPC-Web/protobuf call(s) detected by content-type only",
            coverage.grpc_or_protobuf
        ));
    }
    if coverage.flows_capped {
        out.push("Capture hit flow cap — later traffic may be missing".into());
    }
    out
}
