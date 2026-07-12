use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::Utc;

use crate::config::Config;
use crate::findings::{Finding, FindingsFile, Severity};
use crate::state::RitualDirs;

/// What one secrets scan saw and produced.
#[derive(Debug, Default)]
pub struct SecretsReport {
    pub scanned_files: usize,
    pub leaks: usize,
    pub findings_path: Option<PathBuf>,
}

/// True when the configured gitleaks binary answers `version`.
pub fn available(cfg: &Config) -> bool {
    crate::agents_status::run_capture(&cfg.gitleaks_cmd, &["version"])
        .is_some_and(|o| o.status.success())
}

/// Scan what changed (tracked modifications + untracked files — exactly the
/// agent's attack surface) for leaked secrets. Changed files are staged into
/// a temp dir preserving relative paths and scanned with ONE `gitleaks dir`
/// run: full file/line anchoring (a piped diff loses it) while staying
/// diff-scoped, and `.gitleaksignore` fingerprints keep matching. Hits become
/// critical/confirmed findings, so the existing exit-code/CI contract blocks
/// until a human dismisses (d) or fingerprints them.
pub fn scan(cfg: &Config, dirs: &RitualDirs) -> Result<SecretsReport> {
    let files = changed_files(dirs)?;
    if files.is_empty() {
        return Ok(SecretsReport::default());
    }

    let stage = tempfile::tempdir().context("creating scan dir")?;
    let mut staged = 0usize;
    for rel in &files {
        let src = dirs.work_root.join(rel);
        if !src.is_file() {
            continue; // deleted files, submodules
        }
        let dst = stage.path().join(rel);
        if let Some(parent) = dst.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::copy(&src, &dst).with_context(|| format!("staging {rel:?}"))?;
        staged += 1;
    }
    // Repo-level suppressions ride along (fingerprints are scan-root-relative).
    let ignore = dirs.work_root.join(".gitleaksignore");
    if ignore.is_file() {
        let _ = std::fs::copy(&ignore, stage.path().join(".gitleaksignore"));
    }
    if staged == 0 {
        return Ok(SecretsReport::default());
    }

    let report_path = stage.path().join("gitleaks-report.json");
    let argv = &cfg.gitleaks_cmd;
    let out = std::process::Command::new(&argv[0])
        .args(&argv[1..])
        .arg("dir")
        .arg(stage.path())
        .args([
            "--no-banner",
            "--redact",
            "--report-format",
            "json",
            "--exit-code",
            "2",
        ])
        .arg("--report-path")
        .arg(&report_path)
        .output()
        .with_context(|| format!("running `{}`", argv.join(" ")))?;

    match out.status.code() {
        Some(0) => {
            return Ok(SecretsReport {
                scanned_files: staged,
                ..Default::default()
            });
        }
        Some(2) => {} // leaks found — the gate's whole point
        c => anyhow::bail!(
            "`{}` failed ({c:?}): {}",
            argv.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        ),
    }

    let text = std::fs::read_to_string(&report_path)
        .with_context(|| format!("reading {}", report_path.display()))?;
    let findings = findings_from_report(&text, stage.path(), cfg.redaction)?;
    let leaks = findings.len();
    if findings.is_empty() {
        return Ok(SecretsReport {
            scanned_files: staged,
            ..Default::default()
        });
    }

    let file = FindingsFile {
        ritual_findings: 1,
        stage: "secrets".into(),
        branch: crate::state::current_branch(&dirs.work_root).unwrap_or_default(),
        generated_at: Utc::now().to_rfc3339(),
        findings,
        ..Default::default()
    };
    std::fs::create_dir_all(dirs.findings_dir())?;
    let path = dirs.findings_dir().join(format!(
        "{}-secrets.json",
        Utc::now().format("%Y%m%dT%H%M%SZ")
    ));
    std::fs::write(&path, serde_json::to_string_pretty(&file)?)?;
    Ok(SecretsReport {
        scanned_files: staged,
        leaks,
        findings_path: Some(path),
    })
}

/// Best-effort pre-review scan for the dual-review spawn path: never fatal,
/// returns a one-line notice when something is worth telling the user.
pub fn preflight(cfg: &Config, dirs: &RitualDirs) -> Option<String> {
    if !cfg.secrets_enabled {
        return None;
    }
    if !available(cfg) {
        return Some("secrets gate skipped — gitleaks not installed".into());
    }
    match scan(cfg, dirs) {
        Ok(r) if r.leaks > 0 => Some(format!(
            "gitleaks: {} leak(s) in changed files → critical findings (block until dismissed)",
            r.leaks
        )),
        Ok(_) => None,
        Err(e) => Some(format!("secrets scan failed: {e:#}")),
    }
}

