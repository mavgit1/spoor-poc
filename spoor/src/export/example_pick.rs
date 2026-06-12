use url::Url;

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
    if let Some(body) = &entry.entry.flow.request_body {
        score += body.len().min(50_000);
    }
    if let Some(body) = &entry.entry.flow.response_body {
        score += 1_000;
        score += body.len().min(50_000);
    }
    score
}
