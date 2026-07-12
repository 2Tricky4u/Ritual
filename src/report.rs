//! `ritual report` — one Markdown document summarizing a feature's journey:
//! spec → plan → findings → runs → costs. Optionally converted to PDF via
//! pandoc. All content passes through redaction.

use std::path::PathBuf;

use anyhow::{Context, Result};
use chrono::Utc;

use crate::config::Config;
use crate::findings::{self, Severity};
use crate::history;
use crate::redact::Redactor;
use crate::state::{self, PIPELINE, RitualDirs, State};

pub struct ReportOutcome {
    pub markdown: PathBuf,
    pub pdf: Option<PathBuf>,
}

pub fn generate(
    cfg: &Config,
    dirs: &RitualDirs,
    feature: Option<&str>,
    pdf: bool,
) -> Result<ReportOutcome> {
    let slug = match feature {
        Some(f) => state::branch_slug(f),
        None => state::branch_slug(
            &state::current_branch(&dirs.project_root).unwrap_or_else(|| "detached".into()),
        ),
    };
    let st = State::load(dirs)?;
    let feat = st
        .features
        .get(&slug)
        .with_context(|| format!("no feature '{slug}' in .ritual/state.json"))?;

    let spec = std::fs::read_to_string(dirs.spec_file(&slug)).ok();
    let plan = std::fs::read_to_string(dirs.plan_file(&slug)).ok();
    let loaded = findings::load_all(&dirs.findings_dir())?;
    let metas: Vec<_> = history::load_all(&dirs.runs_dir())?
        .into_iter()
        .filter(|m| m.branch == feat.branch || m.branch.is_empty())
        .collect();

    let mut md = String::new();
    let now = Utc::now();
    md.push_str(&format!(
        "---\ntitle: \"{}\"\nsubtitle: \"ritual report — branch {}\"\ndate: {}\n---\n\n",
        feat.title.replace('"', "'"),
        feat.branch,
        now.format("%Y-%m-%d")
    ));

    // Pipeline state.
    md.push_str("## Pipeline\n\n| stage | status |\n|---|---|\n");
    for id in PIPELINE {
        let s = feat.stage(*id);
        md.push_str(&format!("| {} | {:?} |\n", id.label(), s.status));
    }
    md.push('\n');

    if let Some(spec) = &spec {
        md.push_str("## Spec\n\n");
        md.push_str(spec);
        md.push_str("\n\n");
    }
    if let Some(plan) = &plan {
        md.push_str("## Plan\n\n");
        md.push_str(plan);
        md.push_str("\n\n");
    }

    // Findings, most severe first. Reports are the record: include resolved
    // ones too — the action column shows fixed/dismissed.
    let agg = findings::aggregate(&loaded, true);
    md.push_str("## Findings\n\n");
    if agg.is_empty() {
        md.push_str("_none recorded_\n\n");
    } else {
        md.push_str("| severity | sources | location | finding | verdict | action |\n|---|---|---|---|---|---|\n");
        for af in &agg {
            let f = &af.finding;
            let sources = if f.cross_confirmed() {
                "both".to_string()
            } else {
                f.sources.join("+")
            };
            let action = if f.action.is_empty() {
                loaded[af.file_idx].file.stage.clone()
            } else {
                f.action.clone()
            };
            md.push_str(&format!(
                "| {} | {} | {} | {} | {} | {} |\n",
                f.severity.label(),
                sources,
                f.location(),
                f.title.replace('|', "\\|"),
                f.verdict,
                action,
            ));
        }
        let criticals = agg
            .iter()
            .filter(|af| af.finding.severity == Severity::Critical)
            .count();
        md.push_str(&format!(
            "\n{} findings total, {} critical.\n\n",
            agg.len(),
            criticals
        ));
        // Anchored source excerpts (tables can't hold code blocks).
        let with_snippets: Vec<_> = agg
            .iter()
            .filter(|af| af.finding.snippet.is_some())
            .collect();
        if !with_snippets.is_empty() {
            md.push_str("### Evidence\n\n");
            for af in with_snippets {
                let f = &af.finding;
                md.push_str(&format!("**{}** — {}\n\n", f.location(), f.title));
                md.push_str(&format!("```\n{}\n```\n\n", f.snippet.as_deref().unwrap()));
            }
        }
    }

    // Runs + spend.
    md.push_str("## Runs\n\n");
    if metas.is_empty() {
        md.push_str("_no recorded runs_\n\n");
    } else {
        md.push_str("| when (UTC) | stage | agent | ok | cost | tokens out | duration |\n|---|---|---|---|---|---|---|\n");
        let mut total_cost = 0.0;
        for m in &metas {
            total_cost += m.total_cost_usd.unwrap_or(0.0);
            md.push_str(&format!(
                "| {} | {} | {} | {} | {} | {} | {} |\n",
                m.started_at
                    .map(|d| d.format("%m-%d %H:%M").to_string())
                    .unwrap_or_else(|| "?".into()),
                m.stage,
                m.agent,
                if m.ok { "yes" } else { "NO" },
                m.total_cost_usd
                    .map(|c| format!("${c:.3}"))
                    .unwrap_or_else(|| "-".into()),
                m.usage
                    .as_ref()
                    .map(|u| u.output_tokens.to_string())
                    .unwrap_or_else(|| "-".into()),
                m.duration_ms
                    .map(|d| format!("{:.0}s", d as f64 / 1000.0))
                    .unwrap_or_else(|| "-".into()),
            ));
        }
        md.push_str(&format!("\nTotal recorded spend: ${total_cost:.2}.\n\n"));
    }

    // Redact and write.
    let md = Redactor::new(cfg.redaction).text(&md);
    let reports_dir = dirs.root().join("reports");
    std::fs::create_dir_all(&reports_dir)?;
    let base = format!("{}-{}", now.format("%Y%m%dT%H%M%SZ"), slug);
    let md_path = reports_dir.join(format!("{base}.md"));
    std::fs::write(&md_path, &md).with_context(|| format!("writing {}", md_path.display()))?;

    let pdf_path = if pdf { convert_pdf(&md_path)? } else { None };
    Ok(ReportOutcome {
        markdown: md_path,
        pdf: pdf_path,
    })
}

