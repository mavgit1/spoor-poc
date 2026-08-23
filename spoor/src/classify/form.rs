use crate::ir::TrafficEntry;

/// Form-urlencoded or multipart form detection. Returns a stable label for the op.
pub fn try_parse_form(entry: &TrafficEntry) -> Option<String> {
    if entry.is_websocket() {
        return None;
    }
    let ct = entry.request_content_type()?.to_ascii_lowercase();
    if ct.contains("application/x-www-form-urlencoded") {
        let body = entry.text_request().unwrap_or("");
        let keys = form_keys(body);
        return Some(form_label(&entry.path, &keys));
    }
    if ct.contains("multipart/form-data") {
        let body = entry.text_request().unwrap_or("");
        let keys = multipart_part_names(body);
        return Some(form_label(&entry.path, &keys));
    }
    None
}

fn form_label(path: &str, keys: &[String]) -> String {
    if keys.is_empty() {
        format!("form:{path}")
    } else {
        let preview: Vec<_> = keys.iter().take(5).map(|s| s.as_str()).collect();
        format!("form:{path}?{}", preview.join("&"))
    }
}

fn form_keys(body: &str) -> Vec<String> {
    let mut keys = Vec::new();
    for pair in body.split('&') {
        if pair.is_empty() {
            continue;
        }
        let key = pair.split('=').next().unwrap_or("").trim();
        if !key.is_empty() && !keys.iter().any(|k| k == key) {
            keys.push(key.to_string());
        }
    }
    keys
}

/// Cheap multipart name scrape — not a full MIME parser.
fn multipart_part_names(body: &str) -> Vec<String> {
    let mut keys = Vec::new();
    for line in body.lines() {
        let lower = line.to_ascii_lowercase();
        if let Some(rest) = lower
            .split("name=\"")
            .nth(1)
            .or_else(|| lower.split("name='").nth(1))
        {
            let name = rest
                .split('"')
                .next()
                .or_else(|| rest.split('\'').next())
                .unwrap_or("")
                .trim();
            // Prefer original casing from line if possible
            if let Some(idx) = line.to_ascii_lowercase().find("name=") {
                let slice = &line[idx + 5..];
                let raw = slice
                    .trim_start_matches(['"', '\''])
                    .split(['"', '\'', ';'])
                    .next()
                    .unwrap_or(name);
                if !raw.is_empty() && !keys.iter().any(|k| k == raw) {
                    keys.push(raw.to_string());
                }
            } else if !name.is_empty() {
                keys.push(name.to_string());
            }
        }
    }
    keys
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::capture::{Body, CaptureRecord, Transport};

    #[test]
    fn parses_urlencoded() {
        let entry = TrafficEntry::from_flow(CaptureRecord {
            id: "1".into(),
            transport: Transport::Http,
            url: "https://api.example.test/login".into(),
            method: Some("POST".into()),
            request_headers: HashMap::from([(
                "content-type".into(),
                "application/x-www-form-urlencoded".into(),
            )]),
            request_body: Some(Body::text("user=a&password=b")),
            status: Some(200),
            response_headers: None,
            response_body: Some(Body::text("ok")),
            resource_type: Some("Document".into()),
            sequence: 0,
            timestamp_ms: None,
            ws_request_id: None,
            ws_opcode: None,
            direction: None,
        })
        .unwrap();
        let label = try_parse_form(&entry).unwrap();
        assert!(label.contains("user"));
        assert!(label.contains("password"));
    }
}
