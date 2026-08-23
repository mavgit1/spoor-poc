use regex::Regex;
use serde_json::Value;

/// Field names (case-insensitive) whose values become `[REDACTED]` when redact is on.
pub const DEFAULT_FIELDS: &[&str] = &[
    "token",
    "access_token",
    "refresh_token",
    "id_token",
    "password",
    "passwd",
    "secret",
    "authorization",
    "api_key",
    "apikey",
    "api-key",
    "client_secret",
    "clientsecret",
    "session",
    "sessionid",
    "session_id",
    "cookie",
    "email",
    "emailaddress",
    "email_address",
    "application_email",
    "mail",
    "phone",
    "phonenumber",
    "phone_number",
    "mobile",
    "msisdn",
    "ip",
    "ipaddress",
    "ip_address",
    "client_ip",
];

/// Value patterns scrubbed anywhere in JSON strings when redact is on.
pub fn default_patterns() -> Vec<String> {
    vec![
        // JWT
        r"^eyJ[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+$".into(),
        // Email
        r"(?i)^[a-z0-9._%+\-]+@[a-z0-9.\-]+\.[a-z]{2,}$".into(),
        // IPv4
        r"^(?:\d{1,3}\.){3}\d{1,3}$".into(),
        // Statsig / similar public client keys
        r"(?i)^client-[a-z0-9_-]{8,}$".into(),
        // E.164-ish / long digit phones
        r"^\+?\d[\d\s\-()]{8,}$".into(),
    ]
}

pub struct Redactor {
    patterns: Vec<Regex>,
    fields: Vec<String>,
}

impl Redactor {
    pub fn new(patterns: &[String], fields: &[String]) -> Result<Self, regex::Error> {
        let patterns = patterns
            .iter()
            .map(|p| Regex::new(p))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self {
            patterns,
            fields: fields.to_vec(),
        })
    }

    /// Agent-pack defaults used by export when `redact=true`.
    pub fn for_agent_pack() -> Self {
        let fields: Vec<String> = DEFAULT_FIELDS.iter().map(|s| (*s).to_string()).collect();
        Self::new(&default_patterns(), &fields).expect("built-in redact patterns compile")
    }

    pub fn redact(&self, value: &mut Value) {
        match value {
            Value::Object(map) => {
                for (key, val) in map.iter_mut() {
                    if self.field_match(key) {
                        *val = Value::String("[REDACTED]".to_string());
                    } else {
                        self.redact(val);
                    }
                }
            }
            Value::Array(arr) => {
                for item in arr.iter_mut() {
                    self.redact(item);
                }
            }
            Value::String(s) if self.patterns.iter().any(|p| p.is_match(s)) => {
                *value = Value::String("[REDACTED]".to_string());
            }
            _ => {}
        }
    }

    /// Scrub auth-ish / PII query params in a URL string (for addressing examples).
    pub fn redact_url(&self, url: &str) -> String {
        let Ok(mut parsed) = url::Url::parse(url) else {
            return url.to_string();
        };
        let pairs: Vec<(String, String)> = parsed
            .query_pairs()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        if pairs.is_empty() {
            return url.to_string();
        }
        let mut parts: Vec<String> = Vec::with_capacity(pairs.len());
        for (k, v) in pairs {
            let scrub = sensitive_query_param(&k, &v)
                || self.field_match(&k)
                || self.patterns.iter().any(|p| p.is_match(&v));
            if scrub {
                parts.push(format!("{k}=[REDACTED]"));
            } else {
                parts.push(format!("{k}={v}"));
            }
        }
        parsed.set_query(Some(&parts.join("&")));
        parsed.to_string()
    }

    fn field_match(&self, key: &str) -> bool {
        let lower = key.to_ascii_lowercase().replace('-', "_");
        self.fields.iter().any(|f| {
            let f = f.to_ascii_lowercase().replace('-', "_");
            lower == f || lower.ends_with(&format!("_{f}")) || (f.len() >= 5 && lower.contains(&f))
        })
    }
}

/// Query keys that carry credentials or session material (shared with auth observation).
pub fn sensitive_query_param(name: &str, value: &str) -> bool {
    let n = name.to_ascii_lowercase();
    match n.as_str() {
        "api_key" | "apikey" | "api-key" | "access_token" | "refresh_token" | "token"
        | "client_secret" | "clientsecret" | "auth" | "authorization" => true,
        "client" | "client_id" | "clientid" | "sdk_key" | "sdkkey" => true,
        // Statsig-style short keys: only when value looks credential-like.
        "k" => {
            value.len() >= 8
                && (value.to_ascii_lowercase().starts_with("client-")
                    || value.chars().any(|c| c.is_ascii_alphanumeric()))
        }
        // Session-ish single-letter params on WS URLs.
        "s" | "i" => value.len() >= 12,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn redact_nested_field() {
        let r = Redactor::for_agent_pack();
        let mut v = json!({"user": {"token": "secret123", "name": "Alice"}});
        r.redact(&mut v);
        assert_eq!(v, json!({"user": {"token": "[REDACTED]", "name": "Alice"}}));
    }

    #[test]
    fn redact_email_field_and_value() {
        let r = Redactor::for_agent_pack();
        let mut v = json!({"emailAddress": "a@b.com"});
        r.redact(&mut v);
        assert_eq!(v["emailAddress"], json!("[REDACTED]"));
        let mut exact = json!("me@x.org");
        r.redact(&mut exact);
        assert_eq!(exact, json!("[REDACTED]"));
    }

    #[test]
    fn redact_url_query_client_key() {
        let r = Redactor::for_agent_pack();
        let out = r.redact_url("https://api.example/v1?k=client-abc12345&page=1");
        assert!(out.contains("[REDACTED]"), "{out}");
        assert!(out.contains("page=1"), "{out}");
    }
}
