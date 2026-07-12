use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::{Deserialize, Serialize};

/// One findings file, written by the plan-review / dual-review skills.
/// Every field is defaulted: a missing field must never break the browser.
/// Unknown fields are preserved via `extra` so `set_action` rewrites never
/// silently drop data a newer skill wrote.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FindingsFile {
    #[serde(default)]
    pub ritual_findings: u32,
    #[serde(default)]
    pub stage: String,
    #[serde(default)]
    pub branch: String,
    #[serde(default)]
    pub generated_at: String,
    #[serde(default)]
    pub source_models: serde_json::Map<String, serde_json::Value>,
    #[serde(default)]
    pub findings: Vec<Finding>,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Finding {
    #[serde(default)]
    pub id: u32,
    #[serde(default)]
    pub severity: Severity,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub file: Option<String>,
    #[serde(default)]
    pub line: Option<u32>,
    #[serde(default)]
    pub plan_step: Option<String>,
    /// 1-3 verbatim source lines at the finding — hunk-anchored evidence
    /// (reviewers act on snippet-bearing findings; absent for plan findings).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snippet: Option<String>,
    #[serde(default)]
    pub scenario: String,
    #[serde(default)]
    pub sources: Vec<String>,
    #[serde(default)]
    pub verdict: String,
    #[serde(default)]
    pub action: String,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

impl Finding {
    /// Both models flagged it -> strongest signal.
    pub fn cross_confirmed(&self) -> bool {
        self.sources.len() >= 2
    }

    /// A human closed this finding out ("fixed" from the TUI or a skill,
    /// "dismissed" from the TUI). Anything else — "pending", "", free text —
    /// is unresolved. Resolved findings don't block CI or the exit code.
    pub fn resolved(&self) -> bool {
        matches!(self.action.as_str(), "fixed" | "dismissed")
    }

    pub fn location(&self) -> String {
        match (&self.file, self.line) {
            (Some(f), Some(l)) => format!("{f}:{l}"),
            (Some(f), None) => f.clone(),
            _ => self.plan_step.clone().unwrap_or_default(),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Critical,
    Major,
    #[default]
    Minor,
}

impl Severity {
    #[allow(dead_code)] // used by the TUI findings pane (M3)
    pub fn label(&self) -> &'static str {
        match self {
            Severity::Critical => "critical",
            Severity::Major => "major",
            Severity::Minor => "minor",
        }
    }
}

/// A findings file plus where it came from.
#[derive(Debug, Clone)]
pub struct LoadedFindings {
    #[allow(dead_code)] // used by the TUI editor-jump (M3)
    pub path: PathBuf,
    pub file: FindingsFile,
}

/// Load every parseable findings file, newest first (filenames start with a
/// UTC timestamp so lexicographic order == chronological order).
/// Unparseable files are skipped, never fatal.
pub fn load_all(findings_dir: &Path) -> Result<Vec<LoadedFindings>> {
    let mut out = Vec::new();
    if !findings_dir.is_dir() {
        return Ok(out);
    }
    let mut paths: Vec<PathBuf> = std::fs::read_dir(findings_dir)?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "json"))
        .collect();
    paths.sort();
    paths.reverse();
    for path in paths {
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        match serde_json::from_str::<FindingsFile>(&text) {
            Ok(file) => out.push(LoadedFindings { path, file }),
            Err(_) => continue, // tolerate junk; the browser must never die on one bad file
        }
    }
    Ok(out)
}

/// One finding plus its stable identity: the loaded-file index and the
/// finding's position within that file (ids/titles are free-form and not
/// unique, so position is the only safe write-back key).
#[derive(Debug, Clone)]
pub struct AggregatedFinding {
    pub file_idx: usize,
    pub pos: usize,
    pub finding: Finding,
}

