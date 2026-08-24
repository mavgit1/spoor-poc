//! Telemetry beacon detection.
//!
//! Beacons are the hardest noise to remove because they are deliberately shaped
//! like APIs: `POST /api/v1/public/job/impression`, `POST /api/log`,
//! `POST /api/v2/pixel`. Path shape alone cannot decide it — `/api/v1/events`
//! is a plausible real endpoint — and neither can an empty response, since a
//! real `DELETE` legitimately returns 204.
//!
//! So we require **both** signals together:
//!
//! 1. a generic telemetry token in the path, and
//! 2. no observed response body.
//!
//! An endpoint an agent could integrate with returns something. One that only
//! ever accepted writes and answered with nothing offers no contract to
//! describe, which is also why omitting it costs the pack nothing.
//!
//! Tokens are path-only and vendor-neutral. Host matching is forbidden — see
//! the project rules — because the same beacon paths appear on first-party
//! hosts, and real APIs appear on third-party ones.

use crate::capture::Body;
use crate::ir::TrafficEntry;

/// Generic telemetry path tokens. These name an *act of reporting*, not a
/// resource, which is what separates them from nouns like `/jobs` or `/users`.
const TELEMETRY_TOKENS: &[&str] = &[
    "/collect",
    "/beacon",
    "/pixel",
    "/impression",
    "/telemetry",
    "/analytics",
    "/statistics",
    "/metrics",
    "/log",
    "/logs",
    "/track",
    "/tracking",
    "/conversion",
    "/gen_204",
    "/ping",
];

/// True when the response carried nothing an agent could learn a contract from.
fn has_no_observed_response_body(entry: &TrafficEntry) -> bool {
    match &entry.flow.response_body {
        None => true,
        Some(Body::Omitted { .. }) => true,
        Some(Body::Text(t)) => t.trim().is_empty(),
        Some(Body::Bytes { data, .. }) => data.is_empty(),
    }
}

fn has_telemetry_token(path: &str) -> bool {
    let p = path.to_ascii_lowercase();
    TELEMETRY_TOKENS.iter().any(|token| {
        // Match a whole segment or a segment prefix boundary, so `/logs` and
        // `/log/batch` match while `/login` and `/catalog` do not.
        match p.find(token) {
            Some(idx) => {
                let rest = &p[idx + token.len()..];
                rest.is_empty() || rest.starts_with('/') || rest.starts_with('.')
            }
            None => false,
        }
    })
}

/// Both signals must hold. Either one alone produces false positives that would
/// silently discard real endpoints.
pub fn looks_like_beacon(entry: &TrafficEntry) -> bool {
    has_telemetry_token(&entry.path) && has_no_observed_response_body(entry)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture::{CaptureRecord, Transport};

    fn entry(method: &str, url: &str, response: Option<Body>) -> TrafficEntry {
        TrafficEntry::from_flow(CaptureRecord {
            id: "1".into(),
            transport: Transport::Http,
            url: url.into(),
            method: Some(method.into()),
            request_headers: Default::default(),
            request_body: None,
            status: Some(204),
            response_headers: None,
            response_body: response,
            resource_type: Some("XHR".into()),
            sequence: 1,
            timestamp_ms: None,
            ws_request_id: None,
            ws_opcode: None,
            direction: None,
        })
        .unwrap()
    }

    fn json(body: &str) -> Option<Body> {
        Some(Body::text(body))
    }

    /// Paths taken from a real recording session (jobs.ch, job-room.ch, deepl,
    /// google) so the corpus is not invented.
    #[test]
    fn drops_real_beacons_from_a_live_session() {
        for (method, url) in [
            ("POST", "https://www.jobs.ch/api/v1/public/job/impression"),
            (
                "POST",
                "https://www.jobs.ch/api/v1/public/product/track/view",
            ),
            ("POST", "https://d.adroll.com/api/v2/pixel"),
            ("POST", "https://example.test/api/log"),
            ("POST", "https://www.google.com/gen_204?s=web"),
            ("GET", "https://region1.google-analytics.com/collect?v=2"),
        ] {
            assert!(
                looks_like_beacon(&entry(method, url, None)),
                "{method} {url} should be treated as a beacon"
            );
        }
    }

    /// The two-signal rule exists for these. Each would be wrongly dropped by
    /// either signal on its own.
    #[test]
    fn keeps_real_endpoints_from_the_same_session() {
        // Real API, JSON response — telemetry token absent anyway.
        assert!(!looks_like_beacon(&entry(
            "GET",
            "https://www.job-room.ch/user-service/api/current-user",
            json(r#"{"id":"u-1"}"#)
        )));
        assert!(!looks_like_beacon(&entry(
            "POST",
            "https://www.job-room.ch/jobadservice/api/jobAdvertisements/_search",
            json(r#"{"totalCount":42}"#)
        )));
        assert!(!looks_like_beacon(&entry(
            "POST",
            "https://www.deepl.com/v1/storefront/translate",
            json(r#"{"text":"hallo"}"#)
        )));

        // Empty response but no telemetry token: a real DELETE returning 204.
        assert!(!looks_like_beacon(&entry(
            "DELETE",
            "https://www.jobs.ch/api/v1/user/bookmark/job",
            None
        )));

        // Telemetry token but a real response body: an endpoint that reports
        // *and* answers is describable, so we keep it.
        assert!(!looks_like_beacon(&entry(
            "POST",
            "https://example.test/api/v1/metrics",
            json(r#"{"accepted":3,"rejected":0}"#)
        )));
    }

    #[test]
    fn token_match_respects_segment_boundaries() {
        assert!(has_telemetry_token("/api/log"));
        assert!(has_telemetry_token("/api/logs"));
        assert!(has_telemetry_token("/api/log/batch"));
        assert!(has_telemetry_token("/v1/collect.gif"));

        // Substrings that merely contain a token must not match.
        assert!(!has_telemetry_token("/api/login"));
        assert!(!has_telemetry_token("/api/logout"));
        assert!(!has_telemetry_token("/catalog"));
        assert!(!has_telemetry_token("/api/v1/blogging"));
        assert!(!has_telemetry_token("/pixelart/gallery"));
    }

    #[test]
    fn omitted_and_blank_bodies_count_as_absent() {
        use crate::capture::OmitReason;
        assert!(looks_like_beacon(&entry(
            "POST",
            "https://example.test/beacon",
            Some(Body::Omitted {
                reason: OmitReason::TooLarge
            })
        )));
        assert!(looks_like_beacon(&entry(
            "POST",
            "https://example.test/beacon",
            Some(Body::text("   "))
        )));
    }
}