/// Best-effort pandoc conversion. Engine order favors Unicode-correct
/// renderers first: typst (no LaTeX needed), then xelatex (handles the
/// report's Nerd-Font/box-drawing glyphs, unlike pdflatex), then pandoc's
/// default. Reports carry unicode, so a plain pdflatex-only box fails —
/// falling through to markdown-only is the documented graceful degrade.
fn convert_pdf(md: &std::path::Path) -> Result<Option<PathBuf>> {
    let out = md.with_extension("pdf");
    for args in [
        vec!["--pdf-engine=typst"],
        vec!["--pdf-engine=xelatex"],
        vec![], // pandoc's default engine
    ] {
        let status = std::process::Command::new("pandoc")
            .arg(md)
            .arg("-o")
            .arg(&out)
            .args(&args)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
        if let Ok(s) = status
            && s.success()
            && out.exists()
        {
            return Ok(Some(out));
        }
    }
    eprintln!(
        "pdf conversion failed (is pandoc + typst or a LaTeX engine installed?) — markdown kept"
    );
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::StageId;

    #[test]
    fn report_contains_sections_and_redacts() {
        let tmp = tempfile::tempdir().unwrap();
        let dirs = RitualDirs::new(tmp.path());
        crate::scaffold::init(tmp.path(), false).unwrap();

        let mut st = State::load(&dirs).unwrap();
        let f = st.feature_for_branch_mut("feat/r");
        f.title = "Report Feature".into();
        f.stages.entry(StageId::PlanReview).or_default().status = crate::state::StageStatus::Done;
        st.save(&dirs).unwrap();

        std::fs::create_dir_all(dirs.feature_dir("feat-r")).unwrap();
        std::fs::write(
            dirs.spec_file("feat-r"),
            "spec with secret api_key = \"hunter2hunter2\"",
        )
        .unwrap();
        std::fs::write(
            dirs.findings_dir()
                .join("20260711T000000Z-dual-review.json"),
            r#"{"ritual_findings":1,"stage":"dual-review","findings":[
                {"id":1,"severity":"critical","title":"bad thing","file":"a.rs","line":1,
                 "sources":["claude","codex"],"verdict":"confirmed","action":"pending"}]}"#,
        )
        .unwrap();
        std::fs::write(
            dirs.runs_dir().join("20260711T000000Z-x.meta.json"),
            r#"{"run_id":"r","stage":"plan-review","branch":"feat/r","agent":"claude",
                "ok":true,"total_cost_usd":0.5,"started_at":"2026-07-11T00:00:00Z"}"#,
        )
        .unwrap();

        let cfg = Config::default();
        let out = generate(&cfg, &dirs, Some("feat/r"), false).unwrap();
        let text = std::fs::read_to_string(&out.markdown).unwrap();
        assert!(text.contains("## Pipeline"));
        assert!(text.contains("## Findings"));
        assert!(text.contains("bad thing"));
        assert!(text.contains("## Runs"));
        assert!(text.contains("$0.50"));
        assert!(
            text.contains("[REDACTED:assignment]"),
            "secret must not survive"
        );
        assert!(!text.contains("hunter2hunter2"));
    }
}
