//! Golden-file pack from a stored session artifact — not a live recording.
//!
//! Asserts on structure, stable ids, no credential leakage, and no speculative
//! prose. Does not snapshot whole YAML blobs.

use std::io::Read;

use spoor::export;
use spoor::pipeline;
use spoor::session;
use spoor::types::{GenerateRequest, GenerateSelection};

const BEARER: &str = "tok_live_abcdefghijklmnopqrstuvwxyz012345";

const EXPECTED_OP_IDS: &[&str] = &[
    "rest|https://api.example.test|GET|/v1/me",
    "rest|https://api.example.test|GET|/v1/orders",
    "rest|https://api.example.test|POST|/oauth/token",
];

#[tokio::test]
async fn stored_session_pack_structure_is_stable() {
    pipeline::disable_llm();

    let loaded =
        session::load_session_dir(std::path::Path::new("tests/fixtures/sessions/auth_bearer"))
            .expect("load stored session fixture");
    assert_eq!(loaded.meta.id, "fixture-auth-bearer");
    assert_eq!(loaded.flows.len(), 3);
    assert_eq!(loaded.flows[0].sequence, 0);
    assert_eq!(loaded.meta.pages.len(), 1);

    let analysis = pipeline::analyze(&loaded.flows, loaded.meta.flows_capped).await;
    let mut got_ids: Vec<_> = analysis.candidates.iter().map(|c| c.id.clone()).collect();
    got_ids.sort();
    let mut expected: Vec<_> = EXPECTED_OP_IDS.iter().map(|s| (*s).to_string()).collect();
    expected.sort();
    assert_eq!(got_ids, expected, "op ids must stay stable across replays");

    let selected: Vec<GenerateSelection> = analysis
        .candidates
        .iter()
        .map(|c| GenerateSelection {
            id: c.id.clone(),
            pattern: Some(c.guessed_pattern.clone()),
        })
        .collect();
    let req = GenerateRequest {
        origin: None,
        selected,
        ignore_patterns: vec![],
        redact: false,
    };
    let result = export::generate_bundle_with_coverage(
        &analysis.classified,
        &analysis.candidates,
        &req,
        &loaded.flows,
        &analysis.coverage,
        &loaded.meta.pages,
    )
    .expect("bundle");

    let cursor = std::io::Cursor::new(result.bundle.zip_bytes);
    let mut archive = zip::ZipArchive::new(cursor).unwrap();
    let names: Vec<String> = (0..archive.len())
        .map(|i| archive.by_index(i).unwrap().name().to_string())
        .collect();

    assert!(names.iter().any(|n| n == "MANIFEST.yaml"), "{names:?}");
    assert!(names.iter().any(|n| n == "relations.yaml"), "{names:?}");
    assert!(
        names
            .iter()
            .any(|n| n == "surfaces/api-example-test_rest/surface.yaml"),
        "{names:?}"
    );
    assert!(
        names
            .iter()
            .any(|n| n == "surfaces/api-example-test_rest/auth.yaml"),
        "{names:?}"
    );
    assert!(
        names
            .iter()
            .any(|n| n.starts_with("surfaces/api-example-test_rest/ops/") && n.ends_with(".yaml")),
        "{names:?}"
    );
    assert!(
        !names.iter().any(|n| n.starts_with("integration-brief-")),
        "legacy briefs must not appear"
    );

    let mut blob = String::new();
    for i in 0..archive.len() {
        let mut f = archive.by_index(i).unwrap();
        let mut body = String::new();
        f.read_to_string(&mut body).unwrap();
        blob.push_str(&format!("# {}\n{body}\n", f.name()));
    }

    assert!(blob.contains("spoor_version: 3"));
    assert!(blob.contains("purpose: agent_integration"));
    assert!(blob.contains("read_order:"));
    assert!(blob.contains("protocol: rest"));
    for id in EXPECTED_OP_IDS {
        assert!(blob.contains(id), "missing op id {id} in pack");
    }

    assert!(
        !blob.contains(BEARER),
        "raw bearer token leaked into pack:\n{blob}"
    );
    assert!(!blob.contains("how-to"), "speculative prose:\n{blob}");
    assert!(
        !blob.contains("Pagination: increment"),
        "speculative pagination:\n{blob}"
    );
    assert!(
        !blob.contains("For search APIs:"),
        "speculative search advice:\n{blob}"
    );
    assert!(
        !blob.contains("you can paginate"),
        "speculative pagination:\n{blob}"
    );
    assert!(
        !blob.contains("how to log in"),
        "speculative auth prose:\n{blob}"
    );
}

#[tokio::test]
async fn analyze_is_pure_over_flows() {
    pipeline::disable_llm();
    let loaded =
        session::load_session_dir(std::path::Path::new("tests/fixtures/sessions/auth_bearer"))
            .unwrap();
    let a = pipeline::analyze(&loaded.flows, false).await;
    let b = pipeline::analyze(&loaded.flows, false).await;
    let ids = |r: &pipeline::AnalysisResult| {
        r.candidates
            .iter()
            .map(|c| c.id.clone())
            .collect::<Vec<_>>()
    };
    assert_eq!(ids(&a), ids(&b));
    assert_eq!(a.classified.len(), b.classified.len());
}
