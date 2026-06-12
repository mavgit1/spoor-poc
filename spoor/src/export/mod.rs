pub mod graphql;
pub mod openapi;

use std::collections::BTreeMap;
use std::io::Write;

use zip::write::SimpleFileOptions;
use zip::ZipWriter;

use crate::classify::ClassifiedEntry;
use crate::types::{Candidate, ExportBundle, GenerateRequest};

pub fn generate_bundle(
    classified: &[ClassifiedEntry],
    candidates: &[Candidate],
    req: &GenerateRequest,
) -> anyhow::Result<ExportBundle> {
    let mut zip_files: Vec<(String, String)> = Vec::new();

    let mut rest_by_origin: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut gql_by_origin: BTreeMap<String, Vec<String>> = BTreeMap::new();

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

    let multi_file = rest_by_origin.len() + gql_by_origin.len() > 1
        || (rest_by_origin.len() == 1 && !gql_by_origin.is_empty())
        || (gql_by_origin.len() == 1 && !rest_by_origin.is_empty());

    for (origin, patterns) in rest_by_origin {
        let yaml = openapi::generate_openapi(classified, &origin, &patterns)?;
        let name = if multi_file {
            format!("openapi-{}.yaml", host_slug(&origin))
        } else {
            "openapi.yaml".to_string()
        };
        zip_files.push((name, yaml));
    }

    for (origin, ops) in gql_by_origin {
        let yaml = graphql::generate_operations_yaml(classified, &origin, &ops)?;
        let name = if multi_file {
            format!("graphql-ops-{}.yaml", host_slug(&origin))
        } else {
            "graphql-ops.yaml".to_string()
        };
        zip_files.push((name, yaml));
    }

    for pattern in &req.ignore_patterns {
        let _ = crate::classify::filters::persist_ignore(pattern);
    }

    if zip_files.is_empty() {
        anyhow::bail!("no export artifacts produced for selection");
    }

    let zip_bytes = build_zip(&zip_files)?;

    Ok(ExportBundle { zip_bytes })
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