/// Flatten + sort: severity first (critical on top), then newest file first.
/// The resolved filter lives HERE — the single chokepoint every consumer
/// (TUI selection, editor jump, quickfix, custom commands) goes through, so
/// `selected_finding` indexes stay consistent everywhere.
pub fn aggregate(loaded: &[LoadedFindings], show_resolved: bool) -> Vec<AggregatedFinding> {
    let mut all: Vec<AggregatedFinding> = loaded
        .iter()
        .enumerate()
        .flat_map(|(file_idx, lf)| {
            lf.file
                .findings
                .iter()
                .enumerate()
                .map(move |(pos, f)| AggregatedFinding {
                    file_idx,
                    pos,
                    finding: f.clone(),
                })
        })
        .filter(|af| show_resolved || !af.finding.resolved())
        .collect();
    all.sort_by_key(|af| (af.finding.severity, af.file_idx));
    all
}

/// Resolved findings hidden by the default view (for the "N hidden" footer).
pub fn resolved_count(loaded: &[LoadedFindings]) -> usize {
    loaded
        .iter()
        .flat_map(|lf| &lf.file.findings)
        .filter(|f| f.resolved())
        .count()
}

/// Set a finding's action ("fixed" / "dismissed"), toggling back to
/// "pending" when it already carries that action. Mutates in memory AND
/// rewrites the source file atomically (pretty JSON, tmp + rename) so the
/// resolution survives restarts and reaches CI/scripts.
pub fn set_action(
    loaded: &mut [LoadedFindings],
    file_idx: usize,
    pos: usize,
    action: &str,
) -> Result<()> {
    let lf = loaded
        .get_mut(file_idx)
        .ok_or_else(|| anyhow::anyhow!("no findings file at index {file_idx}"))?;
    let finding = lf
        .file
        .findings
        .get_mut(pos)
        .ok_or_else(|| anyhow::anyhow!("no finding at position {pos}"))?;
    finding.action = if finding.action == action {
        "pending".to_string() // toggle off
    } else {
        action.to_string()
    };
    let tmp = lf.path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(&lf.file)?)?;
    std::fs::rename(&tmp, &lf.path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_canonical_file() {
        let json = r#"{
            "ritual_findings": 1, "stage": "dual-review", "branch": "feat-x",
            "generated_at": "2026-07-11T00:00:00Z",
            "source_models": {"claude": "c", "codex": "x"},
            "findings": [
                {"id":1,"severity":"critical","title":"bug","file":"src/a.rs","line":10,
                 "plan_step":null,"scenario":"s","sources":["claude","codex"],
                 "verdict":"confirmed","action":"pending"}
            ]
        }"#;
        let f: FindingsFile = serde_json::from_str(json).unwrap();
        assert_eq!(f.findings.len(), 1);
        assert!(f.findings[0].cross_confirmed());
        assert_eq!(f.findings[0].severity, Severity::Critical);
        assert_eq!(f.findings[0].location(), "src/a.rs:10");
    }

    #[test]
    fn missing_fields_are_defaulted() {
        let f: FindingsFile = serde_json::from_str(r#"{"findings":[{"title":"x"}]}"#).unwrap();
        assert_eq!(f.findings[0].severity, Severity::Minor);
        assert!(!f.findings[0].cross_confirmed());
        assert_eq!(f.findings[0].location(), "");
    }

    #[test]
    fn unknown_severity_is_skipped_not_fatal() {
        // A whole-file parse error must not break load_all.
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("20260711T000000Z-a.json"),
            r#"{"findings":[{"severity":"apocalyptic"}]}"#,
        )
        .unwrap();
        std::fs::write(
            tmp.path().join("20260711T000001Z-b.json"),
            r#"{"findings":[{"title":"good","severity":"major"}]}"#,
        )
        .unwrap();
        std::fs::write(tmp.path().join("garbage.json"), "not json").unwrap();
        let loaded = load_all(tmp.path()).unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].file.findings[0].title, "good");
    }

    fn mk_file(sev_titles: &[(&str, &str)]) -> FindingsFile {
        let findings: Vec<String> = sev_titles
            .iter()
            .map(|(s, t)| format!(r#"{{"severity":"{s}","title":"{t}"}}"#))
            .collect();
        serde_json::from_str(&format!(r#"{{"findings":[{}]}}"#, findings.join(","))).unwrap()
    }

    #[test]
    fn aggregate_sorts_by_severity_and_carries_identity() {
        let loaded = vec![
            LoadedFindings {
                path: "a".into(),
                file: mk_file(&[("minor", "m"), ("critical", "c2")]),
            },
            LoadedFindings {
                path: "b".into(),
                file: mk_file(&[("critical", "c")]),
            },
        ];
        let agg = aggregate(&loaded, false);
        assert_eq!(agg[0].finding.title, "c2");
        assert_eq!((agg[0].file_idx, agg[0].pos), (0, 1)); // identity survives sort
        assert_eq!(agg[1].finding.title, "c");
        assert_eq!(agg[2].finding.title, "m");
    }

    #[test]
    fn aggregate_hides_resolved_unless_asked() {
        let mut file = mk_file(&[("critical", "open"), ("critical", "done")]);
        file.findings[1].action = "fixed".into();
        let loaded = vec![LoadedFindings {
            path: "a".into(),
            file,
        }];
        let agg = aggregate(&loaded, false);
        assert_eq!(agg.len(), 1);
        assert_eq!(agg[0].finding.title, "open");
        assert_eq!(aggregate(&loaded, true).len(), 2);
        assert_eq!(resolved_count(&loaded), 1);
    }

    #[test]
    fn set_action_toggles_and_roundtrips_unknown_fields() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("20260712T000000Z-dual-review.json");
        // Unknown fields at both levels must survive the rewrite.
        std::fs::write(
            &path,
            r#"{"stage":"dual-review","custom_top":"keep-me",
                "findings":[{"title":"bug","severity":"critical","verdict":"confirmed",
                             "custom_inner":42}]}"#,
        )
        .unwrap();
        let mut loaded = load_all(tmp.path()).unwrap();

        set_action(&mut loaded, 0, 0, "dismissed").unwrap();
        assert_eq!(loaded[0].file.findings[0].action, "dismissed");
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains(r#""dismissed""#));
        assert!(text.contains("keep-me"), "top-level extra dropped");
        assert!(text.contains("custom_inner"), "finding extra dropped");

        // Same action again -> toggles back to pending.
        set_action(&mut loaded, 0, 0, "dismissed").unwrap();
        assert_eq!(loaded[0].file.findings[0].action, "pending");
        assert!(std::fs::read_to_string(&path).unwrap().contains("pending"));

        // Out-of-range is an error, not a panic.
        assert!(set_action(&mut loaded, 0, 99, "fixed").is_err());
        assert!(set_action(&mut loaded, 9, 0, "fixed").is_err());
    }

    #[test]
    fn snippet_roundtrips_and_absence_stays_absent() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("20260712T000001Z-dual-review.json");
        std::fs::write(
            &path,
            r#"{"stage":"dual-review",
                "findings":[{"title":"with","snippet":"let x = 1;"},
                            {"title":"without"}]}"#,
        )
        .unwrap();
        let mut loaded = load_all(tmp.path()).unwrap();
        assert_eq!(
            loaded[0].file.findings[0].snippet.as_deref(),
            Some("let x = 1;")
        );
        assert_eq!(loaded[0].file.findings[1].snippet, None);

        // A rewrite keeps the snippet and does NOT invent one where absent
        // (skip_serializing_if — external emitters' files stay minimal).
        set_action(&mut loaded, 0, 0, "fixed").unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("let x = 1;"));
        assert_eq!(text.matches("snippet").count(), 1);
    }

    #[test]
    fn resolved_semantics() {
        let mut f = Finding::default();
        assert!(!f.resolved());
        f.action = "pending".into();
        assert!(!f.resolved());
        f.action = "fixed".into();
        assert!(f.resolved());
        f.action = "dismissed".into();
        assert!(f.resolved());
        f.action = "wontfix-someday".into(); // free text stays unresolved
        assert!(!f.resolved());
    }
}
