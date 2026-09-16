use url::Url;

use crate::capture::Body;
use crate::classify::ClassifiedEntry;

/// Prefer captures with fuller bodies and query strings over first-seen.
pub fn richest_entry<'a>(
    entries: impl Iterator<Item = &'a ClassifiedEntry>,
) -> Option<&'a ClassifiedEntry> {
    entries.max_by_key(|e| entry_richness(e))
}

pub fn entry_richness(entry: &ClassifiedEntry) -> usize {
    let mut score = 0usize;
    if let Ok(url) = Url::parse(&entry.entry.flow.url) {
        score += url.query().map(str::len).unwrap_or(0);
    }
    score += body_len(entry.entry.flow.request_body.as_ref()).min(50_000);
    if entry.entry.flow.response_body.is_some() {
        score += 1_000;
        score += body_len(entry.entry.flow.response_body.as_ref()).min(50_000);
    }
    score
}

fn body_len(body: Option<&Body>) -> usize {
    match body {
        Some(Body::Text(s)) => s.len(),
        Some(Body::Bytes { data, .. }) => data.len(),
        Some(Body::Omitted { .. }) | None => 0,
    }
}
