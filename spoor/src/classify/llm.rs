use crate::classify::Protocol;
use crate::ir::TrafficEntry;
use crate::log;

const OPENROUTER_URL: &str = "https://openrouter.ai/api/v1/chat/completions";

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
            log::warn(&format!("LLM classify failed: {e:#}"));
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

    if entries.len() > 20 {
        log::warn(&format!(
            "LLM classify: {} ambiguous entries, only first 20 sent to model",
            entries.len()
        ));
    }

    let model = std::env::var("OPENROUTER_MODEL")
        .unwrap_or_else(|_| "qwen/qwen3.6-flash".to_string());

    let snippets: Vec<serde_json::Value> = entries
        .iter()
        .take(20)
        .map(|e| {
            serde_json::json!({
                "method": e.flow.method,
                "url": e.flow.url,
                "request_body": e.flow.request_body.as_ref().map(|b| truncate(b, 500)),
                "response_status": e.flow.status,
            })
        })
        .collect();

    let prompt = format!(
        "Classify each HTTP capture as rest, graphql, or noise. \
         Respond with JSON only: {{\"labels\":[\"rest\"|\"graphql\"|\"noise\", ...]}} \
         in the same order as the input array.\n\n{}",
        serde_json::to_string_pretty(&snippets)?
    );

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(60))
        .build()?;

    let body = serde_json::json!({
        "model": model,
        "messages": [{"role": "user", "content": prompt}],
        "provider": {"sort": "price"}
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

    let mut out = Vec::with_capacity(entries.len());
    for (i, _) in entries.iter().enumerate() {
        let label = labels
            .get(i)
            .and_then(|v| v.as_str())
            .unwrap_or("noise");
        out.push(match label {
            "rest" => Protocol::Rest,
            "graphql" => Protocol::Graphql,
            _ => Protocol::Noise,
        });
    }
    while out.len() < entries.len() {
        out.push(Protocol::Noise);
    }
    Ok(out)
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
