pub mod graphql;
pub mod rest;

use crate::classify::{ClassifiedEntry, Confidence, Protocol};
use crate::types::Candidate;

pub fn discover_candidates(classified: &[ClassifiedEntry]) -> Vec<Candidate> {
    let mut candidates = Vec::new();
    candidates.extend(rest::discover(classified));
    candidates.extend(graphql::discover(classified));
    candidates.sort_by(compare_candidates);
    candidates
}

fn compare_candidates(a: &Candidate, b: &Candidate) -> std::cmp::Ordering {
    b.request_count
        .cmp(&a.request_count)
        .then_with(|| confidence_rank(&b.confidence).cmp(&confidence_rank(&a.confidence)))
        .then_with(|| a.protocol.cmp(&b.protocol))
        .then_with(|| a.label.cmp(&b.label))
}

fn confidence_rank(confidence: &str) -> u8 {
    match confidence {
        "parser" => 3,
        "inferred" => 2,
        "llm" => 1,
        _ => 0,
    }
}

pub fn confidence_str(c: Confidence) -> &'static str {
    match c {
        Confidence::Parser => "parser",
        Confidence::Inferred => "inferred",
        Confidence::Llm => "llm",
    }
}

pub fn protocol_str(p: Protocol) -> &'static str {
    match p {
        Protocol::Rest => "rest",
        Protocol::Graphql => "graphql",
        Protocol::Unknown => "unknown",
        Protocol::Noise => "noise",
    }
}
