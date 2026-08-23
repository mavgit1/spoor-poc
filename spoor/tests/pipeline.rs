use spoor::classify::{self, ClassifiedEntry, Protocol};
use spoor::discover;
use spoor::export;
use spoor::ir;
use spoor::types::{Candidate, CapturedFlow, GenerateRequest, GenerateSelection};

const REST_ORIGIN: &str = "https://portal.example.test";
const GQL_ORIGIN: &str = "https://api.example.test";
const RPC_ORIGIN: &str = "https://api.example.test";

fn load_fixture(name: &str) -> Vec<CapturedFlow> {
    let path = format!("tests/fixtures/{name}.json");
    let raw = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path}: {e}"));
    serde_json::from_str(&raw).unwrap_or_else(|e| panic!("parse {path}: {e}"))
}

fn pack_files(
    classified: &[ClassifiedEntry],
    candidates: &[Candidate],
    flows: &[CapturedFlow],
    selected: Vec<GenerateSelection>,
) -> Vec<(String, String)> {
    let req = GenerateRequest {
        origin: None,
        selected,
        ignore_patterns: vec![],
        redact: false,
    };
    let coverage = classify::CoverageReport::from_session(flows, classified, false);
    export::pack::build_pack_files(classified, candidates, &req, flows, &coverage, &[])
        .expect("pack files")
}

