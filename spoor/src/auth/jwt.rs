//! Decode JWT header + claim *names* only. No signature verification, no claim values.

use base64::Engine;
use base64::engine::general_purpose::{URL_SAFE, URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JwtShape {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub alg: Option<String>,
    pub claim_names: Vec<String>,
    /// Derived from `iat`/`exp` when both are numeric — never the raw timestamps.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lifetime: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expired_at_capture: Option<bool>,
}

/// A compact JWT is three base64url segments separated by `.`.
pub fn looks_like_jwt(value: &str) -> bool {
    let mut parts = value.split('.');
    let Some(h) = parts.next() else {
        return false;
    };
    let Some(p) = parts.next() else {
        return false;
    };
    let Some(s) = parts.next() else {
        return false;
    };
    parts.next().is_none()
        && !h.is_empty()
        && !p.is_empty()
        && !s.is_empty()
        && h.starts_with("eyJ")
}

pub fn inspect(value: &str, capture_time_ms: Option<u64>) -> Option<JwtShape> {
    if !looks_like_jwt(value) {
        return None;
    }
    let mut parts = value.split('.');
    let header_b64 = parts.next()?;
    let payload_b64 = parts.next()?;

    let header = decode_json(header_b64)?;
    let payload = decode_json(payload_b64)?;

    let alg = header
        .get("alg")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let mut claim_names: Vec<String> = payload
        .as_object()
        .map(|m| m.keys().cloned().collect())
        .unwrap_or_default();
    claim_names.sort();

    let iat = payload.get("iat").and_then(json_i64);
    let exp = payload.get("exp").and_then(json_i64);
    let lifetime = match (iat, exp) {
        (Some(i), Some(e)) if e >= i => Some(format_ttl(e - i)),
        _ => None,
    };
    let expired_at_capture = match (exp, capture_time_ms) {
        (Some(e), Some(ms)) => Some((ms / 1000) > e as u64),
        _ => None,
    };

    Some(JwtShape {
        alg,
        claim_names,
        lifetime,
        expired_at_capture,
    })
}

fn decode_json(b64: &str) -> Option<Value> {
    let bytes = URL_SAFE_NO_PAD
        .decode(b64)
        .or_else(|_| URL_SAFE.decode(b64))
        .ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn json_i64(v: &Value) -> Option<i64> {
    v.as_i64()
        .or_else(|| v.as_u64().and_then(|n| i64::try_from(n).ok()))
        .or_else(|| v.as_f64().map(|f| f as i64))
}

fn format_ttl(secs: i64) -> String {
    if secs < 90 {
        format!("≈{secs}s TTL")
    } else if secs < 3600 {
        format!("≈{}m TTL", secs / 60)
    } else if secs < 36 * 3600 {
        format!("≈{}h TTL", secs / 3600)
    } else {
        format!("≈{}d TTL", secs / 86400)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOKEN: &str = "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIiOiJ1c2VyLTQyIiwibmFtZSI6IkFsaWNlIiwiaWF0IjoxNzAwMDAwMDAwLCJleHAiOjE3MDAwMDM2MDAsInJvbGUiOiJhZG1pbiJ9.fakesignature";

    #[test]
    fn decodes_header_and_claim_names_not_values() {
        let shape = inspect(TOKEN, Some(1_700_002_000_000)).unwrap();
        assert_eq!(shape.alg.as_deref(), Some("HS256"));
        assert!(shape.claim_names.iter().any(|n| n == "sub"));
        assert!(shape.claim_names.iter().any(|n| n == "name"));
        assert!(shape.claim_names.iter().any(|n| n == "role"));
        assert_eq!(shape.lifetime.as_deref(), Some("≈1h TTL"));
        assert_eq!(shape.expired_at_capture, Some(false));
        let yaml = serde_yaml_ng::to_string(&shape).unwrap();
        assert!(!yaml.contains("user-42"), "{yaml}");
        assert!(!yaml.contains("Alice"), "{yaml}");
        assert!(!yaml.contains("admin"), "{yaml}");
        assert!(!yaml.contains("1700000000"), "{yaml}");
        assert!(!yaml.contains("1700003600"), "{yaml}");
    }

    #[test]
    fn expired_relative_to_capture_time() {
        let shape = inspect(TOKEN, Some(1_700_004_000_000)).unwrap();
        assert_eq!(shape.expired_at_capture, Some(true));
    }

    #[test]
    fn non_jwt_is_none() {
        assert!(inspect("not-a-jwt", None).is_none());
        assert!(inspect("tok_live_abc", None).is_none());
    }
}
