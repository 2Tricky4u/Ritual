use std::path::PathBuf;

use anyhow::{Context, Result};
use chrono::Utc;

use crate::config::Config;
use crate::findings::{Finding, FindingsFile, Severity};
use crate::state::RitualDirs;

/// What one `ritual mutants` invocation saw and produced.
#[derive(Debug, Default)]
pub struct MutantsReport {
    pub caught: usize,
    pub missed: usize,
    pub unviable: usize,
    pub timeout: usize,
    /// Findings file written when mutants survived.
    pub findings_path: Option<PathBuf>,
    /// True when the diff against the base ref was empty.
    pub empty_diff: bool,
}

/// Mutation-kill gate over the current diff (Meta-ACH style): mutate only
/// the changed code, run the tests, and turn every SURVIVING mutant into a
/// major/confirmed finding: proof the diff's tests don't discriminate it.
/// Advisory by design: major findings never block the CI contract; the
/// findings tab (f/d) is the adjudication surface.
pub fn run(cfg: &Config, dirs: &RitualDirs, base: Option<&str>) -> Result<MutantsReport> {
    let base_ref = base.unwrap_or(&cfg.base_ref);

    // Default `git diff` output keeps the b/ prefixes --in-diff matches on.
    let diff = std::process::Command::new("git")
        .args(["diff", base_ref])
        .current_dir(&dirs.work_root)
        .output()
        .context("running git diff")?;
    anyhow::ensure!(
        diff.status.success(),
        "git diff {base_ref} failed: {}",
        String::from_utf8_lossy(&diff.stderr).trim()
    );
    if diff.stdout.is_empty() {
        return Ok(MutantsReport {
            empty_diff: true,
            ..Default::default()
        });
    }

    let scratch = tempfile::tempdir().context("creating scratch dir")?;
    let diff_path = scratch.path().join("changes.diff");
    std::fs::write(&diff_path, &diff.stdout)?;

    // Progress streams straight to the user's terminal (mutant builds are
    // slow); only the artifacts are parsed.
    let argv = &cfg.mutants_cmd;
    let status = std::process::Command::new(&argv[0])
        .args(&argv[1..])
        .arg("--in-diff")
        .arg(&diff_path)
        .arg("--no-shuffle")
        .arg("--timeout")
        .arg(cfg.mutants_timeout_secs.to_string())
        .arg("--output")
        .arg(scratch.path())
        .current_dir(&dirs.work_root)
        .status()
        .with_context(|| {
            format!(
                "running `{}` (cargo install cargo-mutants?)",
                argv.join(" ")
            )
        })?;

    // 0 = all caught (or nothing to mutate), 2 = missed mutants (the gate's
    // whole point, not an error), 3 = timeouts occurred. 4 means the tree's
    // own tests already fail: nothing was measured.
    match status.code() {
        Some(0) | Some(2) | Some(3) => {}
        Some(4) => anyhow::bail!("baseline tests already failing; get ./check.sh green first"),
        Some(1) => anyhow::bail!(
            "`{}` rejected its arguments (version too old?)",
            argv.join(" ")
        ),
        c => anyhow::bail!("`{}` exited abnormally ({c:?})", argv.join(" ")),
    }

    let outcomes_path = scratch.path().join("mutants.out/outcomes.json");
    let text = std::fs::read_to_string(&outcomes_path)
        .with_context(|| format!("reading {}", outcomes_path.display()))?;
    let (mut report, findings) = parse_outcomes(&text)?;

    if !findings.is_empty() {
        let file = FindingsFile {
            ritual_findings: 1,
            stage: "mutants".into(),
            branch: crate::state::current_branch(&dirs.work_root).unwrap_or_default(),
            generated_at: Utc::now().to_rfc3339(),
            findings,
            ..Default::default()
        };
        std::fs::create_dir_all(dirs.findings_dir())?;
        let path = dirs.findings_dir().join(format!(
            "{}-mutants.json",
            Utc::now().format("%Y%m%dT%H%M%SZ")
        ));
        std::fs::write(&path, serde_json::to_string_pretty(&file)?)?;
        report.findings_path = Some(path);
    }
    Ok(report)
}

/// Pure parser over cargo-mutants' `outcomes.json`. Drift-tolerant like the
/// agent event parsers: unknown summaries and missing fields are skipped or
/// defaulted, never fatal.
fn parse_outcomes(text: &str) -> Result<(MutantsReport, Vec<Finding>)> {
    let v: serde_json::Value = serde_json::from_str(text).context("parsing outcomes.json")?;
    let mut report = MutantsReport::default();
    let mut findings = Vec::new();
    let empty = Vec::new();
    for o in v["outcomes"].as_array().unwrap_or(&empty) {
        match o["summary"].as_str().unwrap_or("") {
            "CaughtMutant" => report.caught += 1,
            "Unviable" => report.unviable += 1,
            "Timeout" => report.timeout += 1,
            "MissedMutant" => {
                report.missed += 1;
                findings.push(finding_from(
                    &o["scenario"]["Mutant"],
                    findings.len() as u32,
                ));
            }
            _ => {} // Baseline Success/Failure, future variants
        }
    }
    Ok((report, findings))
}

