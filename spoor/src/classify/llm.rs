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
                "LLM classify batch {}: got {} labels for {} entries — padding with noise",
                batch_idx + 1,
                labels.len(),
                chunk.len()
            ));
        }
        for i in 0..chunk.len() {
            let orig_idx = selected[label_offset + i].0;
            out[orig_idx] = labels.get(i).copied().unwrap_or(Protocol::Noise);
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
) -> anyhow::Result<Vec<Protocol>> {
    let snippets: Vec<serde_json::Value> = entries
        .iter()
        .map(|e| {
            serde_json::json!({
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
         Respond with JSON only: {{\"labels\":[\"rest\"|\"graphql\"|\"jsonrpc\"|\"form\"|\"noise\", ...]}}\n\
         Same order and length as the input array.\n\n{}",
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
    let labels = parsed["labels"].as_array().cloned().unwrap_or_default();

    Ok(labels
        .iter()
        .map(|v| match v.as_str().unwrap_or("noise") {
            "rest" => Protocol::Rest,
            "graphql" => Protocol::Graphql,
            "jsonrpc" => Protocol::JsonRpc,
            "form" => Protocol::Form,
            _ => Protocol::Noise,
        })
        .collect())
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