/// Tracked modifications vs HEAD plus untracked (non-ignored) files.
fn changed_files(dirs: &RitualDirs) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    for args in [
        &["diff", "--name-only", "HEAD"][..],
        &["ls-files", "--others", "--exclude-standard"][..],
    ] {
        let out = std::process::Command::new("git")
            .args(args)
            .current_dir(&dirs.work_root)
            .output()
            .context("running git")?;
        // `git diff HEAD` fails on a repo with no commits — treat as empty.
        if !out.status.success() {
            continue;
        }
        for line in String::from_utf8_lossy(&out.stdout).lines() {
            let p = PathBuf::from(line.trim());
            if !line.trim().is_empty() && !files.contains(&p) {
                files.push(p);
            }
        }
    }
    Ok(files)
}

/// Pure mapper over a gitleaks JSON report. Drift-tolerant: missing fields
/// default, unknown ones are ignored. `scan_root` strips the temp-stage
/// prefix so findings anchor at repo-relative paths.
fn findings_from_report(text: &str, scan_root: &Path, redact: bool) -> Result<Vec<Finding>> {
    let v: serde_json::Value = serde_json::from_str(text).context("parsing gitleaks report")?;
    let mut redactor = crate::redact::Redactor::new(redact);
    let empty = Vec::new();
    let mut findings = Vec::new();
    for (i, hit) in v.as_array().unwrap_or(&empty).iter().enumerate() {
        let rule = hit["RuleID"].as_str().unwrap_or("secret");
        let file = hit["File"].as_str().map(|f| {
            Path::new(f)
                .strip_prefix(scan_root)
                .map(|p| p.display().to_string())
                .unwrap_or_else(|_| f.to_string())
        });
        let mut extra = serde_json::Map::new();
        if let Some(fp) = hit["Fingerprint"].as_str() {
            // Repo-relative too, so it can be pasted into .gitleaksignore.
            extra.insert(
                "fingerprint".into(),
                serde_json::Value::String(fp.replace(&format!("{}/", scan_root.display()), "")),
            );
        }
        let mut title = format!("secret: {rule}");
        title.truncate(80);
        findings.push(Finding {
            id: (i + 1) as u32,
            severity: Severity::Critical,
            title,
            file,
            line: hit["StartLine"].as_u64().map(|l| l as u32),
            snippet: hit["Line"]
                .as_str()
                .map(|l| redactor.text(l.trim()))
                .filter(|l| !l.is_empty()),
            scenario: format!(
                "{} — committed secrets outlive the commit; rotate it and use env/secret storage",
                hit["Description"]
                    .as_str()
                    .unwrap_or("secret detected in the diff")
            ),
            sources: vec!["gitleaks".into()],
            verdict: "confirmed".into(),
            action: "pending".into(),
            extra,
            ..Default::default()
        });
    }
    Ok(findings)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_report_hits_to_critical_findings() {
        let report = r#"[
            {"RuleID": "generic-api-key", "Description": "Generic API Key",
             "File": "/scan/root/cfg/leaky.py", "StartLine": 3,
             "Line": "api_key = \"REDACTED\"",
             "Fingerprint": "/scan/root/cfg/leaky.py:generic-api-key:3",
             "Secret": "REDACTED", "Entropy": 3.5}
        ]"#;
        let f = findings_from_report(report, Path::new("/scan/root"), true).unwrap();
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].severity, Severity::Critical);
        assert_eq!(f[0].title, "secret: generic-api-key");
        assert_eq!(f[0].file.as_deref(), Some("cfg/leaky.py"));
        assert_eq!(f[0].line, Some(3));
        assert_eq!(f[0].verdict, "confirmed");
        // ritual's own redactor runs over the snippet too (belt + braces on
        // top of gitleaks --redact): the assignment is masked wholesale.
        assert!(f[0].snippet.as_deref().unwrap().contains("[REDACTED"));
        assert_eq!(
            f[0].extra["fingerprint"], "cfg/leaky.py:generic-api-key:3",
            "fingerprint must be pasteable into .gitleaksignore"
        );
    }

    #[test]
    fn tolerates_sparse_and_empty_reports() {
        let f = findings_from_report(r#"[{"RuleID":"x"}]"#, Path::new("/s"), true).unwrap();
        assert_eq!(f[0].file, None);
        assert_eq!(f[0].snippet, None);
        assert!(
            findings_from_report("[]", Path::new("/s"), true)
                .unwrap()
                .is_empty()
        );
        assert!(findings_from_report("junk", Path::new("/s"), true).is_err());
    }
}
