use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use anyhow::Result;
use base64::Engine;
use base64::prelude::BASE64_STANDARD;
use chromiumoxide::Page;
use chromiumoxide::cdp::browser_protocol::network::{
    EventWebSocketCreated, EventWebSocketFrameReceived, EventWebSocketFrameSent, WebSocketFrame,
};
use futures::StreamExt;
use tokio::sync::RwLock;

use crate::capture::model::{
    Body, CaptureRecord, Direction, MAX_BINARY_BYTES, OmitReason, Transport,
};
use crate::log;

fn frame_body(frame: &WebSocketFrame) -> Body {
    // opcode 1 = text; otherwise binary (payload is base64).
    if (frame.opcode - 1.0).abs() < f64::EPSILON {
        Body::Text(frame.payload_data.clone())
    } else {
        match BASE64_STANDARD.decode(&frame.payload_data) {
            Ok(data) => {
                if data.len() > MAX_BINARY_BYTES {
                    Body::Omitted {
                        reason: OmitReason::TooLarge,
                    }
                } else {
                    Body::Bytes {
                        data,
                        content_type: None,
                    }
                }
            }
            Err(_) => Body::Text(frame.payload_data.clone()),
        }
    }
}

fn opcode_label(opcode: f64) -> String {
    if (opcode - 1.0).abs() < f64::EPSILON {
        "text".into()
    } else if (opcode - 2.0).abs() < f64::EPSILON {
        "binary".into()
    } else {
        format!("opcode_{opcode}")
    }
}

pub async fn run(
    page: Arc<Page>,
    flows: Arc<RwLock<Vec<CaptureRecord>>>,
    flows_capped: Arc<AtomicBool>,
    sequence: Arc<AtomicU64>,
    max_flows: usize,
) -> Result<()> {
    let mut urls: HashMap<String, String> = HashMap::new();
    let mut frame_seq: HashMap<String, u64> = HashMap::new();

    let mut created = page.event_listener::<EventWebSocketCreated>().await?;
    let mut sent = page.event_listener::<EventWebSocketFrameSent>().await?;
    let mut received = page.event_listener::<EventWebSocketFrameReceived>().await?;

    let mut created_open = true;
    let mut sent_open = true;
    let mut received_open = true;

    loop {
        if !created_open && !sent_open && !received_open {
            break;
        }

        tokio::select! {
            ev = created.next(), if created_open => {
                match ev {
                    None => {
                        log::debug("capture/ws: WebSocketCreated listener closed");
                        created_open = false;
                    }
                    Some(ev) => {
                        let id = ev.request_id.inner().clone();
                        urls.insert(id, ev.url.clone());
                    }
                }
            }
            ev = sent.next(), if sent_open => {
                match ev {
                    None => {
                        log::debug("capture/ws: FrameSent listener closed");
                        sent_open = false;
                    }
                    Some(ev) => {
                        push_frame(
                            &flows,
                            &flows_capped,
                            &sequence,
                            max_flows,
                            &urls,
                            &mut frame_seq,
                            ev.request_id.inner(),
                            &ev.response,
                            Direction::Outbound,
                            (*ev.timestamp.inner() * 1000.0) as u64,
                        ).await;
                    }
                }
            }
            ev = received.next(), if received_open => {
                match ev {
                    None => {
                        log::debug("capture/ws: FrameReceived listener closed");
                        received_open = false;
                    }
                    Some(ev) => {
                        push_frame(
                            &flows,
                            &flows_capped,
                            &sequence,
                            max_flows,
                            &urls,
                            &mut frame_seq,
                            ev.request_id.inner(),
                            &ev.response,
                            Direction::Inbound,
                            (*ev.timestamp.inner() * 1000.0) as u64,
                        ).await;
                    }
                }
            }
        }
    }

    Ok(())
}

async fn push_frame(
    flows: &Arc<RwLock<Vec<CaptureRecord>>>,
    flows_capped: &Arc<AtomicBool>,
    sequence: &Arc<AtomicU64>,
    max_flows: usize,
    urls: &HashMap<String, String>,
    frame_seq: &mut HashMap<String, u64>,
    request_id: &str,
    frame: &WebSocketFrame,
    direction: Direction,
    timestamp_ms: u64,
) {
    let url = urls
        .get(request_id)
        .cloned()
        .unwrap_or_else(|| format!("ws://unknown/{request_id}"));
    let n = frame_seq.entry(request_id.to_string()).or_insert(0);
    *n += 1;
    let frame_n = *n;
    let seq = sequence.fetch_add(1, Ordering::SeqCst);
    let body = frame_body(frame);
    let opcode = opcode_label(frame.opcode);

    let record = CaptureRecord {
        id: format!("ws:{request_id}:{frame_n}"),
        transport: Transport::WebSocket,
        url,
        method: None,
        request_headers: HashMap::new(),
        request_body: if direction == Direction::Outbound {
            Some(body.clone())
        } else {
            None
        },
        status: None,
        response_headers: None,
        response_body: if direction == Direction::Inbound {
            Some(body)
        } else {
            None
        },
        resource_type: Some("WebSocket".into()),
        sequence: seq,
        timestamp_ms: Some(timestamp_ms),
        ws_request_id: Some(request_id.to_string()),
        ws_opcode: Some(opcode),
        direction: Some(direction),
    };

    log::debug(&format!(
        "capture/ws: {:?} {} ({})",
        direction,
        record.url,
        record.ws_opcode.as_deref().unwrap_or("?")
    ));

    let mut guard = flows.write().await;
    if guard.len() >= max_flows {
        flows_capped.store(true, Ordering::SeqCst);
    } else {
        guard.push(record);
    }
}
