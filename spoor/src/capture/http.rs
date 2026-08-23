use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use anyhow::Result;
use base64::Engine;
use base64::prelude::BASE64_STANDARD;
use chromiumoxide::Page;
use chromiumoxide::cdp::browser_protocol::network::{
    EventLoadingFailed, EventLoadingFinished, EventRequestWillBeSent, EventResponseReceived,
    GetResponseBodyParams, Headers, PostDataEntry, Response,
};
use futures::StreamExt;
use tokio::sync::RwLock;

use crate::capture::model::{
    Body, CaptureRecord, Direction, Transport, media_omit_reason, resource_type_omit_reason,
};
use crate::log;

/// In-flight requests that never finish (cancelled, failed, SSE) would otherwise
/// sit in `pending` forever. Bound + age keep memory finite.
const MAX_PENDING: usize = 2_048;
const PENDING_TTL: Duration = Duration::from_secs(120);

struct PendingRequest {
    record: CaptureRecord,
    hop: u32,
    updated: Instant,
}

pub(crate) fn headers_to_map(headers: &Headers) -> HashMap<String, String> {
    headers
        .inner()
        .as_object()
        .map(|obj| {
            obj.iter()
                .map(|(k, v)| (k.clone(), v.as_str().unwrap_or("").to_string()))
                .collect()
        })
        .unwrap_or_default()
}

fn unique_flow_id(request_id: &str, hop: u32) -> String {
    format!("{request_id}#{hop}")
}

fn wall_time_ms(secs: f64) -> Option<u64> {
    if secs.is_finite() && secs >= 0.0 {
        Some((secs * 1000.0) as u64)
    } else {
        None
    }
}

/// CDP `PostDataEntry.bytes` is protocol-base64. Concatenate every chunk, then decode.
fn request_body_from_entries(
    entries: Option<&Vec<PostDataEntry>>,
    content_type: Option<String>,
) -> Option<Body> {
    let entries = entries?;
    if entries.is_empty() {
        return None;
    }
    let mut decoded = Vec::new();
    let mut saw_any = false;
    for entry in entries {
        let Some(bytes) = entry.bytes.as_ref() else {
            continue;
        };
        let raw: &str = bytes.as_ref();
        saw_any = true;
        match BASE64_STANDARD.decode(raw) {
            Ok(chunk) => decoded.extend_from_slice(&chunk),
            // Malformed traffic: keep the raw bytes rather than dropping the body.
            Err(_) => decoded.extend_from_slice(raw.as_bytes()),
        }
    }
    if !saw_any {
        return None;
    }
    Some(Body::from_raw_bytes(decoded, content_type))
}

fn decide_response_body(record: &CaptureRecord, body: &str, base64_encoded: bool) -> Body {
    if let Some(rt) = record.resource_type.as_deref()
        && let Some(reason) = resource_type_omit_reason(rt)
    {
        return Body::Omitted { reason };
    }
    let ct = record.response_content_type().map(|s| s.to_string());
    if let Some(ct_ref) = ct.as_deref()
        && let Some(reason) = media_omit_reason(ct_ref)
    {
        return Body::Omitted { reason };
    }
    Body::from_cdp_body(body, base64_encoded, ct)
}

fn apply_response(flow: &mut CaptureRecord, resp: &Response, resource_type: Option<String>) {
    flow.status = Some(resp.status as u16);
    flow.response_headers = Some(headers_to_map(&resp.headers));
    if let Some(rt) = resource_type
        && rt != "Other"
    {
        flow.resource_type = Some(rt);
    }
}

