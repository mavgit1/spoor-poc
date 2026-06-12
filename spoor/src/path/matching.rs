use std::collections::{HashMap, HashSet};

use regex::Regex;

use super::segments::{is_base58, is_hex_string, is_numeric_string, is_upper_case_slug, is_uuid};
use crate::path::MIN_VARIABILITY_CARDINALITY;

/// Check if a path segment looks like a parameter value (numeric, UUID, etc.).
pub fn is_param_segment(segment: &str, custom_regex: Option<&Regex>) -> bool {
    if segment.is_empty() || is_version_prefix(segment) {
        return false;
    }
    if is_numeric_string(segment) || is_uuid(segment) {
        return true;
    }
    if is_upper_case_slug(segment) || is_hex_string(segment) || is_base58(segment) {
        return true;
    }
    custom_regex.is_some_and(|re| re.is_match(segment))
}

/// Suggest `{id}` path templates from observed URL paths on the same origin.
pub fn suggest_param_templates(paths: &[String], custom_regex: Option<&Regex>) -> Vec<String> {
    let mut position_values: HashMap<(usize, usize), HashSet<&str>> = HashMap::new();
    for path in paths {
        let segs: Vec<&str> = path.split('/').collect();
        let n = segs.len();
        for (i, seg) in segs.iter().enumerate() {
            if !seg.is_empty() {
                position_values.entry((n, i)).or_default().insert(seg);
            }
        }
    }

    let variability_params: HashSet<(usize, usize)> = position_values
        .into_iter()
        .filter(|(_, vals)| {
            vals.len() >= MIN_VARIABILITY_CARDINALITY
                && !vals.iter().any(|v| is_version_prefix(v))
                && !vals.iter().all(|v| looks_like_api_vocabulary(v))
                && vals
                    .iter()
                    .all(|v| is_param_segment(v, custom_regex) || !looks_like_api_vocabulary(v))
        })
        .map(|(k, _)| k)
        .collect();

    let mut templates: HashSet<String> = HashSet::new();

    for path in paths {
        let segments: Vec<&str> = path.split('/').collect();
        let n = segments.len();
        let mut param_count = 0u32;
        let mut template_segments: Vec<String> = Vec::new();

        for (i, segment) in segments.iter().enumerate() {
            if segment.is_empty() {
                template_segments.push(String::new());
                continue;
            }
            if is_param_segment(segment, custom_regex) || variability_params.contains(&(n, i)) {
                param_count += 1;
                template_segments.push(format!("{{__P{param_count}}}"));
            } else {
                template_segments.push((*segment).to_string());
            }
        }

        let mut template = template_segments.join("/");
        if param_count == 1 {
            template = template.replace("{__P1}", "{id}");
        } else {
            for i in 1..=param_count {
                template = template.replace(&format!("{{__P{i}}}"), &format!("{{id{i}}}"));
            }
        }
        templates.insert(template);
    }

    let mut result: Vec<String> = templates.into_iter().collect();
    result.sort();
    result
}

fn is_version_prefix(s: &str) -> bool {
    let mut chars = s.chars();
    matches!(chars.next(), Some('v')) && chars.all(|c| c.is_ascii_digit()) && s.len() >= 2
}

fn looks_like_api_vocabulary(segment: &str) -> bool {
    !segment.is_empty()
        && segment.len() >= 2
        && !segment.chars().any(|c| c.is_ascii_digit())
        && segment
            .chars()
            .all(|c| c.is_ascii_lowercase() || c == '-' || c == '_')
}

#[cfg(test)]
#[allow(clippy::indexing_slicing)]
mod tests {
    use super::*;

    #[test]
    fn numeric_is_param() {
        assert!(is_param_segment("123", None));
        assert!(!is_param_segment("users", None));
    }

    #[test]
    fn suggest_replaces_numeric_segments() {
        let paths = vec!["/users/1".to_string(), "/users/2".to_string()];
        assert_eq!(suggest_param_templates(&paths, None), vec!["/users/{id}"]);
    }

    #[test]
    fn variability_api_vocabulary_not_merged() {
        let paths = vec![
            "/api/v1/public/meta/typeahead".to_string(),
            "/api/v1/public/search/job".to_string(),
            "/api/v1/user/bookmark/job".to_string(),
        ];
        let templates = suggest_param_templates(&paths, None);
        assert!(templates.contains(&"/api/v1/public/meta/typeahead".to_string()));
        assert!(!templates.iter().any(|t| t.contains("{id1}")));
    }

    #[test]
    fn variability_two_values_not_parameterized() {
        let paths = vec![
            "/api/v1/status/active".to_string(),
            "/api/v1/status/inactive".to_string(),
        ];
        assert_eq!(suggest_param_templates(&paths, None).len(), 2);
    }
}
