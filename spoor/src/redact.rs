use regex::Regex;
use serde_json::Value;

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

    pub fn redact(&self, value: &mut Value) {
        match value {
            Value::Object(map) => {
                for (key, val) in map.iter_mut() {
                    if self.fields.contains(key) {
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn redact_nested_field() {
        let r = Redactor::new(&[], &["token".to_string()]).unwrap();
        let mut v = json!({"user": {"token": "secret123", "name": "Alice"}});
        r.redact(&mut v);
        assert_eq!(v, json!({"user": {"token": "[REDACTED]", "name": "Alice"}}));
    }
}
