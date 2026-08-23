pub mod api_origins;
pub mod coverage;
pub mod filters;
pub mod form;
pub mod graphql;
pub mod grpc;
pub mod jsonrpc;
pub mod llm;
pub mod rest;
pub mod websocket;

pub use coverage::CoverageReport;

use crate::ir::TrafficEntry;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Protocol {
    Rest,
    Graphql,
    JsonRpc,
    WebSocket,
    GrpcWeb,
    Protobuf,
    Form,
    Noise,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Confidence {
    Parser,
    Llm,
}

#[derive(Debug, Clone)]
pub struct ClassifiedEntry {
    pub entry: TrafficEntry,
    pub protocol: Protocol,
    pub confidence: Confidence,
    pub operation_name: Option<String>,
}

/// Classify order (fixed):
/// filter → GraphQL → JSON-RPC → gRPC/protobuf → form → REST (+ tRPC label)
/// → WebSocket → LLM (text ambiguous only).
pub async fn classify_entries(entries: Vec<TrafficEntry>) -> Vec<ClassifiedEntry> {
    let ignore = filters::IgnoreRegistry::load();
    let mut out = Vec::new();
    let mut unknown_batch = Vec::new();

    for entry in entries {
        if filters::should_ignore(&entry, &ignore) {
            continue;
        }

        if entry.is_websocket() {
            if let Some(msg) = websocket::try_parse_message(&entry) {
                out.push(ClassifiedEntry {
                    entry,
                    protocol: Protocol::WebSocket,
                    confidence: Confidence::Parser,
                    operation_name: Some(msg),
                });
            } else {
                // Keep undecoded WS as websocket — do not spend LLM budget on binary frames.
                out.push(ClassifiedEntry {
                    entry,
                    protocol: Protocol::WebSocket,
                    confidence: Confidence::Parser,
                    operation_name: Some("undecoded".into()),
                });
            }
            continue;
        }

        if let Some(op) = graphql::try_parse_operation(&entry) {
            out.push(ClassifiedEntry {
                entry,
                protocol: Protocol::Graphql,
                confidence: Confidence::Parser,
                operation_name: Some(op),
            });
            continue;
        }

        if let Some(methods) = jsonrpc::try_parse_methods(&entry) {
            for method in methods {
                out.push(ClassifiedEntry {
                    entry: entry.clone(),
                    protocol: Protocol::JsonRpc,
                    confidence: Confidence::Parser,
                    operation_name: Some(method),
                });
            }
            continue;
        }

        if let Some(proto) = grpc::try_detect(&entry) {
            out.push(ClassifiedEntry {
                entry,
                protocol: proto,
                confidence: Confidence::Parser,
                operation_name: None,
            });
            continue;
        }

        if let Some(label) = form::try_parse_form(&entry) {
            out.push(ClassifiedEntry {
                entry,
                protocol: Protocol::Form,
                confidence: Confidence::Parser,
                operation_name: Some(label),
            });
            continue;
        }

        if rest::looks_like_rest(&entry) {
            let operation_name = rest::trpc_operation_name(&entry);
            out.push(ClassifiedEntry {
                entry,
                protocol: Protocol::Rest,
                confidence: Confidence::Parser,
                operation_name,
            });
            continue;
        }

        // Skip pure binary leftovers for LLM.
        if entry.flow.has_binary_payload() && entry.text_request().is_none() {
            continue;
        }

        unknown_batch.push(entry);
    }

    if !unknown_batch.is_empty() {
        let llm_results = llm::classify_batch(&unknown_batch).await;
        for (entry, protocol) in unknown_batch.into_iter().zip(llm_results) {
            if protocol == Protocol::Noise {
                continue;
            }
            let operation_name = match protocol {
                Protocol::Graphql => graphql::try_parse_operation(&entry),
                Protocol::JsonRpc => jsonrpc::try_parse_method(&entry),
                Protocol::Form => form::try_parse_form(&entry),
                Protocol::WebSocket => websocket::try_parse_message(&entry),
                Protocol::Rest => rest::trpc_operation_name(&entry),
                _ => None,
            };
            out.push(ClassifiedEntry {
                entry,
                protocol,
                confidence: Confidence::Llm,
                operation_name,
            });
        }
    }

    out
}

pub fn protocol_str(p: Protocol) -> &'static str {
    match p {
        Protocol::Rest => "rest",
        Protocol::Graphql => "graphql",
        Protocol::JsonRpc => "jsonrpc",
        Protocol::WebSocket => "websocket",
        Protocol::GrpcWeb => "grpcweb",
        Protocol::Protobuf => "protobuf",
        Protocol::Form => "form",
        Protocol::Noise => "noise",
    }
}
