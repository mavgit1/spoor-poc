use serde_json::Value;

const MAX_DEPTH: usize = 8;
const MAX_STRING_LEN: usize = 800;
const MAX_ARRAY_ITEMS: usize = 25;

pub fn trim_json(value: &Value) -> Value {
    if looks_like_i18n_bundle(value) {
        return Value::String(i18n_bundle_summary(value));
    }
    trim_value(value, 0)
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

fn trim_value(value: &Value, depth: usize) -> Value {
    if depth >= MAX_DEPTH {
        return Value::String("[trimmed: max depth]".to_string());
    }
    match value {
        Value::Null | Value::Bool(_) | Value::Number(_) => value.clone(),
        Value::String(s) => {
            if s.len() <= MAX_STRING_LEN {
                value.clone()
            } else {
                Value::String(format!(
                    "{}… [trimmed, {} chars total]",
                    &s[..MAX_STRING_LEN],
                    s.len()
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
            for (k, v) in map {
                out.insert(k.clone(), trim_value(v, depth + 1));
            }
            Value::Object(out)
        }
    }
}
