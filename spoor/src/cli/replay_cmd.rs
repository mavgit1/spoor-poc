//! `spoor sessions` and `spoor replay`.

use std::path::Path;

use anyhow::{Result, bail};

use crate::export;
use crate::log;
use crate::pipeline::{self, AnalysisResult};
use crate::session::{self, LoadedSession, SessionStore, format_bytes};
use crate::types::{GenerateRequest, GenerateSelection};

pub async fn run_sessions() -> Result<()> {
    let store = SessionStore::default_store();
    let list = store.list()?;
    if list.is_empty() {
        log::info(format!(
            "no stored sessions under {}",
            store.root().display()
        ));
        log::info(format!(
            "keep {} newest, cap {} — set SPOOR_SESSION_KEEP / SPOOR_SESSION_MAX_MB",
            session::keep_count(),
            format_bytes(session::max_bytes())
        ));
        return Ok(());
    }

    println!("{:<28} {:>7} {:>10}  ENDED", "SESSION", "FLOWS", "SIZE");
    for s in &list {
        let ended =
            s.meta
                .ended_at
                .as_deref()
                .unwrap_or(if s.meta.gzipped { "yes" } else { "recording?" });
        println!(
            "{:<28} {:>7} {:>10}  {}",
            s.meta.id,
            s.meta.flow_count,
            format_bytes(s.size_bytes),
            ended
        );
    }
    log::info(format!(
        "{} session(s) in {}",
        list.len(),
        store.root().display()
    ));
    Ok(())
}

pub async fn run_replay(
    target: &str,
    select: &[String],
    all: bool,
    out: Option<&Path>,
    no_llm: bool,
) -> Result<()> {
    if all && !select.is_empty() {
        bail!("use either --all or --select, not both");
    }
    if out.is_some() && !all && select.is_empty() {
        bail!(
            "nothing selected — pass --all or --select <id>… to generate a pack (patterns are not selected by default)"
        );
    }

    if no_llm {
        pipeline::disable_llm();
    }

    let loaded = session::load_source(target)?;
    log::info(format!(
        "replay {} — {} flow(s){}",
        display_source(&loaded, target),
        loaded.flows.len(),
        if loaded.meta.flows_capped {
            ", capped"
        } else {
            ""
        }
    ));

    let AnalysisResult {
        classified,
        candidates,
        coverage,
    } = pipeline::analyze(&loaded.flows, loaded.meta.flows_capped).await;

    print_candidates(&candidates);

    let Some(out) = out else {
        return Ok(());
    };

    let selected = if all {
        candidates
            .iter()
            .map(|c| GenerateSelection {
                id: c.id.clone(),
                pattern: Some(c.guessed_pattern.clone()),
            })
            .collect::<Vec<_>>()
    } else {
        let mut sel = Vec::new();
        for id in select {
            let c = candidates.iter().find(|c| c.id == *id).ok_or_else(|| {
                anyhow::anyhow!("unknown candidate id {id:?} — run without --out to list ids")
            })?;
            sel.push(GenerateSelection {
                id: c.id.clone(),
                pattern: Some(c.guessed_pattern.clone()),
            });
        }
        sel
    };

    if selected.is_empty() {
        bail!("no candidates to export");
    }

    let selected_n = selected.len();
    let req = GenerateRequest {
        origin: None,
        selected,
        ignore_patterns: vec![],
        redact: false,
    };
    let result = export::generate_bundle_with_coverage(
        &classified,
        &candidates,
        &req,
        &loaded.flows,
        &coverage,
        &loaded.meta.pages,
    )?;
    if let Some(parent) = out.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(out, &result.bundle.zip_bytes)?;
    for w in &result.warnings {
        log::warn(w);
    }
    log::info(format!(
        "wrote {} ({} bytes, {selected_n} selected)",
        out.display(),
        result.bundle.zip_bytes.len()
    ));
    Ok(())
}

fn display_source(loaded: &LoadedSession, spec: &str) -> String {
    if !loaded.path.as_os_str().is_empty() {
        loaded.meta.id.clone()
    } else {
        spec.to_string()
    }
}

fn print_candidates(candidates: &[crate::types::Candidate]) {
    if candidates.is_empty() {
        log::info("no candidates");
        return;
    }
    println!("{:>5}  {:<10}  {:<40}  ID", "N×", "PROTO", "LABEL");
    for c in candidates {
        let label: String = c.label.chars().take(40).collect();
        println!(
            "{:>5}  {:<10}  {:<40}  {}",
            c.request_count, c.protocol, label, c.id
        );
    }
}
