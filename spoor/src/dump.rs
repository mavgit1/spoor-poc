use std::collections::BTreeMap;
use std::io::Write;

use flate2::Compression;
use flate2::write::GzEncoder;
use serde::Serialize;

use crate::capture::CaptureRecord;
use crate::classify::{ClassifiedEntry, Confidence, CoverageReport, Protocol, protocol_str};

#[derive(Serialize)]
struct CaptureDump {
    spoor_version: u32,
    flows_capped: bool,
    flow_count: usize,
    classified_count: usize,
    unclassified_count: usize,
    coverage: CoverageReport,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    jsonrpc_methods: BTreeMap<String, usize>,
    flows: Vec<DumpFlow>,
}

#[derive(Serialize)]
struct DumpFlow {
    #[serde(flatten)]
    flow: CaptureRecord,
    #[serde(skip_serializing_if = "Option::is_none")]
    classify: Option<DumpClassify>,
}

#[derive(Serialize)]
struct DumpClassify {
    protocol: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    operation_name: Option<String>,
    confidence: String,
    origin: String,
    path: String,
}

pub fn build_capture_dump_gzip(
    flows: &[CaptureRecord],
    classified: &[ClassifiedEntry],
    flows_capped: bool,
) -> anyhow::Result<Vec<u8>> {
    let by_id: BTreeMap<&str, &ClassifiedEntry> = classified
        .iter()
        .map(|c| (c.entry.flow.id.as_str(), c))
        .collect();

    let mut jsonrpc_methods: BTreeMap<String, usize> = BTreeMap::new();
    for item in classified
        .iter()
        .filter(|c| c.protocol == Protocol::JsonRpc)
    {
        if let Some(method) = item.operation_name.as_deref() {
            *jsonrpc_methods.entry(method.to_string()).or_insert(0) += 1;
        }
    }

    let coverage = CoverageReport::from_session(flows, classified, flows_capped);

    let dump_flows: Vec<DumpFlow> = flows
        .iter()
        .map(|flow| {
            let classify = by_id.get(flow.id.as_str()).map(|c| DumpClassify {
                protocol: protocol_str(c.protocol).to_string(),
                operation_name: c.operation_name.clone(),
                confidence: confidence_str(c.confidence).to_string(),
                origin: c.entry.origin.clone(),
                path: c.entry.path.clone(),
            });
            DumpFlow {
                flow: flow.clone(),
                classify,
            }
        })
        .collect();

    let classified_count = classified.len();
    let dump = CaptureDump {
        spoor_version: 2,
        flows_capped,
        flow_count: flows.len(),
        classified_count,
        unclassified_count: flows.len().saturating_sub(by_id.len()),
        coverage,
        jsonrpc_methods,
        flows: dump_flows,
    };

    let json = serde_json::to_vec(&dump)?;
    let mut out = Vec::new();
    {
        let mut enc = GzEncoder::new(&mut out, Compression::default());
        enc.write_all(&json)?;
        enc.finish()?;
    }
    Ok(out)
}

fn confidence_str(c: Confidence) -> &'static str {
    match c {
        Confidence::Parser => "parser",
        Confidence::Llm => "llm",
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::capture::{Body, CaptureRecord, Transport};
    use crate::ir::TrafficEntry;

    fn sample_flow(id: &str, url: &str, body: &str) -> CaptureRecord {
        CaptureRecord {
            id: id.into(),
            transport: Transport::Http,
            url: url.into(),
            method: Some("POST".into()),
            request_headers: HashMap::from([("content-type".into(), "application/json".into())]),
            request_body: Some(Body::text(body)),
            status: Some(200),
            response_headers: None,
            response_body: Some(Body::text(r#"{"jsonrpc":"2.0","result":{},"id":1}"#)),
            resource_type: Some("Fetch".into()),
            sequence: 0,
            timestamp_ms: None,
            ws_request_id: None,
            ws_opcode: None,
            direction: None,
        }
    }

    #[test]
    fn dump_includes_classify_overlay_and_method_index() {
        let flows = vec![sample_flow(
            "1",
            "https://api.example.test/jsonrpc",
            r#"{"jsonrpc":"2.0","method":"Alpha","params":{},"id":1}"#,
        )];
        let entry = TrafficEntry::from_flow(flows[0].clone()).unwrap();
        let classified = vec![ClassifiedEntry {
            entry,
            protocol: Protocol::JsonRpc,
            confidence: Confidence::Parser,
            operation_name: Some("Alpha".into()),
        }];

        let gzip = build_capture_dump_gzip(&flows, &classified, false).unwrap();
        let raw = flate2::read::GzDecoder::new(&gzip[..]);
        let dump: serde_json::Value = serde_json::from_reader(raw).unwrap();

        assert_eq!(dump["flow_count"], 1);
        assert_eq!(dump["classified_count"], 1);
        assert_eq!(dump["flows"][0]["classify"]["protocol"], "jsonrpc");
        assert_eq!(dump["jsonrpc_methods"]["Alpha"], 1);
        assert!(dump["coverage"].is_object());
    }
}
