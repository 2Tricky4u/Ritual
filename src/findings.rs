use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::{Deserialize, Serialize};

/// One findings file, written by the plan-review / dual-review skills.
/// Every field is defaulted: a missing field must never break the browser.
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
    #[serde(default)]
    pub scenario: String,
    #[serde(default)]
    pub sources: Vec<String>,
    #[serde(default)]
    pub verdict: String,
    #[serde(default)]
    pub action: String,
}

impl Finding {
    /// Both models flagged it -> strongest signal.
    pub fn cross_confirmed(&self) -> bool {
        self.sources.len() >= 2
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

/// Flatten + sort: severity first (critical on top), then newest file first.
pub fn aggregate(loaded: &[LoadedFindings]) -> Vec<(usize, Finding)> {
    let mut all: Vec<(usize, Finding)> = loaded
        .iter()
        .enumerate()
        .flat_map(|(i, lf)| lf.file.findings.iter().cloned().map(move |f| (i, f)))
        .collect();
    all.sort_by_key(|(i, f)| (f.severity, *i));
    all
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

    #[test]
    fn aggregate_sorts_by_severity() {
        let mk = |sev: &str, title: &str| {
            serde_json::from_str::<FindingsFile>(&format!(
                r#"{{"findings":[{{"severity":"{sev}","title":"{title}"}}]}}"#
            ))
            .unwrap()
        };
        let loaded = vec![
            LoadedFindings {
                path: "a".into(),
                file: mk("minor", "m"),
            },
            LoadedFindings {
                path: "b".into(),
                file: mk("critical", "c"),
            },
        ];
        let agg = aggregate(&loaded);
        assert_eq!(agg[0].1.title, "c");
        assert_eq!(agg[1].1.title, "m");
    }
}
