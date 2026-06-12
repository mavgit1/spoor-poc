use std::collections::{BTreeMap, BTreeSet};

use regex::Regex;

use crate::classify::api_origins;
use crate::classify::{ClassifiedEntry, Protocol};
use crate::discover::{confidence_str, protocol_str};
use crate::path;
use crate::types::Candidate;

pub fn discover(classified: &[ClassifiedEntry]) -> Vec<Candidate> {
    let custom_regex = Regex::new(r"^[0-9a-fA-F-]{8,}$").ok();

    let api_origins = api_origins::from_classified(classified);

    let mut paths_by_origin: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for item in classified.iter().filter(|c| c.protocol == Protocol::Rest) {
        if !api_origins.contains(&item.entry.origin) {
            continue;
        }
        paths_by_origin
            .entry(item.entry.origin.clone())
            .or_default()
            .insert(item.entry.path.clone());
    }

    let mut candidates = Vec::new();
    for (origin, paths) in paths_by_origin {
        let paths_vec: Vec<String> = paths
            .into_iter()
            .filter(|p| !is_unclusterable_path(p, custom_regex.as_ref()))
            .collect();
        if paths_vec.is_empty() {
            continue;
        }
        let suggested = path::suggest_param_templates(&paths_vec, custom_regex.as_ref());

        for template in suggested {
            if !is_actionable_template(&template) {
                continue;
            }
            let methods: BTreeSet<String> = classified
                .iter()
                .filter(|c| c.protocol == Protocol::Rest && c.entry.origin == origin)
                .filter(|c| path_matches_template(&c.entry.path, &template))
                .map(|c| c.entry.flow.method.to_uppercase())
                .collect();

            if methods.is_empty() {
                continue;
            }

            let example_entry = classified
                .iter()
                .find(|c| {
                    c.protocol == Protocol::Rest
                        && c.entry.origin == origin
                        && path_matches_template(&c.entry.path, &template)
                })
                .cloned();

            let Some(example_entry) = example_entry else {
                continue;
            };

            let method_list: Vec<String> = methods.into_iter().collect();
            let primary_method = method_list[0].clone();
            let id = format!("rest|{origin}|{primary_method}|{template}");
            let conf = example_entry.confidence;
            let request_count = classified
                .iter()
                .filter(|c| c.protocol == Protocol::Rest && c.entry.origin == origin)
                .filter(|c| path_matches_template(&c.entry.path, &template))
                .count();

            candidates.push(Candidate {
                id,
                label: format!("{primary_method} {template}"),
                protocol: protocol_str(Protocol::Rest).to_string(),
                guessed_pattern: template,
                example: format!(
                    "{} {}",
                    example_entry.entry.flow.method.to_uppercase(),
                    example_entry.entry.flow.url
                ),
                host: origin
                    .trim_start_matches("https://")
                    .trim_start_matches("http://")
                    .to_string(),
                methods: method_list,
                confidence: confidence_str(conf).to_string(),
                origin: origin.clone(),
                request_count,
            });
        }
    }

    candidates
}

/// Lone dynamic segments (e.g. a UUID at `/`) produce useless `/{id}` templates.
fn is_unclusterable_path(path: &str, custom_regex: Option<&Regex>) -> bool {
    let segs: Vec<&str> = path
        .trim_matches('/')
        .split('/')
        .filter(|s| !s.is_empty())
        .collect();
    segs.len() == 1 && path::is_param_segment(segs[0], custom_regex)
}

fn is_actionable_template(template: &str) -> bool {
    let segs: Vec<&str> = template
        .trim_matches('/')
        .split('/')
        .filter(|s| !s.is_empty())
        .collect();
    if segs.is_empty() {
        return false;
    }
    if segs.len() == 1 {
        let s = segs[0];
        return !(s.starts_with('{') && s.ends_with('}'));
    }
    true
}

fn path_matches_template(path: &str, template: &str) -> bool {
    if path == template {
        return true;
    }
    let path_segs: Vec<&str> = path.trim_matches('/').split('/').collect();
    let tmpl_segs: Vec<&str> = template.trim_matches('/').split('/').collect();
    if path_segs.len() != tmpl_segs.len() {
        return false;
    }
    path_segs
        .iter()
        .zip(tmpl_segs.iter())
        .all(|(p, t)| {
            if t.starts_with('{') && t.ends_with('}') {
                !p.is_empty()
            } else {
                p == t
            }
        })
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use crate::classify::{ClassifiedEntry, Confidence, Protocol};
    use crate::ir::TrafficEntry;
    use crate::types::CapturedFlow;

    fn microservice_entry(path: &str, method: &str, body: &str) -> ClassifiedEntry {
        const ORIGIN: &str = "https://portal.example.test";
        ClassifiedEntry {
            entry: TrafficEntry {
                flow: CapturedFlow {
                    id: "1".into(),
                    url: format!("{ORIGIN}{path}"),
                    method: method.into(),
                    request_headers: HashMap::new(),
                    request_body: None,
                    status: Some(200),
                    response_headers: Some(HashMap::from([(
                        "content-type".into(),
                        "application/json".into(),
                    )])),
                    response_body: Some(body.into()),
                    resource_type: Some("None".into()),
                },
                origin: ORIGIN.into(),
                path: path.into(),
            },
            protocol: Protocol::Rest,
            confidence: Confidence::Llm,
            operation_name: None,
        }
    }

    #[test]
    fn discovers_microservice_paths() {
        let uuid = "4fb18b9c-90a5-4972-9b4f-23a1e68b440b";
        let classified = vec![
            microservice_entry(
                "/jobadservice/api/jobAdvertisements/_search",
                "POST",
                r#"{"total":0}"#,
            ),
            microservice_entry(
                &format!("/jobadservice/api/jobAdvertisements/{uuid}"),
                "GET",
                r#"{"id":"x"}"#,
            ),
            microservice_entry(
                "/referenceservice/api/_search/occupations/label",
                "GET",
                r#"[]"#,
            ),
        ];
        let candidates = super::discover(&classified);
        assert!(
            !candidates.is_empty(),
            "expected microservice REST candidates, got none"
        );
        assert!(candidates.iter().any(|c| c.guessed_pattern.contains("_search")));
    }
}
