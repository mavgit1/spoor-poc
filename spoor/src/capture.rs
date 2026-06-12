use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::Result;
use base64::Engine;
use base64::prelude::BASE64_STANDARD;
use chromiumoxide::Page;
use chromiumoxide::cdp::browser_protocol::network::{
    EventLoadingFinished, EventRequestWillBeSent, EventResponseReceived, GetResponseBodyParams,
    Headers,
};
use futures::StreamExt;
use tokio::sync::RwLock;

use crate::log;
use crate::types::CapturedFlow;

const DEFAULT_MAX_FLOWS: usize = 10_000;

pub fn max_flows_limit() -> usize {
    std::env::var("SPOOR_MAX_FLOWS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_MAX_FLOWS)
}

fn headers_to_map(headers: &Headers) -> HashMap<String, String> {
    headers
        .inner()
        .as_object()
        .map(|obj| {
            obj.iter()
                .map(|(k, v)| (k.clone(), v.as_str().unwrap_or("").to_string()))
                .collect()
        })
        .unwrap_or_default()
}

fn decode_body(body: &str, base64_encoded: bool) -> String {
    if base64_encoded {
        BASE64_STANDARD
            .decode(body)
            .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
            .unwrap_or_else(|_| body.to_string())
    } else {
        body.to_string()
    }
}

fn request_body_from_entries(
    entries: Option<&Vec<chromiumoxide::cdp::browser_protocol::network::PostDataEntry>>,
) -> Option<String> {
    let entry = entries?.first()?;
    let bytes = entry.bytes.as_ref()?;
    let raw = bytes.as_ref();
    if BASE64_STANDARD.decode(raw).is_ok() {
        Some(decode_body(raw, true))
    } else {
        Some(raw.to_string())
    }
}

fn should_store_response_body(flow: &CapturedFlow) -> bool {
    if let Some(rt) = flow.resource_type.as_deref() {
        let lower = rt.to_ascii_lowercase();
        if lower.contains("image")
            || lower.contains("font")
            || lower.contains("media")
            || lower.contains("stylesheet")
            || lower.contains("script")
        {
            return false;
        }
    }
    if let Some(headers) = &flow.response_headers {
        let ct = headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("content-type"))
            .map(|(_, v)| v.to_ascii_lowercase());
        if let Some(ct) = ct {
            if ct.starts_with("image/")
                || ct.starts_with("font/")
                || ct.starts_with("video/")
                || ct.starts_with("audio/")
                || ct == "application/octet-stream"
            {
                return false;
            }
        }
    }
    true
}

pub async fn capture(
    page: Arc<Page>,
    flows: Arc<RwLock<Vec<CapturedFlow>>>,
    flows_capped: Arc<AtomicBool>,
) -> Result<()> {
    let max_flows = max_flows_limit();
    let mut pending: HashMap<String, CapturedFlow> = HashMap::new();

    let mut will_be_sent = page.event_listener::<EventRequestWillBeSent>().await?;
    let mut response_received = page.event_listener::<EventResponseReceived>().await?;
    let mut loading_finished = page.event_listener::<EventLoadingFinished>().await?;

    let mut will_be_sent_open = true;
    let mut response_received_open = true;
    let mut loading_finished_open = true;

    loop {
        if !will_be_sent_open && !response_received_open && !loading_finished_open {
            break;
        }

        tokio::select! {
            ev = will_be_sent.next(), if will_be_sent_open => {
                match ev {
                    None => {
                        log::debug("capture: requestWillBeSent listener closed");
                        will_be_sent_open = false;
                    }
                    Some(ev) => {
                        let req = &ev.request;
                        let id = ev.request_id.inner().clone();
                        pending.insert(id.clone(), CapturedFlow {
                            id,
                            url: req.url.clone(),
                            method: req.method.clone(),
                            request_headers: headers_to_map(&req.headers),
                            request_body: request_body_from_entries(req.post_data_entries.as_ref()),
                            status: None,
                            response_headers: None,
                            response_body: None,
                            resource_type: ev.r#type.as_ref().map(|t| format!("{t:?}")),
                        });
                    }
                }
            }
            ev = response_received.next(), if response_received_open => {
                match ev {
                    None => {
                        log::debug("capture: responseReceived listener closed");
                        response_received_open = false;
                    }
                    Some(ev) => {
                        if let Some(flow) = pending.get_mut(ev.request_id.inner()) {
                            let resp = &ev.response;
                            flow.status = Some(resp.status as u16);
                            flow.response_headers = Some(headers_to_map(&resp.headers));
                            let rt = format!("{:?}", ev.r#type);
                            if rt != "Other" {
                                flow.resource_type = Some(rt);
                            }
                        }
                    }
                }
            }
            ev = loading_finished.next(), if loading_finished_open => {
                match ev {
                    None => {
                        log::debug("capture: loadingFinished listener closed");
                        loading_finished_open = false;
                    }
                    Some(ev) => {
                        let id = ev.request_id.inner().clone();
                        if let Some(mut flow) = pending.remove(&id) {
                            if should_store_response_body(&flow) {
                                if let Ok(body) = page
                                    .execute(GetResponseBodyParams::new(ev.request_id.clone()))
                                    .await
                                {
                                    flow.response_body =
                                        Some(decode_body(&body.body, body.base64_encoded));
                                }
                            }
                            log::debug(&format!(
                                "capture: {} {} → {:?}",
                                flow.method, flow.url, flow.status
                            ));
                            let mut guard = flows.write().await;
                            if guard.len() >= max_flows {
                                flows_capped.store(true, Ordering::SeqCst);
                            } else {
                                guard.push(flow);
                            }
                        }
                    }
                }
            }
        }
    }

    let count = flows.read().await.len();
    log::info(&format!("capture loop ended with {count} flows stored"));
    Ok(())
}
