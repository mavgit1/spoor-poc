use serde_json::Value;

const MAX_DEPTH: usize = 8;
const MAX_STRING_LEN: usize = 800;
const MAX_ARRAY_ITEMS: usize = 25;
const MAX_OBJECT_KEYS: usize = 40;

/// Compact JSON for inline agent pack examples (lossy by design).
/// Full payloads can be handed off via sidecar files — see [`needs_handoff`].
pub fn trim_json(value: &Value) -> Value {
    if looks_like_i18n_bundle(value) {
        return Value::String(i18n_bundle_summary(value));
    }
    trim_value(value, 0)
}

/// True when inline trim would drop material structure — write a sidecar instead.
pub fn needs_handoff(full: &Value, trimmed: &Value) -> bool {
    estimate_size(full) > 8_000
        || estimate_size(full) > estimate_size(trimmed).saturating_mul(2) + 500
}

/// Light cleanup for sidecar handoff: keep structure, only truncate huge strings/HTML.
pub fn handoff_json(value: &Value) -> Value {
    handoff_value(value, 0)
}

fn estimate_size(value: &Value) -> usize {
    serde_json::to_vec(value).map(|v| v.len()).unwrap_or(0)
}

fn looks_like_i18n_bundle(value: &Value) -> bool {
    let Value::Object(map) = value else {
        return false;
    };
    if map.len() < 15 {
        return false;
    }
    if !map.values().all(|v| v.is_string()) {
        return false;
    }
    let dotted = map.keys().filter(|k| k.contains('.')).count();
    dotted * 100 / map.len() >= 80
}

fn i18n_bundle_summary(value: &Value) -> String {
    let Value::Object(map) = value else {
        return "[i18n bundle]".to_string();
    };
    format!(
        "[i18n bundle: {} string entries, keys like {:?}…]",
        map.len(),
        map.keys().next()
    )
}

fn looks_like_html(s: &str) -> bool {
    if s.len() < 200 {
        return false;
    }
    let lower = s.to_ascii_lowercase();
    (lower.contains("<html") || lower.contains("<div") || lower.contains("<script"))
        && lower.contains('>')
}

fn trim_value(value: &Value, depth: usize) -> Value {
    if depth >= MAX_DEPTH {
        return Value::String("[trimmed: max depth]".to_string());
    }
    match value {
        Value::Null | Value::Bool(_) | Value::Number(_) => value.clone(),
        Value::String(s) => {
            if looks_like_html(s) {
                return Value::String(format!("[html snippet, {} chars — trimmed]", s.len()));
            }
            if s.len() <= MAX_STRING_LEN {
                value.clone()
            } else {
                let prefix: String = s.chars().take(MAX_STRING_LEN).collect();
                Value::String(format!(
                    "{prefix}… [trimmed, {} chars total]",
                    s.chars().count()
                ))
            }
        }
        Value::Array(arr) => {
            let items: Vec<Value> = arr
                .iter()
                .take(MAX_ARRAY_ITEMS)
                .map(|v| trim_value(v, depth + 1))
                .collect();
            if arr.len() > MAX_ARRAY_ITEMS {
                let mut out = items;
                out.push(Value::String(format!(
                    "[trimmed: {} more items]",
                    arr.len() - MAX_ARRAY_ITEMS
                )));
                Value::Array(out)
            } else {
                Value::Array(items)
            }
        }
        Value::Object(map) => {
            let mut out = serde_json::Map::new();
            let total = map.len();
            for (i, (k, v)) in map.iter().enumerate() {
                if i >= MAX_OBJECT_KEYS {
                    out.insert(
                        "_trimmed_keys".into(),
                        Value::String(format!(
                            "[trimmed: {} more keys — see example_*_ref sidecar]",
                            total - MAX_OBJECT_KEYS
                        )),
                    );
                    break;
                }
                out.insert(k.clone(), trim_value(v, depth + 1));
            }
            Value::Object(out)
        }
    }
}

fn handoff_value(value: &Value, depth: usize) -> Value {
    const HANDOFF_DEPTH: usize = 12;
    const HANDOFF_STRING: usize = 8_000;
    const HANDOFF_ARRAY: usize = 200;

    if depth >= HANDOFF_DEPTH {
        return Value::String("[handoff: max depth]".into());
    }
    match value {
        Value::Null | Value::Bool(_) | Value::Number(_) => value.clone(),
        Value::String(s) => {
            if looks_like_html(s) {
                return Value::String(format!("[html snippet, {} chars]", s.len()));
            }
            if s.chars().count() <= HANDOFF_STRING {
                value.clone()
            } else {
                let prefix: String = s.chars().take(HANDOFF_STRING).collect();
                Value::String(format!(
                    "{prefix}… [handoff truncated, {} chars total]",
                    s.chars().count()
                ))
            }
        }
        Value::Array(arr) => {
            let items: Vec<Value> = arr
                .iter()
                .take(HANDOFF_ARRAY)
                .map(|v| handoff_value(v, depth + 1))
                .collect();
            if arr.len() > HANDOFF_ARRAY {
                let mut out = items;
                out.push(Value::String(format!(
                    "[handoff: {} more items]",
                    arr.len() - HANDOFF_ARRAY
                )));
                Value::Array(out)
            } else {
                Value::Array(items)
            }
        }
        Value::Object(map) => {
            let mut out = serde_json::Map::new();
            for (k, v) in map {
                out.insert(k.clone(), handoff_value(v, depth + 1));
            }
            Value::Object(out)
        }
    }
}
