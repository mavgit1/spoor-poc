//! Observe credential *shape* from captured traffic. Never invent a login story.

use std::collections::{BTreeMap, HashMap, HashSet};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use url::Url;

use crate::capture::CaptureRecord;
use crate::classify::ClassifiedEntry;
use crate::redact::sensitive_query_param;

use super::acquisition::{self, Acquisition};
use super::carrier::{Carrier, Scheme};
use super::fingerprint::{self, Fingerprint};
use super::jwt::{self, JwtShape};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RotationMode {
    Constant,
    Rotated,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rotation {
    pub mode: RotationMode,
    pub distinct_values: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthObservation {
    pub carrier: Carrier,
    pub scheme: Scheme,
    pub fingerprint: Fingerprint,
    pub rotation: Rotation,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub jwt: Option<JwtShape>,
    pub acquisition: Acquisition,
    /// Present only when full-value disclosure was requested.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub example: Option<String>,
    /// Held for pack-wide scrubbing; never serialized.
    #[serde(skip)]
    pub raw_values: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthDocument {
    pub surface_id: String,
    pub origin: String,
    /// Factual: this CLI exists and takes this surface id.
    pub interactive_command: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub credentials: Vec<AuthObservation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unauthorized_count: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

impl AuthDocument {
    pub fn secrets_for_scrub(&self) -> Vec<String> {
        let mut out = Vec::new();
        for cred in &self.credentials {
            out.extend(cred.raw_values.iter().cloned());
            if let Some(ex) = &cred.example {
                out.push(ex.clone());
            }
        }
        out.sort();
        out.dedup();
        out
    }
}

#[derive(Default)]
struct Bucket {
    values: Vec<String>,
    scheme: Option<Scheme>,
}

/// Shape-only by default (`disclose_values = false`).
pub fn observe_for_origin(
    classified: &[ClassifiedEntry],
    origin: &str,
    flows: &[CaptureRecord],
    flow_to_op: &HashMap<String, String>,
    surface_id: &str,
    disclose_values: bool,
) -> AuthDocument {
    let origin_entries: Vec<&ClassifiedEntry> = classified
        .iter()
        .filter(|c| c.entry.origin == origin)
        .collect();

    let mut buckets: BTreeMap<String, Bucket> = BTreeMap::new();
    let mut unauthorized = 0usize;
    let mut capture_ms: Option<u64> = None;

    for item in &origin_entries {
        if matches!(item.entry.flow.status, Some(401 | 403)) {
            unauthorized += 1;
        }
        if let Some(ts) = item.entry.flow.timestamp_ms {
            capture_ms = Some(capture_ms.map_or(ts, |t: u64| t.max(ts)));
        }
        collect_from_flow(&item.entry.flow, &mut buckets);
    }

    // Redirect hops (and other unclassified records) still carry Set-Cookie / later Cookie headers.
    for flow in flows {
        if origin_of(&flow.url).as_deref() == Some(origin) {
            collect_from_flow(flow, &mut buckets);
            if let Some(ts) = flow.timestamp_ms {
                capture_ms = Some(capture_ms.map_or(ts, |t: u64| t.max(ts)));
            }
        }
    }

    let mut credentials = Vec::new();
    for (carrier_key, bucket) in buckets {
        let Ok(carrier) = carrier_key.parse::<Carrier>() else {
            continue;
        };
        let mut distinct: Vec<String> = bucket.values;
        distinct.sort();
        distinct.dedup();
        if distinct.is_empty() {
            continue;
        }
        let representative = distinct[0].clone();
        let scheme = bucket.scheme.unwrap_or(Scheme::Opaque);
        let jwt = jwt::inspect(&strip_scheme(&representative), capture_ms);
        let rotation = Rotation {
            mode: if distinct.len() == 1 {
                RotationMode::Constant
            } else {
                RotationMode::Rotated
            },
            distinct_values: distinct.len(),
        };
        let acquisition = acquisition::find_issuer(&representative, flows, flow_to_op);
        let example = if disclose_values {
            Some(representative.clone())
        } else {
            None
        };
        credentials.push(AuthObservation {
            carrier,
            scheme,
            fingerprint: fingerprint::fingerprint(&strip_scheme(&representative)),
            rotation,
            jwt,
            acquisition,
            example,
            raw_values: distinct,
        });
    }

    credentials.sort_by_key(|a| a.carrier.to_string());

    let note = if credentials.is_empty() && unauthorized == 0 {
        Some("No credentials or 401/403 signals observed in this session".into())
    } else {
        None
    };

    AuthDocument {
        surface_id: surface_id.to_string(),
        origin: origin.to_string(),
        interactive_command: format!("spoor auth --surface {surface_id}"),
        credentials,
        unauthorized_count: (unauthorized > 0).then_some(unauthorized),
        note,
    }
}

fn collect_from_flow(flow: &CaptureRecord, buckets: &mut BTreeMap<String, Bucket>) {
    for (name, value) in &flow.request_headers {
        let lower = name.to_ascii_lowercase();
        match lower.as_str() {
            "authorization" => {
                let (scheme, token) = split_authorization(value);
                record(buckets, Carrier::header("authorization"), scheme, token);
            }
            "cookie" => {
                for part in value.split(';') {
                    let part = part.trim();
                    if part.is_empty() {
                        continue;
                    }
                    let (n, v) = part.split_once('=').unwrap_or((part, ""));
                    if v.is_empty() {
                        continue;
                    }
                    record(
                        buckets,
                        Carrier::cookie(n.trim()),
                        Scheme::Opaque,
                        v.to_string(),
                    );
                }
            }
            "x-api-key" | "api-key" | "x-auth-token" => {
                record(
                    buckets,
                    Carrier::header(&lower),
                    Scheme::Opaque,
                    value.clone(),
                );
            }
            "x-csrf-token" | "x-xsrf-token" => {
                record(
                    buckets,
                    Carrier::header(&lower),
                    Scheme::Opaque,
                    value.clone(),
                );
            }
            _ => {}
        }
    }

    if let Ok(url) = Url::parse(&flow.url) {
        for (k, v) in url.query_pairs() {
            let name = k.to_string();
            let val = v.to_string();
            if sensitive_query_param(&name, &val) {
                record(buckets, Carrier::query(&name), Scheme::Opaque, val);
            }
        }
    }

    if let Some(body) = flow.text_request()
        && let Ok(json) = serde_json::from_str::<Value>(body)
    {
        collect_body_tokens(&json, "", buckets);
    }
}

fn collect_body_tokens(json: &Value, pointer: &str, buckets: &mut BTreeMap<String, Bucket>) {
    const TOKEN_KEYS: &[&str] = &[
        "access_token",
        "refresh_token",
        "id_token",
        "token",
        "api_key",
        "apikey",
        "client_secret",
    ];
    match json {
        Value::Object(map) => {
            for (k, v) in map {
                let next = format!("{pointer}/{}", k.replace('~', "~0").replace('/', "~1"));
                let key_l = k.to_ascii_lowercase();
                if TOKEN_KEYS.contains(&key_l.as_str())
                    && let Some(s) = v.as_str()
                    && !s.is_empty()
                {
                    record(buckets, Carrier::body(&next), Scheme::Opaque, s.to_string());
                } else {
                    collect_body_tokens(v, &next, buckets);
                }
            }
        }
        Value::Array(arr) => {
            for (i, v) in arr.iter().enumerate() {
                collect_body_tokens(v, &format!("{pointer}/{i}"), buckets);
            }
        }
        _ => {}
    }
}

fn record(buckets: &mut BTreeMap<String, Bucket>, carrier: Carrier, scheme: Scheme, value: String) {
    if value.is_empty() {
        return;
    }
    let bucket = buckets.entry(carrier.to_string()).or_default();
    if bucket.scheme.is_none() {
        bucket.scheme = Some(scheme);
    }
    bucket.values.push(value);
}

fn split_authorization(value: &str) -> (Scheme, String) {
    let v = value.trim();
    if let Some(rest) = v
        .strip_prefix("Bearer ")
        .or_else(|| v.strip_prefix("bearer "))
    {
        (Scheme::Bearer, rest.trim().to_string())
    } else if let Some(rest) = v
        .strip_prefix("Basic ")
        .or_else(|| v.strip_prefix("basic "))
    {
        (Scheme::Basic, rest.trim().to_string())
    } else {
        (Scheme::Opaque, v.to_string())
    }
}

fn strip_scheme(value: &str) -> String {
    split_authorization(value).1
}

fn origin_of(url: &str) -> Option<String> {
    let parsed = Url::parse(url).ok()?;
    let host = parsed.host_str()?;
    Some(format!("{}://{host}", parsed.scheme()))
}

/// Latest observed value for `carrier` on `origin`, if any request carried it.
pub fn collect_from_flow_for_carrier(
    flows: &[CaptureRecord],
    origin: &str,
    carrier: &Carrier,
) -> Option<String> {
    let mut buckets: BTreeMap<String, Bucket> = BTreeMap::new();
    for flow in flows {
        if origin_of(&flow.url).as_deref() == Some(origin) {
            collect_from_flow(flow, &mut buckets);
        }
    }
    buckets
        .get(&carrier.to_string())
        .and_then(|b| b.values.last().cloned())
        .filter(|s| !s.is_empty())
}

pub fn session_auth_warnings(
    classified: &[ClassifiedEntry],
    origins: &HashSet<String>,
) -> Vec<String> {
    let empty_map = HashMap::new();
    let mut warnings = Vec::new();
    for origin in origins {
        let doc = observe_for_origin(classified, origin, &[], &empty_map, "surface", false);
        if !doc.credentials.is_empty() || doc.unauthorized_count.is_some() {
            warnings.push(format!(
                "Session auth observed for {origin} — pack records credential shape only; live values are not included"
            ));
        }
    }
    warnings
}

/// Replace every observed secret in `text`. Used so op examples cannot leak tokens
/// when `redact` is off. Prefix-sized fragments are not secrets.
pub fn scrub_secrets(text: &str, secrets: &[String]) -> String {
    let mut out = text.to_string();
    let mut secrets: Vec<&String> = secrets.iter().filter(|s| s.chars().count() >= 8).collect();
    secrets.sort_by_key(|s| std::cmp::Reverse(s.len()));
    for secret in secrets {
        if out.contains(secret.as_str()) {
            out = out.replace(secret.as_str(), "[CREDENTIAL]");
        }
        let encoded: String = url::form_urlencoded::byte_serialize(secret.as_bytes()).collect();
        if encoded != *secret && out.contains(&encoded) {
            out = out.replace(&encoded, "[CREDENTIAL]");
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scrub_replaces_long_secrets_only() {
        let text = "Authorization: Bearer tok_live_abcdefghijklmnopqrstuvwxyz012345 and ok";
        let out = scrub_secrets(
            text,
            &[
                "tok_live_abcdefghijklmnopqrstuvwxyz012345".into(),
                "ok".into(),
            ],
        );
        assert!(out.contains("[CREDENTIAL]"), "{out}");
        assert!(
            !out.contains("tok_live_abcdefghijklmnopqrstuvwxyz012345"),
            "{out}"
        );
        assert!(out.contains(" and ok"), "{out}");
    }
}
