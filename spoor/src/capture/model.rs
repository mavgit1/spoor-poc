use std::collections::HashMap;

use base64::Engine;
use base64::prelude::BASE64_STANDARD;
use serde::de::{self, MapAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Max retained binary body size (request or response).
pub const MAX_BINARY_BYTES: usize = 256 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Transport {
    #[default]
    Http,
    WebSocket,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OmitReason {
    Image,
    Font,
    Media,
    Stylesheet,
    Script,
    TooLarge,
    Cap,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Direction {
    /// HTTP request or WS frame sent by the page.
    Outbound,
    /// HTTP response or WS frame received by the page.
    Inbound,
}

/// Typed payload — text APIs, retained binary, or intentionally omitted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Body {
    Text(String),
    Bytes {
        data: Vec<u8>,
        content_type: Option<String>,
    },
    Omitted {
        reason: OmitReason,
    },
}

impl Body {
    pub fn text(s: impl Into<String>) -> Self {
        Self::Text(s.into())
    }

    pub fn as_text(&self) -> Option<&str> {
        match self {
            Self::Text(s) => Some(s.as_str()),
            _ => None,
        }
    }

    pub fn is_bytes(&self) -> bool {
        matches!(self, Self::Bytes { .. })
    }

    pub fn is_omitted(&self) -> bool {
        matches!(self, Self::Omitted { .. })
    }

    pub fn from_raw_bytes(data: Vec<u8>, content_type: Option<String>) -> Self {
        if data.len() > MAX_BINARY_BYTES {
            return Self::Omitted {
                reason: OmitReason::TooLarge,
            };
        }
        // Prefer UTF-8 text when valid and mostly printable.
        if let Ok(s) = std::str::from_utf8(&data) {
            if looks_like_text(s) {
                return Self::Text(s.to_string());
            }
        }
        Self::Bytes { data, content_type }
    }

    pub fn from_cdp_body(body: &str, base64_encoded: bool, content_type: Option<String>) -> Self {
        if base64_encoded {
            match BASE64_STANDARD.decode(body) {
                Ok(bytes) => Self::from_raw_bytes(bytes, content_type),
                Err(_) => Self::Text(body.to_string()),
            }
        } else {
            Self::Text(body.to_string())
        }
    }
}

fn looks_like_text(s: &str) -> bool {
    if s.is_empty() {
        return true;
    }
    let sample: String = s.chars().take(512).collect();
    let non_text = sample
        .chars()
        .filter(|c| matches!(c, '\u{0000}'..='\u{0008}' | '\u{000B}' | '\u{000C}' | '\u{000E}'..='\u{001F}'))
        .count();
    non_text == 0
}

impl Serialize for Body {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        match self {
            Body::Text(t) => {
                let mut map = serializer.serialize_map(Some(2))?;
                map.serialize_entry("kind", "text")?;
                map.serialize_entry("text", t)?;
                map.end()
            }
            Body::Bytes { data, content_type } => {
                let mut map = serializer.serialize_map(Some(3))?;
                map.serialize_entry("kind", "bytes")?;
                map.serialize_entry("base64", &BASE64_STANDARD.encode(data))?;
                if let Some(ct) = content_type {
                    map.serialize_entry("content_type", ct)?;
                }
                map.end()
            }
            Body::Omitted { reason } => {
                let mut map = serializer.serialize_map(Some(2))?;
                map.serialize_entry("kind", "omitted")?;
                map.serialize_entry("reason", reason)?;
                map.end()
            }
        }
    }
}

impl<'de> Deserialize<'de> for Body {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct BodyVisitor;
        impl<'de> Visitor<'de> for BodyVisitor {
            type Value = Body;

            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("a string or a body object with kind")
            }

            fn visit_str<E: de::Error>(self, v: &str) -> Result<Body, E> {
                Ok(Body::Text(v.to_string()))
            }

            fn visit_string<E: de::Error>(self, v: String) -> Result<Body, E> {
                Ok(Body::Text(v))
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Body, A::Error> {
                let mut kind: Option<String> = None;
                let mut text: Option<String> = None;
                let mut base64_data: Option<String> = None;
                let mut content_type: Option<String> = None;
                let mut reason: Option<OmitReason> = None;

                while let Some(key) = map.next_key::<String>()? {
                    match key.as_str() {
                        "kind" => kind = Some(map.next_value()?),
                        "text" | "value" => text = Some(map.next_value()?),
                        "base64" => base64_data = Some(map.next_value()?),
                        "content_type" => content_type = Some(map.next_value()?),
                        "reason" => reason = Some(map.next_value()?),
                        _ => {
                            let _: de::IgnoredAny = map.next_value()?;
                        }
                    }
                }

                match kind.as_deref() {
                    Some("text") | None if text.is_some() => {
                        Ok(Body::Text(text.unwrap_or_default()))
                    }
                    Some("bytes") => {
                        let b64 = base64_data.ok_or_else(|| de::Error::missing_field("base64"))?;
                        let data = BASE64_STANDARD
                            .decode(&b64)
                            .map_err(|e| de::Error::custom(e.to_string()))?;
                        Ok(Body::Bytes { data, content_type })
                    }
                    Some("omitted") => Ok(Body::Omitted {
                        reason: reason.unwrap_or(OmitReason::Other),
                    }),
                    Some(other) => Err(de::Error::unknown_variant(
                        other,
                        &["text", "bytes", "omitted"],
                    )),
                    None => Err(de::Error::missing_field("kind")),
                }
            }
        }

        deserializer.deserialize_any(BodyVisitor)
    }
}