fn take_expired(
    pending: &mut HashMap<String, PendingRequest>,
    now: Instant,
    ttl: Duration,
    max_pending: usize,
) -> Vec<CaptureRecord> {
    let stale: Vec<String> = pending
        .iter()
        .filter(|(_, p)| now.saturating_duration_since(p.updated) > ttl)
        .map(|(k, _)| k.clone())
        .collect();
    let mut out = Vec::new();
    for key in stale {
        if let Some(p) = pending.remove(&key) {
            log::debug(format!(
                "capture/http: evicting stale pending {} ({})",
                key, p.record.url
            ));
            out.push(p.record);
        }
    }
    while pending.len() > max_pending {
        let oldest = pending
            .iter()
            .min_by_key(|(_, p)| p.updated)
            .map(|(k, _)| k.clone());
        let Some(key) = oldest else {
            break;
        };
        if let Some(p) = pending.remove(&key) {
            log::debug(format!(
                "capture/http: evicting overflow pending {} ({})",
                key, p.record.url
            ));
            out.push(p.record);
        }
    }
    out
}

/// Only network traffic can be API evidence.
///
/// Browser-internal schemes (`chrome://`, `devtools://`) and inline payloads
/// (`data:`, `blob:`) are never a remote API, and a single inlined image can be
/// hundreds of kilobytes. Dropping them here rather than in `classify::filters`
/// keeps them out of memory entirely and stops them consuming the flow cap.
fn is_capturable_url(url: &str) -> bool {
    let Some((scheme, _)) = url.split_once(':') else {
        return false;
    };
    matches!(
        scheme.to_ascii_lowercase().as_str(),
        "http" | "https" | "ws" | "wss"
    )
}

fn start_request(
    pending: &mut HashMap<String, PendingRequest>,
    request_id: String,
    hop: u32,
    record: CaptureRecord,
    now: Instant,
) {
    pending.insert(
        request_id,
        PendingRequest {
            record,
            hop,
            updated: now,
        },
    );
}

/// If Chrome reused `request_id` for a redirect, emit the previous hop (no body).
fn finalize_redirect_hop(
    pending: &mut HashMap<String, PendingRequest>,
    request_id: &str,
    redirect: &Response,
    now: Instant,
) -> Option<(CaptureRecord, u32)> {
    let mut prev = pending.remove(request_id)?;
    apply_response(&mut prev.record, redirect, None);
    prev.record.response_body = None;
    prev.updated = now;
    let next_hop = prev.hop.saturating_add(1);
    Some((prev.record, next_hop))
}

async fn push_flow(
    flows: &Arc<RwLock<Vec<CaptureRecord>>>,
    flows_capped: &Arc<AtomicBool>,
    max_flows: usize,
    flow: CaptureRecord,
) {
    log::debug(format!(
        "capture/http: {} {} → {:?}",
        flow.http_method(),
        flow.url,
        flow.status
    ));
    let mut guard = flows.write().await;
    if guard.len() >= max_flows {
        flows_capped.store(true, Ordering::SeqCst);
    } else {
        guard.push(flow);
    }
}

async fn flush_records(
    flows: &Arc<RwLock<Vec<CaptureRecord>>>,
    flows_capped: &Arc<AtomicBool>,
    max_flows: usize,
    records: Vec<CaptureRecord>,
) {
    for flow in records {
        push_flow(flows, flows_capped, max_flows, flow).await;
    }
}

