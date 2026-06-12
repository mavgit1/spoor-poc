use std::collections::HashMap;
use std::sync::Arc;

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

pub async fn capture(page: Arc<Page>, flows: Arc<RwLock<Vec<CapturedFlow>>>) -> Result<()> {
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
                            // CDP sometimes reports Other; keep Fetch/XHR from requestWillBeSent.
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
                            if let Ok(body) = page
                                .execute(GetResponseBodyParams::new(ev.request_id.clone()))
                                .await
                            {
                                flow.response_body = Some(decode_body(&body.body, body.base64_encoded));
                            }
                            log::debug(&format!(
                                "capture: {} {} → {:?}",
                                flow.method, flow.url, flow.status
                            ));
                            flows.write().await.push(flow);
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
