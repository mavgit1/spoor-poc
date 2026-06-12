pub mod api_origins;
pub mod filters;
pub mod path_rules;
pub mod graphql;
pub mod llm;
pub mod rest;

use crate::ir::TrafficEntry;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Protocol {
    Rest,
    Graphql,
    Unknown,
    Noise,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Confidence {
    Parser,
    Inferred,
    Llm,
}

#[derive(Debug, Clone)]
pub struct ClassifiedEntry {
    pub entry: TrafficEntry,
    pub protocol: Protocol,
    pub confidence: Confidence,
    pub operation_name: Option<String>,
}

pub async fn classify_entries(entries: Vec<TrafficEntry>) -> Vec<ClassifiedEntry> {
    let ignore = filters::IgnoreRegistry::load();
    let mut out = Vec::new();
    let mut unknown_batch = Vec::new();

    for entry in entries {
        if filters::should_ignore(&entry, &ignore) {
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
        if rest::looks_like_rest(&entry) {
            out.push(ClassifiedEntry {
                entry,
                protocol: Protocol::Rest,
                confidence: Confidence::Parser,
                operation_name: None,
            });
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
            let operation_name = if protocol == Protocol::Graphql {
                graphql::try_parse_operation(&entry)
            } else {
                None
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