pub async fn run(
    page: Arc<Page>,
    flows: Arc<RwLock<Vec<CaptureRecord>>>,
    flows_capped: Arc<AtomicBool>,
    sequence: Arc<AtomicU64>,
    max_flows: usize,
) -> Result<()> {
    let mut pending: HashMap<String, PendingRequest> = HashMap::new();

    let mut will_be_sent = page.event_listener::<EventRequestWillBeSent>().await?;
    let mut response_received = page.event_listener::<EventResponseReceived>().await?;
    let mut loading_finished = page.event_listener::<EventLoadingFinished>().await?;
    let mut loading_failed = page.event_listener::<EventLoadingFailed>().await?;

    let mut will_be_sent_open = true;
    let mut response_received_open = true;
    let mut loading_finished_open = true;
    let mut loading_failed_open = true;

    loop {
        if !will_be_sent_open
            && !response_received_open
            && !loading_finished_open
            && !loading_failed_open
        {
            break;
        }

        tokio::select! {
            ev = will_be_sent.next(), if will_be_sent_open => {
                match ev {
                    None => {
                        log::debug("capture/http: requestWillBeSent listener closed");
                        will_be_sent_open = false;
                    }
                    Some(ev) => {
                        let now = Instant::now();
                        let evicted = take_expired(&mut pending, now, PENDING_TTL, MAX_PENDING);
                        flush_records(&flows, &flows_capped, max_flows, evicted).await;

                        let req = &ev.request;
                        if !is_capturable_url(&req.url) {
                            continue;
                        }
                        let cdp_id = ev.request_id.inner().clone();
                        let hop = if let Some(redirect) = ev.redirect_response.as_ref() {
                            if let Some((hop_record, next_hop)) =
                                finalize_redirect_hop(&mut pending, &cdp_id, redirect, now)
                            {
                                log::debug(format!(
                                    "capture/http: redirect hop {} {} → {:?}",
                                    hop_record.http_method(),
                                    hop_record.url,
                                    hop_record.status
                                ));
                                push_flow(&flows, &flows_capped, max_flows, hop_record).await;
                                next_hop
                            } else {
                                0
                            }
                        } else {
                            0
                        };

                        let headers = headers_to_map(&req.headers);
                        let ct = CaptureRecord::header_ci(&headers, "content-type")
                            .map(|s| s.to_string());
                        let seq = sequence.fetch_add(1, Ordering::SeqCst);
                        start_request(
                            &mut pending,
                            cdp_id.clone(),
                            hop,
                            CaptureRecord {
                                id: unique_flow_id(&cdp_id, hop),
                                transport: Transport::Http,
                                url: req.url.clone(),
                                method: Some(req.method.clone()),
                                request_headers: headers,
                                request_body: request_body_from_entries(
                                    req.post_data_entries.as_ref(),
                                    ct,
                                ),
                                status: None,
                                response_headers: None,
                                response_body: None,
                                resource_type: ev.r#type.as_ref().map(|t| format!("{t:?}")),
                                sequence: seq,
                                timestamp_ms: wall_time_ms(*ev.wall_time.inner()),
                                ws_request_id: None,
                                ws_opcode: None,
                                direction: Some(Direction::Outbound),
                            },
                            now,
                        );
                    }
                }
            }
            ev = response_received.next(), if response_received_open => {
                match ev {
                    None => {
                        log::debug("capture/http: responseReceived listener closed");
                        response_received_open = false;
                    }
                    Some(ev) => {
                        let now = Instant::now();
                        let evicted = take_expired(&mut pending, now, PENDING_TTL, MAX_PENDING);
                        flush_records(&flows, &flows_capped, max_flows, evicted).await;
                        if let Some(entry) = pending.get_mut(ev.request_id.inner()) {
                            let rt = format!("{:?}", ev.r#type);
                            apply_response(
                                &mut entry.record,
                                &ev.response,
                                if rt == "Other" { None } else { Some(rt) },
                            );
                            entry.updated = now;
                        }
                    }
                }
            }
            ev = loading_finished.next(), if loading_finished_open => {
                match ev {
                    None => {
                        log::debug("capture/http: loadingFinished listener closed");
                        loading_finished_open = false;
                    }
                    Some(ev) => {
                        let now = Instant::now();
                        let evicted = take_expired(&mut pending, now, PENDING_TTL, MAX_PENDING);
                        flush_records(&flows, &flows_capped, max_flows, evicted).await;
                        let id = ev.request_id.inner().clone();
                        if let Some(entry) = pending.remove(&id) {
                            let mut flow = entry.record;
                            let store_body = !matches!(
                                flow.resource_type.as_deref(),
                                Some(rt) if resource_type_omit_reason(rt).is_some()
                            );
                            if store_body {
                                match page
                                    .execute(GetResponseBodyParams::new(ev.request_id.clone()))
                                    .await
                                {
                                    Ok(body) => {
                                        flow.response_body = Some(decide_response_body(
                                            &flow,
                                            &body.body,
                                            body.base64_encoded,
                                        ));
                                    }
                                    Err(_) => {
                                        if let Some(ct) = flow.response_content_type()
                                            && let Some(reason) = media_omit_reason(ct)
                                        {
                                            flow.response_body = Some(Body::Omitted { reason });
                                        }
                                    }
                                }
                            } else if let Some(reason) =
                                flow.resource_type.as_deref().and_then(resource_type_omit_reason)
                            {
                                flow.response_body = Some(Body::Omitted { reason });
                            }
                            push_flow(&flows, &flows_capped, max_flows, flow).await;
                        }
                    }
                }
            }
            ev = loading_failed.next(), if loading_failed_open => {
                match ev {
                    None => {
                        log::debug("capture/http: loadingFailed listener closed");
                        loading_failed_open = false;
                    }
                    Some(ev) => {
                        let now = Instant::now();
                        let evicted = take_expired(&mut pending, now, PENDING_TTL, MAX_PENDING);
                        flush_records(&flows, &flows_capped, max_flows, evicted).await;
                        let id = ev.request_id.inner().clone();
                        if let Some(entry) = pending.remove(&id) {
                            let mut flow = entry.record;
                            let rt = format!("{:?}", ev.r#type);
                            if rt != "Other" && flow.resource_type.is_none() {
                                flow.resource_type = Some(rt);
                            }
                            log::debug(format!(
                                "capture/http: failed {} {} ({})",
                                flow.http_method(),
                                flow.url,
                                ev.error_text
                            ));
                            push_flow(&flows, &flows_capped, max_flows, flow).await;
                        }
                    }
                }
            }
        }
    }

    let leftover: Vec<CaptureRecord> = pending.into_values().map(|p| p.record).collect();
    if !leftover.is_empty() {
        log::debug(format!(
            "capture/http: flushing {} in-flight requests on shutdown",
            leftover.len()
        ));
        flush_records(&flows, &flows_capped, max_flows, leftover).await;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chromiumoxide::cdp::browser_protocol::network::PostDataEntry;

    fn entry(raw: impl Into<String>) -> PostDataEntry {
        PostDataEntry {
            bytes: Some(raw.into().into()),
        }
    }

    #[test]
    fn captures_only_network_schemes() {
        for url in [
            "https://api.example.test/v1/things",
            "http://127.0.0.1:8080/api",
            "wss://api.example.test/socket",
        ] {
            assert!(is_capturable_url(url), "{url} should be captured");
        }
        for url in [
            "chrome://new-tab-page/",
            "devtools://devtools/bundled/panel.js",
            "data:image/png;base64,iVBORw0KGgo=",
            "blob:https://example.test/1234",
            "about:blank",
            "not-a-url",
        ] {
            assert!(!is_capturable_url(url), "{url} should be skipped");
        }
    }

    fn dummy_record(id: &str, url: &str) -> CaptureRecord {
        CaptureRecord {
            id: id.into(),
            transport: Transport::Http,
            url: url.into(),
            method: Some("GET".into()),
            request_headers: HashMap::new(),
            request_body: None,
            status: None,
            response_headers: None,
            response_body: None,
            resource_type: Some("XHR".into()),
            sequence: 0,
            timestamp_ms: None,
            ws_request_id: None,
            ws_opcode: None,
            direction: Some(Direction::Outbound),
        }
    }

    #[test]
    fn concatenates_multi_entry_post_bodies() {
        let hel = BASE64_STANDARD.encode("hel");
        let lo = BASE64_STANDARD.encode("lo");
        let body = request_body_from_entries(
            Some(&vec![entry(hel), entry(lo)]),
            Some("text/plain".into()),
        )
        .expect("body");
        assert_eq!(body.as_text(), Some("hello"));
    }

    #[test]
    fn decodes_post_data_as_protocol_base64_not_via_probe() {
        // "abcd" is valid base64 of 3 bytes — the old probe would mis-classify it.
        let body = request_body_from_entries(Some(&vec![entry("abcd")]), None).expect("body");
        let expected = BASE64_STANDARD.decode("abcd").expect("fixture");
        match body {
            Body::Bytes { data, .. } => assert_eq!(data, expected),
            Body::Text(s) => assert_eq!(s.as_bytes(), expected.as_slice()),
            other => panic!("unexpected body: {other:?}"),
        }
    }

    #[test]
    fn skips_entries_without_bytes_and_concatenates_rest() {
        let a = BASE64_STANDARD.encode("A");
        let c = BASE64_STANDARD.encode("C");
        let entries = vec![entry(a), PostDataEntry { bytes: None }, entry(c)];
        let body = request_body_from_entries(Some(&entries), None).expect("body");
        assert_eq!(body.as_text(), Some("AC"));
    }

    #[test]
    fn unique_ids_differ_across_redirect_hops() {
        assert_ne!(unique_flow_id("req1", 0), unique_flow_id("req1", 1));
        assert_eq!(unique_flow_id("req1", 0), "req1#0");
    }

    #[test]
    fn redirect_hop_emits_previous_record_with_status_and_no_body() {
        let mut pending = HashMap::new();
        let now = Instant::now();
        let mut record = dummy_record("req1#0", "https://example.test/login");
        record.id = unique_flow_id("req1", 0);
        start_request(&mut pending, "req1".into(), 0, record, now);

        let redirect: Response = serde_json::from_value(serde_json::json!({
            "url": "https://example.test/login",
            "status": 302,
            "statusText": "Found",
            "headers": {
                "location": "https://example.test/oauth",
                "set-cookie": "sid=abc"
            },
            "mimeType": "",
            "charset": "",
            "connectionReused": false,
            "connectionId": 0,
            "encodedDataLength": 0,
            "securityState": "unknown"
        }))
        .expect("redirect response fixture");

        let (hop, next) =
            finalize_redirect_hop(&mut pending, "req1", &redirect, now).expect("redirect hop");
        assert_eq!(next, 1);
        assert_eq!(hop.id, "req1#0");
        assert_eq!(hop.status, Some(302));
        assert!(hop.response_body.is_none());
        let headers = hop.response_headers.expect("headers");
        assert_eq!(
            CaptureRecord::header_ci(&headers, "location"),
            Some("https://example.test/oauth")
        );
        assert_eq!(
            CaptureRecord::header_ci(&headers, "set-cookie"),
            Some("sid=abc")
        );
        assert!(pending.is_empty());
    }

    #[test]
    fn pending_eviction_by_age_and_cap() {
        let mut pending = HashMap::new();
        let t0 = Instant::now();
        start_request(
            &mut pending,
            "old".into(),
            0,
            dummy_record("old#0", "https://example.test/old"),
            t0 - Duration::from_secs(30),
        );
        start_request(
            &mut pending,
            "new".into(),
            0,
            dummy_record("new#0", "https://example.test/new"),
            t0,
        );

        let aged = take_expired(&mut pending, t0, Duration::from_secs(10), 8);
        assert_eq!(aged.len(), 1);
        assert_eq!(aged[0].url, "https://example.test/old");
        assert_eq!(pending.len(), 1);
        assert!(pending.contains_key("new"));

        start_request(
            &mut pending,
            "a".into(),
            0,
            dummy_record("a#0", "https://example.test/a"),
            t0,
        );
        start_request(
            &mut pending,
            "b".into(),
            0,
            dummy_record("b#0", "https://example.test/b"),
            t0 + Duration::from_millis(1),
        );
        let overflow = take_expired(
            &mut pending,
            t0 + Duration::from_secs(1),
            Duration::from_secs(60),
            1,
        );
        assert!(!overflow.is_empty());
        assert_eq!(pending.len(), 1);
    }

    #[test]
    fn wall_time_ms_converts_epoch_seconds() {
        assert_eq!(wall_time_ms(1_700_000_000.5), Some(1_700_000_000_500));
        assert_eq!(wall_time_ms(f64::NAN), None);
        assert_eq!(wall_time_ms(-1.0), None);
    }
}
