//! `ritual pr-comment`: post the latest dual-review findings to a GitHub PR
//! via `gh`. One summary comment by default; `--inline` additionally tries
//! per-finding review comments (best-effort). The body passes through the
//! redactor: PR comments are outward-facing.

use std::io::Write as _;
use std::path::Path;

use anyhow::{Context, Result};

use crate::config::Config;
use crate::findings::{FindingsFile, LoadedFindings};
use crate::redact::Redactor;
use crate::state::RitualDirs;

/// The newest dual-review findings file (filenames are timestamp-prefixed).
fn latest_dual_review(findings_dir: &Path) -> Option<LoadedFindings> {
    crate::findings::load_all(findings_dir)
        .ok()?
        .into_iter()
        .find(|lf| {
            lf.path
                .file_name()
                .is_some_and(|n| n.to_string_lossy().contains("-dual-review"))
        })
}

/// Markdown body: header + one table row per confirmed, unresolved finding.
fn build_body(file: &FindingsFile) -> (String, usize) {
    let mut rows = Vec::new();
    for f in &file.findings {
        if !crate::findings::verdict_confirmed(&f.verdict) || f.resolved() {
            continue; // dismissed/fixed findings stay off the PR
        }
        let sources = if f.cross_confirmed() {
            "◆ both"
        } else {
            "◇ single"
        };
        rows.push(format!(
            "| {} | {} | `{}` | {} | {} |",
            f.severity.label(),
            sources,
            f.location(),
            f.title.replace('|', "\\|"),
            f.scenario.replace('|', "\\|"),
        ));
    }
    let mut body = format!(
        "## ritual dual-review\n\n\
         Cross-model review ({}) on `{}`, {}.\n\n",
        file.source_models
            .keys()
            .cloned()
            .collect::<Vec<_>>()
            .join(" + "),
        file.branch,
        file.generated_at
    );
    if rows.is_empty() {
        body.push_str("No unresolved confirmed findings. ✓\n");
    } else {
        body.push_str(
            "| severity | sources | location | finding | scenario |\n|---|---|---|---|---|\n",
        );
        body.push_str(&rows.join("\n"));
        body.push('\n');
    }
    (body, rows.len())
}

fn gh(cfg: &Config) -> std::process::Command {
    let mut cmd = std::process::Command::new(&cfg.gh_cmd[0]);
    cmd.args(&cfg.gh_cmd[1..]);
    cmd
}

/// PR number from the arg, else the PR associated with the current branch.
fn resolve_pr(cfg: &Config, pr: Option<u32>) -> Result<u32> {
    if let Some(n) = pr {
        return Ok(n);
    }
    let out = gh(cfg)
        .args(["pr", "view", "--json", "number"])
        .output()
        .context("running gh pr view (is gh installed?)")?;
    anyhow::ensure!(
        out.status.success(),
        "no PR for this branch; pass a PR number: ritual pr-comment <N>"
    );
    let v: serde_json::Value = serde_json::from_slice(&out.stdout)?;
    v["number"]
        .as_u64()
        .map(|n| n as u32)
        .context("gh pr view returned no number")
}

pub fn pr_comment(cfg: &Config, dirs: &RitualDirs, pr: Option<u32>, inline: bool) -> Result<()> {
    let latest = latest_dual_review(&dirs.findings_dir())
        .context("no dual-review findings recorded. Run `ritual run dual-review` first")?;
    let (raw_body, posted) = build_body(&latest.file);
    // PR comments leave the machine: redact like any other outward artifact.
    let body = Redactor::new(cfg.redaction).text(&raw_body);
    let pr = resolve_pr(cfg, pr)?;

    // Body via stdin, no argv-length or quoting hazards.
    let mut child = gh(cfg)
        .args(["pr", "comment", &pr.to_string(), "--body-file", "-"])
        .stdin(std::process::Stdio::piped())
        .spawn()
        .context("running gh pr comment")?;
    child
        .stdin
        .as_mut()
        .context("gh stdin")?
        .write_all(body.as_bytes())?;
    let status = child.wait()?;
    anyhow::ensure!(status.success(), "gh pr comment failed");
    println!(
        "posted summary comment on #{pr} ({posted} finding(s) from {})",
        latest
            .path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    );

    if inline {
        post_inline(cfg, &latest.file, pr);
    }
    Ok(())
}

/// One inline review comment's markdown: severity + title + scenario, plus
/// the anchored snippet as evidence when the finding carries one.
fn inline_body(f: &crate::findings::Finding) -> String {
    let snippet = f
        .snippet
        .as_deref()
        .map(|s| format!("\n\n```\n{s}\n```"))
        .unwrap_or_default();
    format!(
        "**{}**{}: {}\n\n{}{}",
        f.severity.label(),
        if f.cross_confirmed() {
            " · ◆ both models"
        } else {
            ""
        },
        f.title,
        f.scenario,
        snippet
    )
}

