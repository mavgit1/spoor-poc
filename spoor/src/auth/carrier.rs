//! Where a credential travelled in captured traffic.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Observed credential location. Serializes to `header:authorization`, `cookie:sid`, etc.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Carrier {
    Header { name: String },
    Cookie { name: String },
    Query { name: String },
    Body { pointer: String },
}

impl Carrier {
    pub fn header(name: impl Into<String>) -> Self {
        Self::Header {
            name: name.into().to_ascii_lowercase(),
        }
    }

    pub fn cookie(name: impl Into<String>) -> Self {
        Self::Cookie { name: name.into() }
    }

    pub fn query(name: impl Into<String>) -> Self {
        Self::Query { name: name.into() }
    }

    pub fn body(pointer: impl Into<String>) -> Self {
        let pointer = pointer.into();
        let pointer = if pointer.starts_with('/') {
            pointer
        } else {
            format!("/{pointer}")
        };
        Self::Body { pointer }
    }

    pub fn kind(&self) -> &'static str {
        match self {
            Self::Header { .. } => "header",
            Self::Cookie { .. } => "cookie",
            Self::Query { .. } => "query",
            Self::Body { .. } => "body",
        }
    }

    pub fn name(&self) -> &str {
        match self {
            Self::Header { name } | Self::Cookie { name } | Self::Query { name } => name,
            Self::Body { pointer } => pointer,
        }
    }
}

impl fmt::Display for Carrier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Header { name } => write!(f, "header:{name}"),
            Self::Cookie { name } => write!(f, "cookie:{name}"),
            Self::Query { name } => write!(f, "query:{name}"),
            Self::Body { pointer } => write!(f, "body:{pointer}"),
        }
    }
}

impl FromStr for Carrier {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (kind, rest) = s
            .split_once(':')
            .ok_or_else(|| format!("carrier must be kind:name, got {s}"))?;
        if rest.is_empty() {
            return Err(format!("carrier missing name: {s}"));
        }
        match kind {
            "header" => Ok(Self::header(rest)),
            "cookie" => Ok(Self::cookie(rest)),
            "query" => Ok(Self::query(rest)),
            "body" => Ok(Self::body(rest)),
            other => Err(format!("unknown carrier kind {other}")),
        }
    }
}

impl Serialize for Carrier {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for Carrier {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

/// How the value was framed on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scheme {
    Bearer,
    Basic,
    Opaque,
}

impl Scheme {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Bearer => "Bearer",
            Self::Basic => "Basic",
            Self::Opaque => "opaque",
        }
    }
}

impl fmt::Display for Scheme {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl Serialize for Scheme {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for Scheme {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        match s.as_str() {
            "Bearer" | "bearer" => Ok(Self::Bearer),
            "Basic" | "basic" => Ok(Self::Basic),
            "opaque" | "Opaque" => Ok(Self::Opaque),
            other => Err(serde::de::Error::custom(format!("unknown scheme {other}"))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_string_forms() {
        for raw in [
            "header:authorization",
            "header:x-api-key",
            "cookie:sid",
            "query:access_token",
            "body:/access_token",
        ] {
            let c: Carrier = raw.parse().unwrap();
            assert_eq!(c.to_string(), raw);
            let yaml = serde_yaml_ng::to_string(&c).unwrap();
            let back: Carrier = serde_yaml_ng::from_str(&yaml).unwrap();
            assert_eq!(back, c);
        }
    }
}
