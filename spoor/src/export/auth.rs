use std::collections::{BTreeMap, HashMap, HashSet};

use serde::Serialize;
use url::Url;

use crate::classify::ClassifiedEntry;
use crate::redact::sensitive_query_param;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AuthVisibility {
    None,
    PublicClientKey,
    BearerSecret,
    SessionSecret,
    CsrfToken,
}

#[derive(Debug, Clone, Serialize)]
pub struct AuthObservation {
    #[serde(rename = "type")]
    pub auth_type: String,
    pub visibility: AuthVisibility,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub example: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

pub fn observe_for_origin(classified: &[ClassifiedEntry], origin: &str) -> Vec<AuthObservation> {
    let entries: Vec<_> = classified
        .iter()
        .filter(|c| c.entry.origin == origin)
        .collect();

    let mut headers_seen: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut public_keys: HashMap<String, (String, usize)> = HashMap::new();
    let mut unauthorized = 0usize;

    for item in &entries {
        if matches!(item.entry.flow.status, Some(401 | 403)) {
            unauthorized += 1;
        }
        if let Ok(url) = Url::parse(&item.entry.flow.url) {
            for (k, v) in url.query_pairs() {
                let name = k.to_string();
                let val = v.to_string();
                if sensitive_query_param(&name, &val) {
                    let entry = public_keys.entry(name).or_insert((val, 0));
                    entry.1 += 1;
                }
            }
        }
        for (k, v) in &item.entry.flow.request_headers {
            headers_seen
                .entry(k.to_ascii_lowercase())
                .or_default()
                .push(v.clone());
        }
    }

    let mut out = Vec::new();

    for (name, (example, count)) in public_keys {
        let visibility = if example.to_ascii_lowercase().starts_with("client-")
            || name.eq_ignore_ascii_case("k")
            || name.to_ascii_lowercase().contains("client")
        {
            AuthVisibility::PublicClientKey
        } else {
            AuthVisibility::SessionSecret
        };
        out.push(AuthObservation {
            auth_type: "query_param".to_string(),
            visibility,
            name,
            example: Some(example),
            note: Some(format!("seen on {count} request(s)")),
        });
    }

    for (name, values) in &headers_seen {
        match name.as_str() {
            "authorization" => {
                let example = values.first().cloned();
                let note = example.as_ref().map(|v| {
                    if v.to_ascii_lowercase().starts_with("bearer ") {
                        "Authorization: Bearer … observed".to_string()
                    } else {
                        "Authorization header observed".to_string()
                    }
                });
                out.push(AuthObservation {
                    auth_type: "header".to_string(),
                    visibility: AuthVisibility::BearerSecret,
                    name: "Authorization".to_string(),
                    example,
                    note,
                });
            }
            "cookie" => {
                let names = cookie_names(values.first());
                out.push(AuthObservation {
                    auth_type: "cookie".to_string(),
                    visibility: AuthVisibility::SessionSecret,
                    name: names.join(", "),
                    example: None,
                    note: Some(format!("Cookie header observed ({})", names.join(", "))),
                });
            }
            "x-csrf-token" | "x-xsrf-token" => out.push(AuthObservation {
                auth_type: "header".to_string(),
                visibility: AuthVisibility::CsrfToken,
                name: name.to_string(),
                example: values.first().cloned(),
                note: Some("CSRF-style header observed on requests".to_string()),
            }),
            "x-api-key" | "api-key" => {
                let example = values.first().cloned();
                out.push(AuthObservation {
                    auth_type: "header".to_string(),
                    visibility: AuthVisibility::PublicClientKey,
                    name: name.to_string(),
                    example,
                    note: None,
                });
            }
            _ => {}
        }
    }

    if unauthorized > 0 {
        out.push(AuthObservation {
            auth_type: "status_signal".to_string(),
            visibility: AuthVisibility::SessionSecret,
            name: "http_401_or_403".to_string(),
            example: None,
            note: Some(format!(
                "{unauthorized} call(s) returned 401/403 — endpoint likely requires a signed-in session"
            )),
        });
    }

    if out.is_empty() {
        out.push(AuthObservation {
            auth_type: "none".to_string(),
            visibility: AuthVisibility::None,
            name: "none".to_string(),
            example: None,
            note: Some("No credentials or 401/403 signals observed in this session".into()),
        });
    }

    out
}

pub fn session_auth_warnings(
    classified: &[ClassifiedEntry],
    origins: &HashSet<String>,
) -> Vec<String> {
    let mut warnings = Vec::new();
    for origin in origins {
        let auth = observe_for_origin(classified, origin);
        if auth.iter().any(|a| {
            matches!(
                a.visibility,
                AuthVisibility::BearerSecret | AuthVisibility::SessionSecret
            )
        }) {
            warnings.push(format!(
                "Session auth observed for {origin} (cookie/bearer/401) — examples may be redacted when redact=true"
            ));
        }
    }
    warnings
}

fn cookie_names(cookie_header: Option<&String>) -> Vec<String> {
    let Some(h) = cookie_header else {
        return vec!["(session)".to_string()];
    };
    h.split(';')
        .filter_map(|part| part.trim().split('=').next())
        .map(|n| n.to_string())
        .collect()
}
