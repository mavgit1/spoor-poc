use spoor::classify::{self, Protocol};
use spoor::discover;
use spoor::export::brief;
use spoor::ir;
use spoor::types::CapturedFlow;

const REST_ORIGIN: &str = "https://portal.example.test";
const GQL_ORIGIN: &str = "https://api.example.test";

fn load_fixture(name: &str) -> Vec<CapturedFlow> {
    let path = format!("tests/fixtures/{name}.json");
    let raw = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path}: {e}"));
    serde_json::from_str(&raw).unwrap_or_else(|e| panic!("parse {path}: {e}"))
}

#[tokio::test]
async fn microservice_rest_classify_and_discover() {
    let flows = load_fixture("microservice_rest");
    let entries = ir::entries_from_flows(&flows);
    assert_eq!(entries.len(), 3, "all fixture URLs should parse");

    let classified = classify::classify_entries(entries).await;
    assert!(
        classified
            .iter()
            .any(|c| c.protocol == Protocol::Rest),
        "expected REST classifications"
    );

    let candidates = discover::discover_candidates(&classified);
    assert!(!candidates.is_empty(), "expected REST candidates");
    assert!(
        candidates
            .iter()
            .any(|c| c.guessed_pattern.contains("_search")),
        "expected _search template candidate"
    );
}

#[tokio::test]
async fn graphql_session_classify_and_discover() {
    let flows = load_fixture("graphql_session");
    let entries = ir::entries_from_flows(&flows);
    let classified = classify::classify_entries(entries).await;

    assert!(
        classified
            .iter()
            .any(|c| c.protocol == Protocol::Graphql),
        "expected GraphQL classification"
    );
    assert!(
        classified
            .iter()
            .any(|c| c.operation_name.as_deref() == Some("StationBoard")),
        "expected operation name from capture"
    );

    let candidates = discover::discover_candidates(&classified);
    assert!(
        candidates.iter().any(|c| c.protocol == "graphql"),
        "expected graphql candidate"
    );
}

#[tokio::test]
async fn brief_golden_microservice_rest_subset() {
    let flows = load_fixture("microservice_rest");
    let entries = ir::entries_from_flows(&flows);
    let classified = classify::classify_entries(entries).await;
    let candidates = discover::discover_candidates(&classified);

    let pattern = candidates
        .iter()
        .find(|c| c.origin == REST_ORIGIN && c.guessed_pattern.contains("_search"))
        .map(|c| c.guessed_pattern.clone())
        .expect("search candidate");

    let yaml = brief::generate_brief_yaml(
        &classified,
        REST_ORIGIN,
        "rest",
        &[pattern],
        &candidates,
        false,
    )
    .expect("brief yaml");

    assert!(yaml.contains("spoor_version: 1"));
    assert!(yaml.contains("id: portal-example-test"));
    assert!(yaml.contains("protocol: rest"));
    assert!(yaml.contains("name: page"));
    assert!(yaml.contains("Pagination:"));
    assert!(yaml.contains("POST"));
}

#[tokio::test]
async fn brief_graphql_depends_on_note() {
    let flows = load_fixture("graphql_session");
    let entries = ir::entries_from_flows(&flows);
    let classified = classify::classify_entries(entries).await;
    let candidates = discover::discover_candidates(&classified);

    let ops: Vec<String> = candidates
        .iter()
        .filter(|c| c.origin == GQL_ORIGIN)
        .map(|c| c.guessed_pattern.clone())
        .collect();
    assert!(!ops.is_empty());

    let yaml = brief::generate_brief_yaml(
        &classified,
        GQL_ORIGIN,
        "graphql",
        &ops,
        &candidates,
        false,
    )
    .expect("brief yaml");

    assert!(yaml.contains("protocol: graphql"));
    assert!(yaml.contains("processId"));
    assert!(yaml.contains("Session state variables"));
}
