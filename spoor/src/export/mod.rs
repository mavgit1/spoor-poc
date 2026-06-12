pub mod auth;
pub mod brief;
pub mod facets;
pub mod graphql;
pub mod query_params;
pub mod trim;

use std::collections::{BTreeMap, HashSet};
use std::io::Write;

use zip::write::SimpleFileOptions;
use zip::ZipWriter;

use crate::classify::ClassifiedEntry;
use crate::types::{Candidate, ExportBundle, GenerateRequest};

pub struct GenerateResult {
    pub bundle: ExportBundle,
    pub warnings: Vec<String>,
}

pub fn generate_bundle(
    classified: &[ClassifiedEntry],
    candidates: &[Candidate],
    req: &GenerateRequest,
) -> anyhow::Result<GenerateResult> {
    let redact = req.redact;
    let mut zip_files: Vec<(String, String)> = Vec::new();

    let mut rest_by_origin: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut gql_by_origin: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut origins_selected: HashSet<String> = HashSet::new();

    for sel in &req.selected {
        let Some(cand) = candidates.iter().find(|c| c.id == sel.id) else {
            continue;
        };
        if let Some(filter) = &req.origin {
            if &cand.origin != filter {
                continue;
            }
        }
        let pattern = sel
            .pattern
            .clone()
            .filter(|p| !p.is_empty())
            .unwrap_or_else(|| cand.guessed_pattern.clone());

        origins_selected.insert(cand.origin.clone());

        if sel.id.starts_with("rest|") {
            rest_by_origin
                .entry(cand.origin.clone())
                .or_default()
                .push(pattern);
        } else if sel.id.starts_with("graphql|") {
            gql_by_origin
                .entry(cand.origin.clone())
                .or_default()
                .push(pattern);
        }
    }

    for (origin, patterns) in &rest_by_origin {
        let brief = brief::generate_brief_yaml(
            classified,
            origin,
            "rest",
            patterns,
            candidates,
            redact,
        )?;
        zip_files.push((
            format!("integration-brief-{}.yaml", host_slug(origin)),
            brief,
        ));
    }

    let multi_file = rest_by_origin.len() + gql_by_origin.len() > 1;

    for (origin, ops) in &gql_by_origin {
        let yaml = graphql::generate_operations_yaml(classified, origin, ops)?;
        zip_files.push((
            if multi_file {
                format!("graphql-ops-{}.yaml", host_slug(origin))
            } else {
                "graphql-ops.yaml".to_string()
            },
            yaml,
        ));
        let brief =
            brief::generate_brief_yaml(classified, origin, "graphql", ops, candidates, redact)?;
        zip_files.push((
            format!("integration-brief-{}.yaml", host_slug(origin)),
            brief,
        ));
    }

    if zip_files.is_empty() {
        anyhow::bail!("no export artifacts produced for selection");
    }

    let warnings = auth::session_auth_warnings(classified, &origins_selected);
    let zip_bytes = build_zip(&zip_files)?;

    Ok(GenerateResult {
        bundle: ExportBundle { zip_bytes },
        warnings,
    })
}

fn host_slug(origin: &str) -> String {
    origin
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .replace('.', "-")
}

fn build_zip(files: &[(String, String)]) -> anyhow::Result<Vec<u8>> {
    let mut buf = Vec::new();
    {
        let mut zip = ZipWriter::new(std::io::Cursor::new(&mut buf));
        let options =
            SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
        for (name, content) in files {
            zip.start_file(name, options)?;
            zip.write_all(content.as_bytes())?;
        }
        zip.finish()?;
    }
    Ok(buf)
}
