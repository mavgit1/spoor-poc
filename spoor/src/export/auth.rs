use std::collections::{BTreeMap, HashMap, HashSet};

use serde::Serialize;
use url::Url;

use crate::classify::ClassifiedEntry;

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

    for item in &entries {
        if let Ok(url) = Url::parse(&item.entry.flow.url) {
            for (k, v) in url.query_pairs() {
                let name = k.to_string();
                if is_auth_query_param(&name) {
                    let entry = public_keys.entry(name).or_insert((v.to_string(), 0));
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
        out.push(AuthObservation {
            auth_type: "query_param".to_string(),
            visibility: AuthVisibility::PublicClientKey,
            name,
            example: Some(example),
            note: Some(format!("seen on {count} request(s)")),
        });
    }

    for (name, values) in &headers_seen {
        match name.as_str() {
            "authorization" => {
                let example = values.first().cloned();
                let note = example.as_ref().and_then(|v| {
                    if v.to_ascii_lowercase().starts_with("bearer ") {
                        Some("Bearer token — redact in shared exports".to_string())
                    } else {
                        Some("Authorization header present".to_string())
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
                    note: Some("Cookie values redacted in exports by default".to_string()),
                });
            }
            "x-csrf-token" | "x-xsrf-token" => out.push(AuthObservation {
                auth_type: "header".to_string(),
                visibility: AuthVisibility::CsrfToken,
                name: name.to_string(),
                example: values.first().cloned(),
                note: Some("Often required on mutations after page load".to_string()),
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

    if out.is_empty() {
        out.push(AuthObservation {
            auth_type: "none".to_string(),
            visibility: AuthVisibility::None,
            name: "none".to_string(),
            example: None,
            note: None,
        });
    }

    out
}

pub fn session_auth_warnings(classified: &[ClassifiedEntry], origins: &HashSet<String>) -> Vec<String> {
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
                "Session auth detected for {origin} — secrets redacted when redact=true"
            ));
        }
    }
    warnings
}

pub(crate) fn is_auth_query_param(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    n == "api_key"
        || n == "apikey"
        || n == "api-key"
        || n == "access_token"
        || n == "refresh_token"
        || n == "token"
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
