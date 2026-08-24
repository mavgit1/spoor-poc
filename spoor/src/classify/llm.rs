use std::collections::HashMap;

use crate::classify::Protocol;
use crate::ir::TrafficEntry;
use crate::log;

const OPENROUTER_URL: &str = "https://openrouter.ai/api/v1/chat/completions";
const BATCH_SIZE: usize = 20;
/// Hard cap — LLM is for leftovers after parsers, not a full-session classifier.
const MAX_AMBIGUOUS: usize = 60;

pub async fn classify_batch(entries: &[TrafficEntry]) -> Vec<Protocol> {
    let api_key = match std::env::var("OPENROUTER_API_KEY") {
        Ok(k) if !k.is_empty() => k,
        _ => {
            log::debug("OPENROUTER_API_KEY unset — skipping ambiguous traffic");
            return entries.iter().map(|_| Protocol::Noise).collect();
        }
    };

    match classify_batch_inner(entries, &api_key).await {
        Ok(v) => v,
        Err(e) => {
            log::warn(format!("LLM classify failed: {e:#}"));
            entries.iter().map(|_| Protocol::Noise).collect()
        }
    }
}

async fn classify_batch_inner(
    entries: &[TrafficEntry],
    api_key: &str,
) -> anyhow::Result<Vec<Protocol>> {
    if entries.is_empty() {
        return Ok(vec![]);
    }

    let model =
        std::env::var("OPENROUTER_MODEL").unwrap_or_else(|_| "qwen/qwen3.6-flash".to_string());

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(90))
        .build()?;

    // Prefer XHR/Fetch with bodies; drop the rest as Noise without calling the API.
    let mut ranked: Vec<(usize, &TrafficEntry)> = entries.iter().enumerate().collect();
    ranked.sort_by_key(|(_, e)| std::cmp::Reverse(llm_priority(e)));

    let mut out = vec![Protocol::Noise; entries.len()];
    let take_n = ranked.len().min(MAX_AMBIGUOUS);
    if ranked.len() > MAX_AMBIGUOUS {
        log::info(format!(
            "LLM classify: {} ambiguous leftovers, scoring top {MAX_AMBIGUOUS} (rest → noise)",
            ranked.len()
        ));
    }

    let selected: Vec<(usize, &TrafficEntry)> = ranked.into_iter().take(take_n).collect();
    let selected_entries: Vec<TrafficEntry> = selected.iter().map(|(_, e)| (*e).clone()).collect();

    let chunks: Vec<_> = selected_entries.chunks(BATCH_SIZE).collect();
    if chunks.len() > 1 {
        log::info(format!(
            "LLM classify: {} entries in {} batches of ≤{BATCH_SIZE}",
            selected_entries.len(),
            chunks.len()
        ));
    }

    let mut label_offset = 0usize;
    for (batch_idx, chunk) in chunks.iter().enumerate() {
        let labels = classify_one_batch(&client, api_key, &model, chunk).await?;
        if labels.len() != chunk.len() {
            log::warn(format!(
                "LLM classify batch {}: {} of {} entries labeled — unlabeled treated as noise",
                batch_idx + 1,
                labels.len(),
                chunk.len()
            ));
        }
        // Keyed by the id we sent, never by position: a model that omits one
        // item must not shift every label after it onto the wrong entry.
        for i in 0..chunk.len() {
            let orig_idx = selected[label_offset + i].0;
            out[orig_idx] = labels.get(&i).copied().unwrap_or(Protocol::Noise);
        }
        label_offset += chunk.len();
    }

    let kept = out.iter().filter(|p| **p != Protocol::Noise).count();
    log::info(format!(
        "LLM classify done: {kept}/{} labeled non-noise ({} scored)",
        out.len(),
        take_n
    ));
    Ok(out)
}

fn llm_priority(entry: &TrafficEntry) -> usize {
    let mut score = 0usize;
    let rt = entry
        .flow
        .resource_type
        .as_deref()
        .unwrap_or("")
        .to_ascii_lowercase();
    if rt.contains("xhr") || rt.contains("fetch") {
        score += 100;
    }
    if entry
        .request_content_type()
        .is_some_and(|c| c.contains("json"))
    {
        score += 40;
    }
    if entry
        .response_content_type()
        .is_some_and(|c| c.contains("json"))
    {
        score += 40;
    }
    if entry.text_request().is_some_and(|b| b.len() > 20) {
        score += 20;
    }
    if entry.text_response().is_some_and(|b| b.len() > 20) {
        score += 20;
    }
    if entry.path.contains("/api") || entry.path.contains("/v1") || entry.path.contains("/v2") {
        score += 15;
    }
    score
}

