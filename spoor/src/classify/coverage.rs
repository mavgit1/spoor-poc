use serde::Serialize;

use crate::capture::{Body, CaptureRecord, Transport};
use crate::classify::{ClassifiedEntry, Protocol};

#[derive(Debug, Clone, Default, Serialize)]
pub struct CoverageReport {
    pub undecoded_binary: usize,
    pub websocket_frames: usize,
    pub grpc_or_protobuf: usize,
    pub omitted_bodies: usize,
    pub flows_capped: bool,
}

impl CoverageReport {
    pub fn from_session(
        flows: &[CaptureRecord],
        classified: &[ClassifiedEntry],
        flows_capped: bool,
    ) -> Self {
        let undecoded_binary = flows.iter().filter(|f| f.has_binary_payload()).count();
        let websocket_frames = flows
            .iter()
            .filter(|f| f.transport == Transport::WebSocket)
            .count();
        let grpc_or_protobuf = classified
            .iter()
            .filter(|c| matches!(c.protocol, Protocol::GrpcWeb | Protocol::Protobuf))
            .count();
        let omitted_bodies = flows
            .iter()
            .filter(|f| {
                matches!(f.request_body, Some(Body::Omitted { .. }))
                    || matches!(f.response_body, Some(Body::Omitted { .. }))
            })
            .count();

        Self {
            undecoded_binary,
            websocket_frames,
            grpc_or_protobuf,
            omitted_bodies,
            flows_capped,
        }
    }

    pub fn for_origin(&self, flows: &[CaptureRecord], origin: &str) -> Self {
        let origin_flows: Vec<_> = flows
            .iter()
            .filter(|f| {
                url::Url::parse(&f.url)
                    .ok()
                    .and_then(|u| {
                        let host = u.host_str()?;
                        Some(format!("{}://{host}", u.scheme()) == origin)
                    })
                    .unwrap_or(false)
            })
            .cloned()
            .collect();
        Self::from_session(&origin_flows, &[], self.flows_capped)
    }

    pub fn is_partial(&self) -> bool {
        self.undecoded_binary > 0 || self.grpc_or_protobuf > 0 || self.flows_capped
    }
}
