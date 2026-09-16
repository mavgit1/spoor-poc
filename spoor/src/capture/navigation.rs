use std::sync::Arc;

use anyhow::Result;
use chromiumoxide::Page;
use chromiumoxide::cdp::browser_protocol::page::EventFrameNavigated;
use futures::StreamExt;
use tokio::sync::RwLock;

use crate::log;
use crate::types::BrowsingPage;

/// Track top-level (main-frame) navigations for session context / relations.
pub async fn run(page: Arc<Page>, page_urls: Arc<RwLock<Vec<BrowsingPage>>>) -> Result<()> {
    let mut navigated = page.event_listener::<EventFrameNavigated>().await?;
    while let Some(ev) = navigated.next().await {
        let frame = &ev.frame;
        if frame.parent_id.is_some() {
            continue;
        }
        let url = frame.url.trim();
        if url.is_empty() || url.starts_with("about:") || url.starts_with("chrome:") {
            continue;
        }
        let domain = if frame.domain_and_registry.is_empty() {
            url::Url::parse(url)
                .ok()
                .and_then(|u| u.host_str().map(str::to_string))
                .unwrap_or_default()
        } else {
            frame.domain_and_registry.clone()
        };
        let entry = BrowsingPage {
            url: url.to_string(),
            domain,
        };
        let mut guard = page_urls.write().await;
        if guard.last().map(|p| p.url.as_str()) != Some(entry.url.as_str()) {
            log::debug(format!("navigation: {}", entry.url));
            guard.push(entry);
        }
    }
    Ok(())
}
