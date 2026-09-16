pub mod auth;
pub mod example_pick;
pub mod facets;
pub mod observations;
pub mod pack;
pub mod query_params;
pub mod session;
pub mod trim;

use std::io::Write;

use zip::ZipWriter;
use zip::write::SimpleFileOptions;

use crate::capture::CaptureRecord;
use crate::classify::{ClassifiedEntry, CoverageReport};
use crate::types::{BrowsingPage, Candidate, ExportBundle, GenerateRequest};

pub struct GenerateResult {
    pub bundle: ExportBundle,
    pub warnings: Vec<String>,
}

pub fn generate_bundle(
    classified: &[ClassifiedEntry],
    candidates: &[Candidate],
    req: &GenerateRequest,
) -> anyhow::Result<GenerateResult> {
    generate_bundle_with_coverage(
        classified,
        candidates,
        req,
        &[],
        &CoverageReport::default(),
        &[],
    )
}

pub fn generate_bundle_with_coverage(
    classified: &[ClassifiedEntry],
    candidates: &[Candidate],
    req: &GenerateRequest,
    flows: &[CaptureRecord],
    coverage: &CoverageReport,
    page_urls: &[BrowsingPage],
) -> anyhow::Result<GenerateResult> {
    let zip_files =
        pack::build_pack_files(classified, candidates, req, flows, coverage, page_urls)?;

    let mut origins_selected = std::collections::HashSet::new();
    for sel in &req.selected {
        if let Some(cand) = candidates.iter().find(|c| c.id == sel.id) {
            origins_selected.insert(cand.origin.clone());
        }
    }

    let warnings = auth::session_auth_warnings(classified, &origins_selected);
    let zip_bytes = build_zip(&zip_files)?;

    Ok(GenerateResult {
        bundle: ExportBundle { zip_bytes },
        warnings,
    })
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