fn finding_from(mutant: &serde_json::Value, idx: u32) -> Finding {
    let genre = mutant["genre"].as_str().unwrap_or("mutation");
    let function = mutant["function"]["function_name"].as_str().unwrap_or("?");
    let mut title = format!("surviving mutant: {genre} in {function}");
    title.truncate(80);
    Finding {
        id: idx + 1,
        severity: Severity::Major,
        title,
        file: mutant["file"].as_str().map(str::to_string),
        line: mutant["span"]["start"]["line"].as_u64().map(|l| l as u32),
        snippet: mutant["replacement"]
            .as_str()
            .filter(|r| !r.is_empty())
            .map(|r| format!("mutated to: {r}")),
        scenario: "the test suite passes with this mutation applied, and the diff's tests do not discriminate it".into(),
        sources: vec!["cargo-mutants".into()],
        verdict: "confirmed".into(),
        action: "pending".into(),
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const OUTCOMES: &str = r#"{
        "cargo_mutants_version": "27.1.0",
        "outcomes": [
            {"scenario": "Baseline", "summary": "Success"},
            {"scenario": {"Mutant": {"package": "ritual", "file": "src/a.rs",
                "function": {"function_name": "clamp", "return_type": "-> u32"},
                "span": {"start": {"line": 42, "column": 5}, "end": {"line": 42, "column": 20}},
                "genre": "FnValue", "replacement": "0"}},
             "summary": "MissedMutant"},
            {"scenario": {"Mutant": {"file": "src/b.rs",
                "function": {"function_name": "load"},
                "span": {"start": {"line": 7}}, "genre": "FnValue", "replacement": "Ok(Default::default())"}},
             "summary": "CaughtMutant"},
            {"scenario": {"Mutant": {"file": "src/c.rs"}}, "summary": "Unviable"},
            {"scenario": {"Mutant": {"file": "src/d.rs"}}, "summary": "Timeout"},
            {"scenario": {"Mutant": {"file": "src/e.rs"}}, "summary": "SomeFutureVariant"}
        ],
        "summary": {"total_mutants": 5}
    }"#;

    #[test]
    fn parses_counts_and_maps_missed_to_findings() {
        let (report, findings) = parse_outcomes(OUTCOMES).unwrap();
        assert_eq!(report.caught, 1);
        assert_eq!(report.missed, 1);
        assert_eq!(report.unviable, 1);
        assert_eq!(report.timeout, 1);

        assert_eq!(findings.len(), 1);
        let f = &findings[0];
        assert_eq!(f.title, "surviving mutant: FnValue in clamp");
        assert_eq!(f.severity, Severity::Major);
        assert_eq!(f.file.as_deref(), Some("src/a.rs"));
        assert_eq!(f.line, Some(42));
        assert_eq!(f.snippet.as_deref(), Some("mutated to: 0"));
        assert_eq!(f.verdict, "confirmed");
        assert_eq!(f.sources, vec!["cargo-mutants"]);
    }

    #[test]
    fn finding_from_truncates_titles_and_skips_empty_replacement() {
        let long_fn = "f".repeat(100);
        let mutant = serde_json::json!({
            "genre": "FnValue",
            "function": {"function_name": long_fn},
            "file": "src/a.rs",
            "span": {"start": {"line": 1}},
            "replacement": ""
        });
        let f = finding_from(&mutant, 0);
        assert_eq!(f.title.chars().count(), 80, "hard cap for actionability");
        assert!(f.snippet.is_none(), "empty replacement -> no snippet");
    }

    #[test]
    fn tolerates_missing_fields_and_junk() {
        let (report, findings) =
            parse_outcomes(r#"{"outcomes":[{"scenario":{"Mutant":{}},"summary":"MissedMutant"}]}"#)
                .unwrap();
        assert_eq!(report.missed, 1);
        assert_eq!(findings[0].title, "surviving mutant: mutation in ?");
        assert_eq!(findings[0].file, None);
        assert_eq!(findings[0].snippet, None);

        // No outcomes array at all -> empty, not an error.
        let (report, findings) = parse_outcomes("{}").unwrap();
        assert_eq!(report.missed + report.caught, 0);
        assert!(findings.is_empty());

        // Actual junk IS an error (the tool contract broke).
        assert!(parse_outcomes("not json").is_err());
    }
}