fn pack_blob(files: &[(String, String)]) -> String {
    files
        .iter()
        .map(|(name, body)| format!("# {name}\n{body}"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn pack_named<'a>(files: &'a [(String, String)], name: &str) -> &'a str {
    files
        .iter()
        .find(|(n, _)| n == name)
        .map(|(_, body)| body.as_str())
        .unwrap_or_else(|| {
            panic!(
                "missing {name}, have {:?}",
                files.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>()
            )
        })
}

#[tokio::test]
async fn microservice_rest_classify_and_discover() {
    let flows = load_fixture("microservice_rest");
    let entries = ir::entries_from_flows(&flows);
    assert_eq!(entries.len(), 3, "all fixture URLs should parse");

    let classified = classify::classify_entries(entries).await;
    assert!(
        classified.iter().any(|c| c.protocol == Protocol::Rest),
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
        classified.iter().any(|c| c.protocol == Protocol::Graphql),
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
async fn pack_evidence_only_no_speculative_pagination() {
    let flows = load_fixture("microservice_rest");
    let entries = ir::entries_from_flows(&flows);
    let classified = classify::classify_entries(entries).await;
    let candidates = discover::discover_candidates(&classified);

    let search = candidates
        .iter()
        .find(|c| c.origin == REST_ORIGIN && c.guessed_pattern.contains("_search"))
        .expect("search candidate");
    let files = pack_files(
        &classified,
        &candidates,
        &flows,
        vec![GenerateSelection {
            id: search.id.clone(),
            pattern: Some(search.guessed_pattern.clone()),
        }],
    );
    let yaml = pack_blob(&files);

    assert!(yaml.contains("spoor_version: 3"));
    assert!(yaml.contains("protocol: rest"));
    // Must not invent how-to pagination / search advice
    assert!(!yaml.contains("Pagination: increment"));
    assert!(!yaml.contains("For search APIs:"));
    assert!(!yaml.contains("you can paginate"));
    assert!(!yaml.contains("how-to"));
    // Evidence: varied page param may appear as observation or common_query_params
    assert!(yaml.contains("page") || yaml.contains("query_param"));
}

#[tokio::test]
async fn pack_graphql_includes_process_id_handoff() {
    let flows = load_fixture("graphql_session");
    let entries = ir::entries_from_flows(&flows);
    let classified = classify::classify_entries(entries).await;
    let candidates = discover::discover_candidates(&classified);

    let selected: Vec<GenerateSelection> = candidates
        .iter()
        .filter(|c| c.origin == GQL_ORIGIN)
        .map(|c| GenerateSelection {
            id: c.id.clone(),
            pattern: Some(c.guessed_pattern.clone()),
        })
        .collect();
    assert!(
        selected.len() >= 2,
        "expected producer and consumer GraphQL ops, got {selected:?}"
    );

    let files = pack_files(&classified, &candidates, &flows, selected);
    let relations = pack_named(&files, "relations.yaml");
    assert!(
        relations.contains("depends_on:"),
        "expected depends_on in relations.yaml:\n{relations}"
    );
    assert!(
        relations.contains("processId"),
        "expected processId id-handoff edge in relations.yaml:\n{relations}"
    );
    assert!(relations.contains("from_op_id:"));
    assert!(relations.contains("to_op_id:"));
}

#[tokio::test]
async fn jsonrpc_session_classify_and_discover() {
    let flows = load_fixture("jsonrpc_session");
    let entries = ir::entries_from_flows(&flows);
    let classified = classify::classify_entries(entries).await;

    assert!(
        classified
            .iter()
            .filter(|c| c.protocol == Protocol::JsonRpc)
            .count()
            >= 2,
        "expected JSON-RPC classifications for Alpha and Beta"
    );

    let candidates = discover::discover_candidates(&classified);
    let rpc: Vec<_> = candidates
        .iter()
        .filter(|c| c.protocol == "jsonrpc" && c.origin == RPC_ORIGIN)
        .collect();
    assert_eq!(rpc.len(), 2, "expected two RPC method candidates");
    assert!(rpc.iter().any(|c| c.guessed_pattern == "Alpha"));
    assert!(rpc.iter().any(|c| c.guessed_pattern == "Beta"));
}

#[tokio::test]
async fn pack_jsonrpc_two_ops() {
    let flows = load_fixture("jsonrpc_session");
    let entries = ir::entries_from_flows(&flows);
    let classified = classify::classify_entries(entries).await;
    let candidates = discover::discover_candidates(&classified);

    let selected: Vec<GenerateSelection> = candidates
        .iter()
        .filter(|c| c.origin == RPC_ORIGIN && c.protocol == "jsonrpc")
        .map(|c| GenerateSelection {
            id: c.id.clone(),
            pattern: Some(c.guessed_pattern.clone()),
        })
        .collect();
    assert_eq!(selected.len(), 2);

    let files = pack_files(&classified, &candidates, &flows, selected);
    let yaml = pack_blob(&files);

    assert!(yaml.contains("protocol: jsonrpc"));
    assert!(yaml.contains("rpc_method: Alpha"));
    assert!(yaml.contains("rpc_method: Beta"));
}

#[tokio::test]
async fn websocket_session_classify_and_discover() {
    let flows = load_fixture("websocket_session");
    let entries = ir::entries_from_flows(&flows);
    let classified = classify::classify_entries(entries).await;
    assert!(
        classified.iter().any(|c| c.protocol == Protocol::WebSocket),
        "expected WebSocket classification"
    );
    assert!(
        classified
            .iter()
            .any(|c| c.operation_name.as_deref() == Some("subscribe")),
        "expected subscribe message type"
    );
    let candidates = discover::discover_candidates(&classified);
    assert!(candidates.iter().any(|c| c.protocol == "websocket"));
}

#[tokio::test]
async fn grpcweb_detect_classify_and_discover() {
    let flows = load_fixture("grpcweb_detect");
    let entries = ir::entries_from_flows(&flows);
    let classified = classify::classify_entries(entries).await;
    assert!(
        classified.iter().any(|c| c.protocol == Protocol::GrpcWeb),
        "expected gRPC-Web detection"
    );
    let candidates = discover::discover_candidates(&classified);
    assert!(candidates.iter().any(|c| c.protocol == "grpcweb"));
}

#[tokio::test]
async fn jsonrpc_batch_expands_methods() {
    let flows = load_fixture("jsonrpc_batch");
    let entries = ir::entries_from_flows(&flows);
    let classified = classify::classify_entries(entries).await;
    let methods: Vec<_> = classified
        .iter()
        .filter(|c| c.protocol == Protocol::JsonRpc)
        .filter_map(|c| c.operation_name.clone())
        .collect();
    assert!(methods.contains(&"Alpha".to_string()));
    assert!(methods.contains(&"Beta".to_string()));
}

#[tokio::test]
async fn form_urlencoded_classify_and_discover() {
    let flows = load_fixture("form_urlencoded");
    let entries = ir::entries_from_flows(&flows);
    let classified = classify::classify_entries(entries).await;
    assert!(
        classified.iter().any(|c| c.protocol == Protocol::Form),
        "expected form classification"
    );
    let candidates = discover::discover_candidates(&classified);
    assert!(candidates.iter().any(|c| c.protocol == "form"));
}

#[tokio::test]
async fn export_pack_has_manifest_surfaces_and_relations() {
    let flows = load_fixture("jsonrpc_session");
    let entries = ir::entries_from_flows(&flows);
    let classified = classify::classify_entries(entries).await;
    let candidates = discover::discover_candidates(&classified);
    let selected: Vec<_> = candidates
        .iter()
        .filter(|c| c.protocol == "jsonrpc")
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
    let coverage = classify::CoverageReport::from_session(&flows, &classified, false);
    let result = export::generate_bundle_with_coverage(
        &classified,
        &candidates,
        &req,
        &flows,
        &coverage,
        &[],
    )
    .expect("bundle");
    let cursor = std::io::Cursor::new(result.bundle.zip_bytes);
    let mut archive = zip::ZipArchive::new(cursor).unwrap();
    let names: Vec<_> = (0..archive.len())
        .map(|i| archive.by_index(i).unwrap().name().to_string())
        .collect();
    assert!(
        names.iter().any(|n| n == "MANIFEST.yaml"),
        "expected MANIFEST.yaml, got {names:?}"
    );
    assert!(names.iter().any(|n| n == "relations.yaml"));
    assert!(
        names
            .iter()
            .any(|n| n.starts_with("surfaces/") && n.ends_with("surface.yaml")),
        "expected surface.yaml under surfaces/, got {names:?}"
    );
    assert!(
        names
            .iter()
            .any(|n| n.contains("/ops/") && n.ends_with(".yaml")),
        "expected per-op yaml files, got {names:?}"
    );
    assert!(
        !names.iter().any(|n| n.starts_with("integration-brief-")),
        "legacy flat briefs should not be emitted"
    );
    assert!(
        !names.iter().any(|n| n == "product_map.yaml"),
        "product_map replaced by MANIFEST"
    );

    let mut manifest = archive.by_name("MANIFEST.yaml").unwrap();
    let mut body = String::new();
    std::io::Read::read_to_string(&mut manifest, &mut body).unwrap();
    assert!(body.contains("spoor_version: 3"));
    assert!(body.contains("purpose: agent_integration"));
    assert!(body.contains("read_order:"));
}