/// Unified capture record for HTTP exchanges and WebSocket frames.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaptureRecord {
    pub id: String,
    #[serde(default)]
    pub transport: Transport,
    pub url: String,
    #[serde(default)]
    pub method: Option<String>,
    #[serde(default)]
    pub request_headers: HashMap<String, String>,
    #[serde(default)]
    pub request_body: Option<Body>,
    pub status: Option<u16>,
    pub response_headers: Option<HashMap<String, String>>,
    pub response_body: Option<Body>,
    pub resource_type: Option<String>,
    #[serde(default)]
    pub sequence: u64,
    #[serde(default)]
    pub timestamp_ms: Option<u64>,
    #[serde(default)]
    pub ws_request_id: Option<String>,
    #[serde(default)]
    pub ws_opcode: Option<String>,
    #[serde(default)]
    pub direction: Option<Direction>,
}

impl CaptureRecord {
    pub fn http_method(&self) -> &str {
        self.method.as_deref().unwrap_or("")
    }

    pub fn text_request(&self) -> Option<&str> {
        self.request_body.as_ref().and_then(Body::as_text)
    }

    pub fn text_response(&self) -> Option<&str> {
        self.response_body.as_ref().and_then(Body::as_text)
    }

    pub fn header_ci<'a>(headers: &'a HashMap<String, String>, name: &str) -> Option<&'a str> {
        headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    pub fn request_content_type(&self) -> Option<&str> {
        Self::header_ci(&self.request_headers, "content-type")
    }

    pub fn response_content_type(&self) -> Option<&str> {
        self.response_headers
            .as_ref()
            .and_then(|h| Self::header_ci(h, "content-type"))
    }

    pub fn has_binary_payload(&self) -> bool {
        self.request_body.as_ref().is_some_and(Body::is_bytes)
            || self.response_body.as_ref().is_some_and(Body::is_bytes)
    }

    pub fn has_omitted_payload(&self) -> bool {
        self.request_body.as_ref().is_some_and(Body::is_omitted)
            || self.response_body.as_ref().is_some_and(Body::is_omitted)
    }
}

/// Back-compat name used across the crate and fixtures.
pub type CapturedFlow = CaptureRecord;

/// Whether a response content-type should be stored as media omission vs retained.
pub fn media_omit_reason(content_type: &str) -> Option<OmitReason> {
    let ct = content_type.to_ascii_lowercase();
    if ct.starts_with("image/") {
        Some(OmitReason::Image)
    } else if ct.starts_with("font/") {
        Some(OmitReason::Font)
    } else if ct.starts_with("video/") || ct.starts_with("audio/") {
        Some(OmitReason::Media)
    } else {
        None
    }
}

pub fn resource_type_omit_reason(resource_type: &str) -> Option<OmitReason> {
    let lower = resource_type.to_ascii_lowercase();
    if lower.contains("image") {
        Some(OmitReason::Image)
    } else if lower.contains("font") {
        Some(OmitReason::Font)
    } else if lower.contains("media") {
        Some(OmitReason::Media)
    } else if lower.contains("stylesheet") {
        Some(OmitReason::Stylesheet)
    } else if lower.contains("script") {
        Some(OmitReason::Script)
    } else {
        None
    }
}

/// True for content-types we retain even when binary (API-ish).
pub fn is_retainable_binary_content_type(ct: &str) -> bool {
    let ct = ct.to_ascii_lowercase();
    let base = ct.split(';').next().unwrap_or("").trim();
    base == "application/octet-stream"
        || base.starts_with("application/grpc")
        || base == "application/x-protobuf"
        || base == "application/protobuf"
        || base == "application/vnd.google.protobuf"
        || base.ends_with("+protobuf")
}