/// Best-effort per-finding review comments: each needs a file+line and the
/// PR's head commit; individual failures are warnings, never fatal.
fn post_inline(cfg: &Config, file: &FindingsFile, pr: u32) {
    let head = gh(cfg)
        .args(["pr", "view", &pr.to_string(), "--json", "headRefOid"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| serde_json::from_slice::<serde_json::Value>(&o.stdout).ok())
        .and_then(|v| v["headRefOid"].as_str().map(str::to_string));
    let Some(commit) = head else {
        eprintln!("  ⚠ could not resolve the PR head commit; inline comments skipped");
        return;
    };
    let redactor = |s: &str| Redactor::new(cfg.redaction).text(s);
    let (mut ok, mut failed) = (0usize, 0usize);
    for f in &file.findings {
        if !crate::findings::verdict_confirmed(&f.verdict) || f.resolved() {
            continue;
        }
        let (Some(path), Some(line)) = (&f.file, f.line) else {
            continue; // no location -> summary table only
        };
        let body = redactor(&inline_body(f));
        let status = gh(cfg)
            .args([
                "api",
                &format!("repos/{{owner}}/{{repo}}/pulls/{pr}/comments"),
                "-f",
                &format!("body={body}"),
                "-f",
                &format!("commit_id={commit}"),
                "-f",
                &format!("path={path}"),
                "-F",
                &format!("line={line}"),
                "-f",
                "side=RIGHT",
            ])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
        match status {
            Ok(s) if s.success() => ok += 1,
            _ => failed += 1,
        }
    }
    println!("  inline: {ok} posted, {failed} failed (best-effort)");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file_from(json: &str) -> FindingsFile {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn body_includes_open_findings_and_excludes_resolved() {
        let file = file_from(
            r#"{"stage":"dual-review","branch":"feat-x","generated_at":"2026-07-12",
                "source_models":{"claude":"c","codex":"x"},
                "findings":[
                  {"title":"real bug","severity":"critical","verdict":"confirmed",
                   "file":"src/a.rs","line":3,"scenario":"boom","sources":["claude","codex"]},
                  {"title":"dismissed noise","severity":"major","verdict":"confirmed","action":"dismissed"},
                  {"title":"unconfirmed","severity":"major","verdict":"unconfirmed"}
                ]}"#,
        );
        let (body, n) = build_body(&file);
        assert_eq!(n, 1);
        assert!(body.contains("real bug"));
        assert!(body.contains("◆ both"));
        assert!(body.contains("claude + codex"));
        assert!(!body.contains("dismissed noise"));
        assert!(!body.contains("unconfirmed"));
    }

    #[test]
    fn inline_body_carries_the_snippet_as_evidence() {
        let file = file_from(
            r#"{"stage":"dual-review","findings":[
                  {"title":"off-by-one","severity":"major","verdict":"confirmed",
                   "file":"src/a.rs","line":3,"scenario":"last item skipped",
                   "snippet":"for i in 0..len - 1 {","sources":["claude","codex"]},
                  {"title":"no snippet","severity":"minor","verdict":"confirmed"}
                ]}"#,
        );
        let body = inline_body(&file.findings[0]);
        assert!(body.contains("**major** · ◆ both models: off-by-one"));
        assert!(body.contains("```\nfor i in 0..len - 1 {\n```"));
        // Absent snippet -> no empty fence.
        assert!(!inline_body(&file.findings[1]).contains("```"));
    }

    #[test]
    fn body_with_nothing_open_says_so() {
        let file = file_from(r#"{"stage":"dual-review","findings":[]}"#);
        let (body, n) = build_body(&file);
        assert_eq!(n, 0);
        assert!(body.contains("No unresolved confirmed findings"));
    }

    #[test]
    fn latest_dual_review_prefers_newest_and_skips_other_stages() {
        let tmp = tempfile::tempdir().unwrap();
        for (name, title) in [
            ("20260710T000000Z-dual-review.json", "old"),
            ("20260712T000000Z-plan-review.json", "wrong stage"),
            ("20260711T000000Z-dual-review.json", "newest dual"),
        ] {
            std::fs::write(
                tmp.path().join(name),
                format!(r#"{{"stage":"x","findings":[{{"title":"{title}"}}]}}"#),
            )
            .unwrap();
        }
        let latest = latest_dual_review(tmp.path()).unwrap();
        assert_eq!(latest.file.findings[0].title, "newest dual");
    }
}
