//! Find the request whose *response* first produced a credential value later sent by clients.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::capture::CaptureRecord;
use crate::export::session::{collect_id_values, looks_like_id_value};

use super::carrier::Carrier;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Acquisition {
    Observed {
        #[serde(skip_serializing_if = "Option::is_none")]
        op_id: Option<String>,
        request_id: String,
        method: String,
        url: String,
        response_location: String,
    },
    NotObserved {
        reason: String,
    },
}

impl Acquisition {
    pub fn not_observed(reason: impl Into<String>) -> Self {
        Self::NotObserved {
            reason: reason.into(),
        }
    }
}

/// First response (by sequence) that contained `secret`, if any.
pub fn find_issuer(
    secret: &str,
    flows: &[CaptureRecord],
    flow_to_op: &HashMap<String, String>,
) -> Acquisition {
    if secret.is_empty() {
        return Acquisition::not_observed(
            "credential value was empty — no issuer can be identified",
        );
    }

    let mut flows_sorted: Vec<&CaptureRecord> = flows.iter().collect();
    flows_sorted.sort_by_key(|f| f.sequence);

    let first_sent_seq = flows_sorted.iter().find_map(|f| {
        if request_carries_secret(f, secret) {
            Some(f.sequence)
        } else {
            None
        }
    });

    for flow in &flows_sorted {
        if let Some(sent) = first_sent_seq
            && flow.sequence >= sent
        {
            // Issuer must precede (or be) the first use; a later echo is not issuance.
            // Allow equal sequence: login response that both sets and is the same record.
        }
        if let Some(location) = response_location_of(flow, secret) {
            if let Some(sent) = first_sent_seq
                && flow.sequence > sent
            {
                continue;
            }
            return Acquisition::Observed {
                op_id: flow_to_op.get(&flow.id).cloned(),
                request_id: flow.id.clone(),
                method: flow.http_method().to_string(),
                url: flow.url.clone(),
                response_location: location,
            };
        }
    }

    if first_sent_seq.is_some() {
        Acquisition::not_observed(
            "credential already present at session start (recording browser uses a persistent profile at cache_dir()/profile-record)",
        )
    } else {
        Acquisition::not_observed(
            "value was never observed in a response body or Set-Cookie in this session",
        )
    }
}

fn request_carries_secret(flow: &CaptureRecord, secret: &str) -> bool {
    if flow.request_headers.values().any(|v| v.contains(secret)) {
        return true;
    }
    if flow.url.contains(secret) {
        return true;
    }
    if flow.text_request().is_some_and(|b| b.contains(secret)) {
        return true;
    }
    false
}

fn response_location_of(flow: &CaptureRecord, secret: &str) -> Option<String> {
    if let Some(headers) = flow.response_headers.as_ref() {
        for (name, value) in headers {
            if name.eq_ignore_ascii_case("set-cookie") {
                for cookie in split_set_cookie(value) {
                    if let Some((cname, cval)) = cookie.split_once('=') {
                        let cval = cval.split(';').next().unwrap_or(cval).trim();
                        if cval == secret {
                            return Some(format!("set-cookie:{}", cname.trim()));
                        }
                    }
                }
            }
            if value.contains(secret) {
                return Some(format!("header:{}", name.to_ascii_lowercase()));
            }
        }
    }

    if let Some(body) = flow.text_response() {
        if let Ok(json) = serde_json::from_str::<Value>(body) {
            if let Some(ptr) = json_pointer_holding(&json, secret) {
                return Some(format!("body:{ptr}"));
            }
            // Reuse session id-walk as a second pass for id-shaped tokens.
            if looks_like_id_value(secret) {
                for (key, val) in collect_id_values(&json) {
                    if val == secret {
                        return Some(format!("body:/{key}"));
                    }
                }
            }
        }
        if body.contains(secret) {
            return Some("body".to_string());
        }
    }
    None
}

fn split_set_cookie(header: &str) -> Vec<&str> {
    // CDP often concatenates Set-Cookie with newlines; commas are unsafe (Expires=).
    if header.contains('\n') {
        header
            .lines()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .collect()
    } else {
        vec![header]
    }
}

fn json_pointer_holding(json: &Value, secret: &str) -> Option<String> {
    fn walk(json: &Value, path: &str, secret: &str) -> Option<String> {
        match json {
            Value::String(s) if s == secret => Some(if path.is_empty() {
                "".to_string()
            } else {
                path.to_string()
            }),
            Value::Object(map) => {
                for (k, v) in map {
                    let next = format!("{path}/{}", escape_pointer_token(k));
                    if let Some(found) = walk(v, &next, secret) {
                        return Some(found);
                    }
                }
                None
            }
            Value::Array(arr) => {
                for (i, v) in arr.iter().enumerate() {
                    let next = format!("{path}/{i}");
                    if let Some(found) = walk(v, &next, secret) {
                        return Some(found);
                    }
                }
                None
            }
            _ => None,
        }
    }
    walk(json, "", secret)
}

