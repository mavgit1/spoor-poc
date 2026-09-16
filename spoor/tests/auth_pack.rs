use spoor::auth;
use spoor::classify::{self, ClassifiedEntry};
use spoor::discover;
use spoor::export;
use spoor::ir;
use spoor::types::{Candidate, CapturedFlow, GenerateRequest, GenerateSelection};

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

fn select_all(candidates: &[Candidate]) -> Vec<GenerateSelection> {
    candidates
        .iter()
        .map(|c| GenerateSelection {
            id: c.id.clone(),
            pattern: Some(c.guessed_pattern.clone()),
        })
        .collect()
}

async fn classify_fixture(name: &str) -> (Vec<CapturedFlow>, Vec<ClassifiedEntry>, Vec<Candidate>) {
    let flows = load_fixture(name);
    let entries = ir::entries_from_flows(&flows);
    let classified = classify::classify_entries(entries).await;
    let candidates = discover::discover_candidates(&classified);
    (flows, classified, candidates)
}

const BEARER: &str = "tok_live_abcdefghijklmnopqrstuvwxyz012345";
const COOKIE: &str = "s3cretSidValue0123456789abcdef";
const JWT: &str = "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIiOiJ1c2VyLTQyIiwibmFtZSI6IkFsaWNlIiwiaWF0IjoxNzAwMDAwMDAwLCJleHAiOjE3MDAwMDM2MDAsInJvbGUiOiJhZG1pbiJ9.fakesignature";

#[tokio::test]
async fn bearer_pack_is_shape_only_and_finds_issuer() {
    let (flows, classified, candidates) = classify_fixture("auth_bearer").await;
    assert!(
        !candidates.is_empty(),
        "expected REST candidates, classified={classified:?}"
    );
    let files = pack_files(&classified, &candidates, &flows, select_all(&candidates));
    let blob = pack_blob(&files);
    assert!(
        !blob.contains(BEARER),
        "raw bearer token leaked into pack:\n{blob}"
    );
    let auth = pack_named(&files, "surfaces/api-example-test_rest/auth.yaml");
    assert!(auth.contains("header:authorization"), "{auth}");
    assert!(auth.contains("Bearer"), "{auth}");
    assert!(
        auth.contains("interactive_command: spoor auth --surface api-example-test_rest"),
        "{auth}"
    );
    assert!(auth.contains("status: observed"), "{auth}");
    assert!(auth.contains("body:/access_token"), "{auth}");
    assert!(auth.contains("prefix: tok_"), "{auth}");
    assert!(!auth.contains("example:"), "{auth}");
}

#[tokio::test]
async fn cookie_redirect_hop_is_issuer() {
    let (flows, classified, candidates) = classify_fixture("auth_cookie_redirect").await;
    let files = pack_files(&classified, &candidates, &flows, select_all(&candidates));
    let blob = pack_blob(&files);
    assert!(
        !blob.contains(COOKIE),
        "raw cookie leaked into pack:\n{blob}"
    );
    let auth = files
        .iter()
        .find(|(n, _)| n.ends_with("/auth.yaml"))
        .map(|(_, b)| b.as_str())
        .unwrap_or_else(|| {
            panic!(
                "missing auth.yaml, have {:?}",
                files.iter().map(|(n, _)| n).collect::<Vec<_>>()
            )
        });
    assert!(auth.contains("cookie:sid"), "{auth}");
    assert!(auth.contains("status: observed"), "{auth}");
    assert!(auth.contains("set-cookie:sid"), "{auth}");
    assert!(auth.contains("request_id: login#0"), "{auth}");
    assert!(!auth.contains("how to log in"), "{auth}");
}

#[tokio::test]
async fn jwt_claim_names_not_values_and_not_observed_issuer() {
    let (flows, classified, candidates) = classify_fixture("auth_jwt").await;
    let files = pack_files(&classified, &candidates, &flows, select_all(&candidates));
    let blob = pack_blob(&files);
    assert!(!blob.contains(JWT), "raw JWT leaked into pack:\n{blob}");
    assert!(!blob.contains("Alice"), "JWT claim value leaked:\n{blob}");
    assert!(!blob.contains("user-42"), "JWT claim value leaked:\n{blob}");
    let auth = pack_named(&files, "surfaces/api-example-test_rest/auth.yaml");
    assert!(auth.contains("claim_names:"), "{auth}");
    assert!(auth.contains("- sub"), "{auth}");
    assert!(auth.contains("- name"), "{auth}");
    assert!(auth.contains("- role"), "{auth}");
    assert!(auth.contains("alg: HS256"), "{auth}");
    assert!(auth.contains("status: not_observed"), "{auth}");
    assert!(
        auth.contains("already present") || auth.contains("persistent profile"),
        "{auth}"
    );
}

#[test]
fn fingerprint_is_stable_and_non_reversible() {
    let a = auth::fingerprint(BEARER);
    let b = auth::fingerprint(BEARER);
    assert_eq!(a, b);
    assert_eq!(a.prefix, "tok_");
    assert!(BEARER.len() > a.prefix.len());
    assert!(!BEARER[4..].contains(&a.prefix) || a.prefix == "tok_");
}