async fn classify_one_batch(
    client: &reqwest::Client,
    api_key: &str,
    model: &str,
    entries: &[TrafficEntry],
) -> anyhow::Result<HashMap<usize, Protocol>> {
    let snippets: Vec<serde_json::Value> = entries
        .iter()
        .enumerate()
        .map(|(id, e)| {
            serde_json::json!({
                "id": id,
                "method": e.http_method(),
                "url": e.flow.url,
                "request_content_type": e.request_content_type(),
                "response_content_type": e.response_content_type(),
                "request_body": e.text_request().map(|b| truncate(b, 400)),
                "response_body": e.text_response().map(|b| truncate(b, 400)),
                "response_status": e.flow.status,
                "resource_type": e.flow.resource_type,
            })
        })
        .collect();

    // Only labels for leftovers after parsers — not websocket/grpc (handled earlier).
    let prompt = format!(
        "Classify each HTTP capture as exactly one of: rest, graphql, jsonrpc, form, noise.\n\
         Rules:\n\
         - rest: JSON/HTML API-ish HTTP resource (CRUD, search, config JSON)\n\
         - graphql: GraphQL query/mutation over HTTP\n\
         - jsonrpc: JSON-RPC method/params/id style\n\
         - form: application/x-www-form-urlencoded or multipart form posts\n\
         - noise: analytics beacons, pixels, static junk, unclear non-API\n\
         When unsure between rest and noise, prefer rest if a JSON body or /api/ path is present.\n\
         Echo back the `id` of every input so labels cannot be misaligned.\n\
         Respond with JSON only: \
         {{\"labels\":[{{\"id\":0,\"label\":\"rest\"|\"graphql\"|\"jsonrpc\"|\"form\"|\"noise\"}}, ...]}}\n\
         One object per input, every id present exactly once.\n\n{}",
        serde_json::to_string_pretty(&snippets)?
    );

    let body = serde_json::json!({
        "model": model,
        "messages": [{"role": "user", "content": prompt}],
        "provider": {"sort": "price"},
        "response_format": {"type": "json_object"}
    });

    let resp = client
        .post(OPENROUTER_URL)
        .header("Authorization", format!("Bearer {api_key}"))
        .json(&body)
        .send()
        .await?;

    if !resp.status().is_success() {
        anyhow::bail!("openrouter HTTP {}", resp.status());
    }

    let json: serde_json::Value = resp.json().await?;
    let content = json["choices"][0]["message"]["content"]
        .as_str()
        .unwrap_or("");
    let parsed: serde_json::Value = serde_json::from_str(&strip_fences(content))?;
    Ok(parse_labels(&parsed, entries.len()))
}

fn protocol_from_label(label: &str) -> Protocol {
    match label {
        "rest" => Protocol::Rest,
        "graphql" => Protocol::Graphql,
        "jsonrpc" => Protocol::JsonRpc,
        "form" => Protocol::Form,
        _ => Protocol::Noise,
    }
}

/// Accepts the id-keyed form, and a bare string array only when its length
/// matches exactly — a short bare array carries no way to tell which entry each
/// label belongs to, so it is discarded rather than guessed at.
fn parse_labels(parsed: &serde_json::Value, expected: usize) -> HashMap<usize, Protocol> {
    let mut out = HashMap::new();
    let Some(labels) = parsed["labels"].as_array() else {
        return out;
    };

    let keyed = labels.iter().filter_map(|v| {
        let id = v.get("id")?.as_u64()? as usize;
        let label = v.get("label")?.as_str()?;
        Some((id, protocol_from_label(label)))
    });
    for (id, protocol) in keyed {
        if id < expected {
            out.insert(id, protocol);
        }
    }
    if !out.is_empty() {
        return out;
    }

    if labels.len() == expected {
        for (i, v) in labels.iter().enumerate() {
            if let Some(label) = v.as_str() {
                out.insert(i, protocol_from_label(label));
            }
        }
    }
    out
}

fn truncate(s: &str, max_chars: usize) -> String {
    let mut out = String::new();
    for (i, ch) in s.chars().enumerate() {
        if i >= max_chars {
            out.push('…');
            break;
        }
        out.push(ch);
    }
    out
}

fn strip_fences(text: &str) -> String {
    let t = text.trim();
    if t.starts_with("```") {
        t.lines()
            .skip(1)
            .take_while(|l| !l.starts_with("```"))
            .collect::<Vec<_>>()
            .join("\n")
    } else {
        t.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn json(s: &str) -> serde_json::Value {
        serde_json::from_str(s).unwrap()
    }

    #[test]
    fn id_keyed_labels_survive_a_dropped_item() {
        // Model omitted id 1. Positional assignment would have shifted "graphql"
        // onto entry 1 and "form" onto entry 2, mislabeling everything after the gap.
        let parsed = json(
            r#"{"labels":[{"id":0,"label":"rest"},{"id":2,"label":"graphql"},{"id":3,"label":"form"}]}"#,
        );
        let got = parse_labels(&parsed, 4);
        assert_eq!(got.get(&0), Some(&Protocol::Rest));
        assert_eq!(
            got.get(&1),
            None,
            "gap must stay a gap, not inherit a label"
        );
        assert_eq!(got.get(&2), Some(&Protocol::Graphql));
        assert_eq!(got.get(&3), Some(&Protocol::Form));
    }

    #[test]
    fn bare_array_accepted_only_at_exact_length() {
        let full = json(r#"{"labels":["rest","noise","graphql"]}"#);
        assert_eq!(parse_labels(&full, 3).len(), 3);

        // Short bare array gives no way to know which entry was skipped.
        let short = json(r#"{"labels":["rest","graphql"]}"#);
        assert!(parse_labels(&short, 3).is_empty());
    }

    #[test]
    fn out_of_range_and_malformed_ids_are_dropped() {
        let parsed = json(r#"{"labels":[{"id":9,"label":"rest"},{"id":0,"label":"bogus"}]}"#);
        let got = parse_labels(&parsed, 2);
        assert_eq!(got.get(&9), None);
        assert_eq!(got.get(&0), Some(&Protocol::Noise));
    }

    #[test]
    fn missing_labels_key_yields_nothing() {
        assert!(parse_labels(&json(r#"{"other":[]}"#), 3).is_empty());
    }
}
