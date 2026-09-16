//! `spoor call --surface --op` — replay one HTTP op with a keychain-injected credential.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use serde::Deserialize;
use serde_json::Value;

use crate::auth::{AuthDocument, Carrier, Scheme, handle_for, load_secret};
use crate::log;

#[derive(Debug, Deserialize)]
struct SurfaceFile {
    origin: String,
    #[serde(default)]
    addressing: AddressingFile,
}

#[derive(Debug, Default, Deserialize)]
struct AddressingFile {
    #[serde(default)]
    sample_url: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ManifestFile {
    #[serde(default)]
    browser_pages: Vec<PageFile>,
}

#[derive(Debug, Deserialize)]
struct PageFile {
    url: String,
}

#[derive(Debug, Deserialize)]
struct OperationsIndex {
    operations: Vec<OpIndexEntry>,
}

#[derive(Debug, Deserialize)]
struct OpIndexEntry {
    id: String,
    file: String,
}

#[derive(Debug, Deserialize)]
struct OpFile {
    #[allow(dead_code)]
    id: String,
    #[serde(default)]
    protocol: String,
    addressing: OpAddressing,
    #[serde(default)]
    example_request: Option<Value>,
}

#[derive(Debug, Deserialize)]
struct OpAddressing {
    #[serde(default)]
    method: Option<String>,
    #[serde(default)]
    url: Option<String>,
}

pub fn resolve_pack_dir(pack: Option<&Path>) -> Result<PathBuf> {
    let dir = pack
        .map(Path::to_path_buf)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    if !dir.join("MANIFEST.yaml").is_file() {
        bail!(
            "no MANIFEST.yaml in {} — pass --pack <unpacked-pack-dir>",
            dir.display()
        );
    }
    Ok(dir)
}

pub fn load_auth_document(pack_dir: &Path, surface_id: &str) -> Result<AuthDocument> {
    let path = pack_dir.join("surfaces").join(surface_id).join("auth.yaml");
    let raw = std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    serde_yaml_ng::from_str(&raw).with_context(|| format!("parse {}", path.display()))
}

pub fn load_entry_url(pack_dir: &Path, surface_id: &str, origin: &str) -> Result<String> {
    let manifest_path = pack_dir.join("MANIFEST.yaml");
    if let Ok(raw) = std::fs::read_to_string(&manifest_path)
        && let Ok(manifest) = serde_yaml_ng::from_str::<ManifestFile>(&raw)
    {
        if let Some(page) = manifest
            .browser_pages
            .iter()
            .find(|p| p.url.starts_with(origin))
        {
            return Ok(page.url.clone());
        }
        if let Some(page) = manifest.browser_pages.first() {
            return Ok(page.url.clone());
        }
    }
    let surface_path = pack_dir
        .join("surfaces")
        .join(surface_id)
        .join("surface.yaml");
    let raw = std::fs::read_to_string(&surface_path)
        .with_context(|| format!("read {}", surface_path.display()))?;
    let surface: SurfaceFile = serde_yaml_ng::from_str(&raw)
        .with_context(|| format!("parse {}", surface_path.display()))?;
    if let Some(url) = surface.addressing.sample_url {
        return Ok(url);
    }
    Ok(surface.origin)
}

pub async fn run_call(
    surface_id: &str,
    op_id: &str,
    pack: Option<&Path>,
    allow_mutating: bool,
) -> Result<()> {
    let pack_dir = resolve_pack_dir(pack)?;
    let auth = load_auth_document(&pack_dir, surface_id)?;
    let op = load_op(&pack_dir, surface_id, op_id)?;

    if op.protocol.eq_ignore_ascii_case("websocket") {
        bail!("spoor call supports HTTP operations only (observed protocol is websocket)");
    }

    let method = op
        .addressing
        .method
        .clone()
        .unwrap_or_else(|| "GET".into())
        .to_uppercase();
    if !is_safe_method(&method) && !allow_mutating {
        bail!("refusing {method} without --allow-mutating (default is GET/HEAD/OPTIONS only)");
    }

    let Some(cred) = auth.credentials.first() else {
        bail!("pack for surface {surface_id} has no credential carrier to inject");
    };
    let handle = handle_for(surface_id, &cred.carrier);
    let secret = load_secret(&handle)?;

    let mut url = op
        .addressing
        .url
        .clone()
        .ok_or_else(|| anyhow!("operation {op_id} has no observed URL"))?;
    let mut headers: HashMap<String, String> = HashMap::new();
    let mut body = op.example_request.clone();
    inject_credential(
        &mut headers,
        &mut url,
        &mut body,
        &cred.carrier,
        cred.scheme,
        &secret,
    )?;

    let client = reqwest::Client::new();
    let http_method = reqwest::Method::from_bytes(method.as_bytes())
        .with_context(|| format!("HTTP method {method}"))?;
    let mut request = client.request(http_method, &url);
    for (k, v) in &headers {
        request = request.header(k.as_str(), v.as_str());
    }
    if method != "GET"
        && method != "HEAD"
        && let Some(b) = body
    {
        request = request.json(&b);
    }

    log::info(format!("calling {method} {url}"));
    let response = request.send().await.context("send HTTP request")?;
    log::info(format!("status {}", response.status()));
    let text = response.text().await.unwrap_or_default();
    if !text.is_empty() {
        println!("{text}");
    }
    Ok(())
}

fn load_op(pack_dir: &Path, surface_id: &str, op_id: &str) -> Result<OpFile> {
    let index_path = pack_dir
        .join("surfaces")
        .join(surface_id)
        .join("operations.yaml");
    let raw = std::fs::read_to_string(&index_path)
        .with_context(|| format!("read {}", index_path.display()))?;
    let index: OperationsIndex =
        serde_yaml_ng::from_str(&raw).with_context(|| format!("parse {}", index_path.display()))?;
    let entry = index
        .operations
        .iter()
        .find(|e| e.id == op_id)
        .ok_or_else(|| anyhow!("operation {op_id} not found in {}", index_path.display()))?;
    let op_path = pack_dir.join("surfaces").join(surface_id).join(&entry.file);
    let raw =
        std::fs::read_to_string(&op_path).with_context(|| format!("read {}", op_path.display()))?;
    serde_yaml_ng::from_str(&raw).with_context(|| format!("parse {}", op_path.display()))
}

fn is_safe_method(method: &str) -> bool {
    matches!(method, "GET" | "HEAD" | "OPTIONS")
}

/// Inject `secret` at the observed carrier. Used by `call` and unit-tested without the keychain.
pub fn inject_credential(
    headers: &mut HashMap<String, String>,
    url: &mut String,
    body: &mut Option<Value>,
    carrier: &Carrier,
    scheme: Scheme,
    secret: &str,
) -> Result<()> {
    match carrier {
        Carrier::Header { name } => {
            let value = match scheme {
                Scheme::Bearer if !secret.starts_with("Bearer ") => format!("Bearer {secret}"),
                Scheme::Basic if !secret.starts_with("Basic ") => format!("Basic {secret}"),
                _ => secret.to_string(),
            };
            headers.insert(name.clone(), value);
        }
        Carrier::Cookie { name } => {
            let cookie = format!("{name}={secret}");
            headers
                .entry("cookie".into())
                .and_modify(|existing| {
                    if !existing.is_empty() {
                        existing.push_str("; ");
                    }
                    existing.push_str(&cookie);
                })
                .or_insert(cookie);
        }
        Carrier::Query { name } => {
            let mut parsed = url::Url::parse(url).with_context(|| format!("parse URL {url}"))?;
            let mut pairs: Vec<(String, String)> = parsed
                .query_pairs()
                .map(|(k, v)| (k.into_owned(), v.into_owned()))
                .collect();
            if let Some(slot) = pairs.iter_mut().find(|(k, _)| k == name) {
                slot.1 = secret.to_string();
            } else {
                pairs.push((name.clone(), secret.to_string()));
            }
            parsed.query_pairs_mut().clear();
            parsed.query_pairs_mut().extend_pairs(&pairs);
            *url = parsed.to_string();
        }
        Carrier::Body { pointer } => {
            let mut json = body.take().unwrap_or(Value::Object(serde_json::Map::new()));
            set_json_pointer(&mut json, pointer, Value::String(secret.to_string()))?;
            *body = Some(json);
        }
    }
    Ok(())
}

fn set_json_pointer(root: &mut Value, pointer: &str, value: Value) -> Result<()> {
    let pointer = pointer.trim_start_matches('/');
    if pointer.is_empty() {
        *root = value;
        return Ok(());
    }
    let mut cur = root;
    let parts: Vec<&str> = pointer.split('/').collect();
    for (i, part) in parts.iter().enumerate() {
        let key = part.replace("~1", "/").replace("~0", "~");
        let last = i + 1 == parts.len();
        match cur {
            Value::Object(map) => {
                if last {
                    map.insert(key, value);
                    return Ok(());
                }
                cur = map
                    .entry(key)
                    .or_insert(Value::Object(serde_json::Map::new()));
            }
            _ => bail!("JSON pointer {pointer} does not address an object"),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn injects_bearer_header() {
        let mut headers = HashMap::new();
        let mut url = "https://api.example.test/v1/me".to_string();
        let mut body = None;
        inject_credential(
            &mut headers,
            &mut url,
            &mut body,
            &Carrier::header("authorization"),
            Scheme::Bearer,
            "tok_live_abc",
        )
        .unwrap();
        assert_eq!(headers.get("authorization").unwrap(), "Bearer tok_live_abc");
    }

    #[test]
    fn injects_cookie() {
        let mut headers = HashMap::new();
        let mut url = "https://app.example.test/api/me".to_string();
        let mut body = None;
        inject_credential(
            &mut headers,
            &mut url,
            &mut body,
            &Carrier::cookie("sid"),
            Scheme::Opaque,
            "s3cret",
        )
        .unwrap();
        assert_eq!(headers.get("cookie").unwrap(), "sid=s3cret");
    }

    #[test]
    fn injects_query() {
        let mut headers = HashMap::new();
        let mut url = "https://api.example.test/v1?page=1".to_string();
        let mut body = None;
        inject_credential(
            &mut headers,
            &mut url,
            &mut body,
            &Carrier::query("access_token"),
            Scheme::Opaque,
            "tok12345678",
        )
        .unwrap();
        assert!(url.contains("access_token=tok12345678"), "{url}");
        assert!(url.contains("page=1"), "{url}");
    }
}