fn escape_pointer_token(s: &str) -> String {
    s.replace('~', "~0").replace('/', "~1")
}

/// Carrier on which `secret` was sent in this request, if any.
pub fn request_carrier_of(flow: &CaptureRecord, secret: &str) -> Option<Carrier> {
    for (name, value) in &flow.request_headers {
        let lower = name.to_ascii_lowercase();
        if lower == "cookie" {
            for part in value.split(';') {
                if let Some((n, v)) = part.trim().split_once('=')
                    && v == secret
                {
                    return Some(Carrier::cookie(n.trim()));
                }
            }
        }
        if let Some(rest) = value.strip_prefix("Bearer ")
            && rest == secret
        {
            return Some(Carrier::header(name));
        }
        if let Some(rest) = value.strip_prefix("Basic ")
            && rest == secret
        {
            return Some(Carrier::header(name));
        }
        if value == secret {
            return Some(Carrier::header(name));
        }
    }
    if let Ok(url) = url::Url::parse(&flow.url) {
        for (k, v) in url.query_pairs() {
            if v.as_ref() == secret {
                return Some(Carrier::query(k.as_ref()));
            }
        }
    }
    if let Some(body) = flow.text_request()
        && let Ok(json) = serde_json::from_str::<Value>(body)
        && let Some(ptr) = json_pointer_holding(&json, secret)
    {
        return Some(Carrier::body(ptr));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture::{Body, CaptureRecord, Transport};
    use std::collections::HashMap;

    #[allow(clippy::too_many_arguments)]
    fn flow(
        id: &str,
        seq: u64,
        method: &str,
        url: &str,
        req_headers: HashMap<String, String>,
        status: u16,
        resp_headers: HashMap<String, String>,
        resp_body: Option<&str>,
    ) -> CaptureRecord {
        CaptureRecord {
            id: id.into(),
            transport: Transport::Http,
            url: url.into(),
            method: Some(method.into()),
            request_headers: req_headers,
            request_body: None,
            status: Some(status),
            response_headers: Some(resp_headers),
            response_body: resp_body.map(Body::text),
            resource_type: Some("Fetch".into()),
            sequence: seq,
            timestamp_ms: None,
            ws_request_id: None,
            ws_opcode: None,
            direction: None,
        }
    }

    #[test]
    fn set_cookie_on_redirect_hop_is_issuer() {
        let secret = "s3cretSidValue0123456789abcdef";
        let login = flow(
            "login#0",
            0,
            "POST",
            "https://app.example.test/login",
            HashMap::new(),
            302,
            HashMap::from([(
                "set-cookie".into(),
                format!("sid={secret}; Path=/; HttpOnly"),
            )]),
            None,
        );
        let api = flow(
            "api-me",
            2,
            "GET",
            "https://app.example.test/api/me",
            HashMap::from([("cookie".into(), format!("sid={secret}"))]),
            200,
            HashMap::from([("content-type".into(), "application/json".into())]),
            Some(r#"{"ok":true}"#),
        );
        let map = HashMap::from([(
            "login#0".into(),
            "form|https://app.example.test|POST|/login".into(),
        )]);
        let acq = find_issuer(secret, &[login, api], &map);
        match acq {
            Acquisition::Observed {
                op_id,
                request_id,
                response_location,
                ..
            } => {
                assert_eq!(
                    op_id.as_deref(),
                    Some("form|https://app.example.test|POST|/login")
                );
                assert_eq!(request_id, "login#0");
                assert_eq!(response_location, "set-cookie:sid");
            }
            other => panic!("expected observed, got {other:?}"),
        }
    }

    #[test]
    fn already_present_is_not_observed() {
        let secret = "tok_live_abcdefghijklmnopqrstuvwxyz012345";
        let api = flow(
            "api-1",
            0,
            "GET",
            "https://api.example.test/v1/me",
            HashMap::from([("authorization".into(), format!("Bearer {secret}"))]),
            200,
            HashMap::from([("content-type".into(), "application/json".into())]),
            Some(r#"{"id":"acct-1"}"#),
        );
        let acq = find_issuer(secret, &[api], &HashMap::new());
        match acq {
            Acquisition::NotObserved { reason } => {
                assert!(reason.contains("already present"), "{reason}");
            }
            other => panic!("expected not_observed, got {other:?}"),
        }
    }
}
