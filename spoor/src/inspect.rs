//! Reading recordings: the two lookups that do most of the work when an agent
//! figures out a site.
//!
//! - [`matching_flows`]: which requests mention X (url or either body)?
//! - [`trace`]: where did value X first *appear* (a response — HTML included),
//!   and which later requests *used* it? This is how you learn that an id comes
//!   from a hidden `<input>` on some page, or a CSRF token from a meta tag.
//!
//! Plain substring search over every flow, no site knowledge.

use crate::capture::CaptureRecord;

const SNIPPET_RADIUS: usize = 60;

fn contains(haystack: Option<&str>, needle: &str) -> bool {
    haystack.is_some_and(|h| h.contains(needle))
}

pub fn matching_flows<'a>(
    flows: &'a [CaptureRecord],
    needle: Option<&str>,
) -> Vec<&'a CaptureRecord> {
    let mut out: Vec<&CaptureRecord> = flows
        .iter()
        .filter(|f| match needle {
            None => true,
            Some(n) => {
                f.url.contains(n) || contains(f.text_request(), n) || contains(f.text_response(), n)
            }
        })
        .collect();
    out.sort_by_key(|f| f.sequence);
    out
}

/// One line per flow: `seq METHOD status type url`.
pub fn flow_line(f: &CaptureRecord) -> String {
    let method = if f.method.is_some() {
        f.http_method().to_string()
    } else {
        format!(
            "WS{}",
            f.direction.map(|d| format!(" {d:?}")).unwrap_or_default()
        )
    };
    let status = f
        .status
        .map(|s| s.to_string())
        .unwrap_or_else(|| "-".into());
    let kind = f.resource_type.as_deref().unwrap_or("-");
    format!(
        "{:>5} {:<7} {:>3} {:<10} {}",
        f.sequence, method, status, kind, f.url
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraceHit {
    pub sequence: u64,
    pub method: String,
    pub url: String,
    /// `response body`, `response header location`, `url`, `request body`, …
    pub place: String,
    pub snippet: String,
}

#[derive(Debug, Default)]
pub struct Trace {
    /// Server → browser: where the value came from.
    pub appears_in: Vec<TraceHit>,
    /// Browser → server: where it was sent back.
    pub used_in: Vec<TraceHit>,
}

pub fn trace(flows: &[CaptureRecord], value: &str) -> Trace {
    let mut out = Trace::default();
    if value.is_empty() {
        return out;
    }
    let mut ordered: Vec<&CaptureRecord> = flows.iter().collect();
    ordered.sort_by_key(|f| f.sequence);
    for f in ordered {
        let hit = |place: String, text: &str| TraceHit {
            sequence: f.sequence,
            method: f.http_method().to_string(),
            url: f.url.clone(),
            place,
            snippet: snippet(text, value),
        };
        if let Some(body) = f.text_response()
            && body.contains(value)
        {
            out.appears_in.push(hit("response body".into(), body));
        }
        for (name, v) in sorted_headers(f.response_headers.as_ref()) {
            if v.contains(value) {
                out.appears_in.push(hit(
                    format!("response header {}", name.to_ascii_lowercase()),
                    v,
                ));
            }
        }
        if f.url.contains(value) {
            out.used_in.push(hit("url".into(), &f.url));
        }
        for (name, v) in sorted_headers(Some(&f.request_headers)) {
            if v.contains(value) {
                out.used_in.push(hit(
                    format!("request header {}", name.to_ascii_lowercase()),
                    v,
                ));
            }
        }
        if let Some(body) = f.text_request()
            && body.contains(value)
        {
            out.used_in.push(hit("request body".into(), body));
        }
    }
    out
}

fn sorted_headers(
    headers: Option<&std::collections::HashMap<String, String>>,
) -> Vec<(&String, &String)> {
    let mut v: Vec<_> = headers.map(|h| h.iter().collect()).unwrap_or_default();
    v.sort();
    v
}

/// `…context around the first match…`, whitespace collapsed, char-boundary safe.
pub fn snippet(text: &str, needle: &str) -> String {
    let Some(pos) = text.find(needle) else {
        return String::new();
    };
    let mut start = pos.saturating_sub(SNIPPET_RADIUS);
    while !text.is_char_boundary(start) {
        start -= 1;
    }
    let mut end = (pos + needle.len() + SNIPPET_RADIUS).min(text.len());
    while !text.is_char_boundary(end) {
        end += 1;
    }
    let body: String = text[start..end]
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    format!(
        "{}{body}{}",
        if start > 0 { "…" } else { "" },
        if end < text.len() { "…" } else { "" }
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture::{Body, Transport};
    use std::collections::HashMap;

    fn flow(
        seq: u64,
        method: &str,
        url: &str,
        req: Option<&str>,
        resp: Option<&str>,
    ) -> CaptureRecord {
        CaptureRecord {
            id: format!("f{seq}"),
            transport: Transport::Http,
            url: url.into(),
            method: Some(method.into()),
            request_headers: HashMap::new(),
            request_body: req.map(Body::text),
            status: Some(200),
            response_headers: None,
            response_body: resp.map(Body::text),
            resource_type: Some("XHR".into()),
            sequence: seq,
            timestamp_ms: None,
            ws_request_id: None,
            ws_opcode: None,
            direction: None,
        }
    }

    /// The Hostpoint shape: an id first shows up in page HTML, then is posted back.
    fn hostpoint_like() -> Vec<CaptureRecord> {
        vec![
            flow(
                3,
                "POST",
                "https://panel.test/dns/edit?name=a.ch",
                Some("_action_get_records=1&id=805415"),
                Some(r#"{"records":[]}"#),
            ),
            flow(
                1,
                "GET",
                "https://panel.test/dns/edit?name=a.ch",
                None,
                Some(r#"<form>  <input type="hidden" name="domainId" value="805415"> </form>"#),
            ),
            flow(
                2,
                "GET",
                "https://panel.test/api/me",
                None,
                Some(r#"{"user":"x"}"#),
            ),
        ]
    }

    #[test]
    fn trace_finds_origin_and_use() {
        let t = trace(&hostpoint_like(), "805415");
        assert_eq!(t.appears_in.len(), 1);
        assert_eq!(t.appears_in[0].sequence, 1);
        assert_eq!(t.appears_in[0].place, "response body");
        assert!(
            t.appears_in[0]
                .snippet
                .contains(r#"name="domainId" value="805415""#)
        );
        assert_eq!(t.used_in.len(), 1);
        assert_eq!(t.used_in[0].place, "request body");
        assert_eq!(t.used_in[0].sequence, 3);
    }

    #[test]
    fn matching_flows_is_ordered_and_filtered() {
        let flows = hostpoint_like();
        let seqs: Vec<u64> = matching_flows(&flows, None)
            .iter()
            .map(|f| f.sequence)
            .collect();
        assert_eq!(seqs, vec![1, 2, 3]);
        let seqs: Vec<u64> = matching_flows(&flows, Some("_action_get_records"))
            .iter()
            .map(|f| f.sequence)
            .collect();
        assert_eq!(seqs, vec![3]);
    }

    #[test]
    fn snippet_is_char_boundary_safe() {
        let text = format!("{}needle{}", "ü".repeat(80), "é".repeat(80));
        let s = snippet(&text, "needle");
        assert!(s.starts_with('…') && s.ends_with('…') && s.contains("needle"));
        assert_eq!(snippet("abc", "zzz"), "");
    }
}
