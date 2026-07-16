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
    /// 1-3 verbatim source lines at the finding: hunk-anchored evidence
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
    /// Triage answer for a still-open finding: "auto" (queued for the claude
    /// batch fix) or "manual" (the user will fix it). Never blocks CI -
    /// answered findings stay unresolved until actually fixed/dismissed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answer: Option<String>,
    /// Free-text context: why a finding was dismissed, or why the batch fix
    /// declined it. Feeds `ritual lessons`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

impl Finding {
    /// Both models flagged it -> strongest signal.
    pub fn cross_confirmed(&self) -> bool {
        self.sources.len() >= 2
    }

    /// A human closed this finding out ("fixed" from the TUI or a skill,
    /// "dismissed" from the TUI). Anything else ("pending", "", free text)
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
/// The resolved filter lives HERE, the single chokepoint every consumer
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

/// Any OPEN (unresolved) confirmed finding from a NON-coverage stage. Coverage
/// gaps are excluded because they are tracked authoritatively by the coverage
/// STAGE status (and each coverage run supersedes the last); this predicate is
/// about lingering code/plan defects. Broader than the critical-only CI gate: a
/// project is not "complete" while any such finding stands.
pub fn has_open_confirmed(loaded: &[LoadedFindings], slug: &str) -> bool {
    loaded.iter().any(|lf| {
        // Scope to THIS feature: a file whose branch resolves to another slug is
        // a different feature's concern. Branch-LESS files stay in scope (a read
        // gate must not silently drop legitimately branch-less findings).
        (lf.file.branch.is_empty() || crate::state::branch_slug(&lf.file.branch) == slug)
            && lf.file.stage != "coverage"
            && lf
                .file
                .findings
                .iter()
                .any(|f| !f.resolved() && verdict_confirmed(&f.verdict))
    })
}

/// Stamp `branch` onto the findings files a run just produced, so completeness
/// consumers can scope by branch reliably - the skill fills `branch` "or empty",
/// which we cannot trust. Best-effort: a file that vanished or won't parse is
/// skipped, never fatal (parse failures are already invisible to `load_all`).
pub fn stamp_branch(findings_dir: &Path, filenames: &[String], branch: &str) {
    for name in filenames {
        let path = findings_dir.join(name);
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(mut file) = serde_json::from_str::<FindingsFile>(&text) else {
            continue;
        };
        if file.branch == branch {
            continue;
        }
        file.branch = branch.to_string();
        let _ = rewrite(&LoadedFindings { path, file });
    }
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
    set_action_with_reason(loaded, file_idx, pos, action, None)
}

/// `set_action` plus a reason recorded in the SAME atomic write (dismissal
/// rationale for `ritual lessons`). Toggling back to "pending" clears any
/// stored reason.
pub fn set_action_with_reason(
    loaded: &mut [LoadedFindings],
    file_idx: usize,
    pos: usize,
    action: &str,
    reason: Option<&str>,
) -> Result<()> {
    let lf = loaded
        .get_mut(file_idx)
        .ok_or_else(|| anyhow::anyhow!("no findings file at index {file_idx}"))?;
    let finding = lf
        .file
        .findings
        .get_mut(pos)
        .ok_or_else(|| anyhow::anyhow!("no finding at position {pos}"))?;
    if finding.action == action {
        finding.action = "pending".to_string(); // toggle off
        finding.reason = None;
    } else {
        // Resolving over a prose action (plan-review writes its debate
        // resolutions THERE) must not destroy the record: with no explicit
        // reason given, the prose migrates into `reason`. An explicit
        // reason always wins.
        let inherited = (is_prose_action(&finding.action)
            && reason.is_none()
            && matches!(action, "fixed" | "dismissed"))
        .then(|| finding.action.clone());
        finding.action = action.to_string();
        finding.reason = reason.map(str::to_string).or(inherited);
    }
    rewrite(lf)
}

/// True when an `action` value is free text (a recorded resolution) rather
/// than one of the lifecycle states. Plan-review's debate writes prose here.
pub fn is_prose_action(action: &str) -> bool {
    !matches!(action, "" | "pending" | "fixed" | "dismissed")
}

/// What one-touch triage would do with a finding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Recommendation {
    /// Queue for the claude batch fix (⚑A): confirmed plan finding.
    QueueAuto,
    /// Queue for the human (⚑M): confirmed code finding.
    QueueManual,
    /// Mark fixed, prose resolution preserved as reason: the review already
    /// applied this fix to the plan and recorded what it did in `action`.
    Archive,
    /// Dismiss with the given reason: the review itself retracted it.
    Dismiss(String),
    /// No safe default - the human decides (shown, never auto-applied).
    NeedsYou,
}

/// The review skills speak different verdict dialects for the same judgment:
/// dual-review emits `confirmed|unconfirmed|refuted`, plan-review emits
/// `accepted|rebutted|unresolved`. Every consumer must treat the synonyms
/// alike or plan findings silently fall out of triage/gates.
pub fn verdict_confirmed(verdict: &str) -> bool {
    matches!(
        verdict.to_ascii_lowercase().as_str(),
        "confirmed" | "accepted"
    )
}

/// The review itself retracted the finding (all dialects).
pub fn verdict_retracted(verdict: &str) -> bool {
    matches!(
        verdict.to_ascii_lowercase().as_str(),
        "withdrawn" | "refuted" | "rebutted"
    )
}

/// The deterministic recommended disposition for a finding, or None when it
/// is already handled (resolved, triaged, or declined by a batch run - a
/// declined finding must NOT be auto-requeued, that would loop).
pub fn recommend(f: &Finding) -> Option<Recommendation> {
    if f.resolved() || f.answer.is_some() {
        return None;
    }
    if f.reason.is_some() {
        return None; // declined by a batch run: explicitly the human's call
    }
    if is_prose_action(&f.action) {
        return Some(Recommendation::Archive);
    }
    if verdict_retracted(&f.verdict) {
        return Some(Recommendation::Dismiss(format!(
            "{} by review",
            f.verdict.to_ascii_lowercase()
        )));
    }
    if verdict_confirmed(&f.verdict) {
        if f.file.is_none() && f.plan_step.is_some() {
            return Some(Recommendation::QueueAuto);
        }
        if f.file.is_some() {
            return Some(Recommendation::QueueManual);
        }
    }
    Some(Recommendation::NeedsYou)
}

/// Set (or clear) a finding's triage answer and reason: plain assignment,
/// no toggle - toggle policy lives with the caller. Same atomic rewrite.
pub fn set_answer(
    loaded: &mut [LoadedFindings],
    file_idx: usize,
    pos: usize,
    answer: Option<&str>,
    reason: Option<&str>,
) -> Result<()> {
    let lf = loaded
        .get_mut(file_idx)
        .ok_or_else(|| anyhow::anyhow!("no findings file at index {file_idx}"))?;
    let finding = lf
        .file
        .findings
        .get_mut(pos)
        .ok_or_else(|| anyhow::anyhow!("no finding at position {pos}"))?;
    finding.answer = answer.map(str::to_string);
    finding.reason = reason.map(str::to_string);
    rewrite(lf)
}

/// Persist a findings file: pretty JSON, tmp + rename (atomic on POSIX). If the
/// file was deleted underneath us (e.g. a concurrent `reset-plan` wiped the
/// plan/coverage findings while the TUI held a stale copy), keep the in-memory
/// change but do NOT resurrect the file on disk.
fn rewrite(lf: &LoadedFindings) -> Result<()> {
    if !lf.path.exists() {
        return Ok(());
    }
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
    fn has_open_confirmed_scopes_by_branch() {
        let mk = |branch: &str| {
            LoadedFindings {
            path: format!("/x/{branch}.json").into(),
            file: serde_json::from_str(&format!(
                r#"{{"stage":"dual-review","branch":"{branch}","findings":[{{"title":"b","verdict":"confirmed","action":"pending"}}]}}"#
            ))
            .unwrap(),
        }
        };
        // Another feature's open confirmed finding must NOT block this one.
        assert!(!has_open_confirmed(
            std::slice::from_ref(&mk("feat-b")),
            "feat-a"
        ));
        // This feature's own does; a branch-less one stays in scope (lenient).
        assert!(has_open_confirmed(
            std::slice::from_ref(&mk("feat-a")),
            "feat-a"
        ));
        let branchless = LoadedFindings {
            path: "/x/none.json".into(),
            file: serde_json::from_str(
                r#"{"stage":"dual-review","findings":[{"title":"b","verdict":"confirmed","action":"pending"}]}"#,
            )
            .unwrap(),
        };
        assert!(has_open_confirmed(
            std::slice::from_ref(&branchless),
            "feat-a"
        ));
    }

    #[test]
    fn stamp_branch_sets_branch_and_preserves_unknown_fields() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        std::fs::write(
            dir.join("20260101T000000Z-coverage.json"),
            r#"{"stage":"coverage","satisfied":["D1"],"findings":[]}"#,
        )
        .unwrap();
        stamp_branch(dir, &["20260101T000000Z-coverage.json".into()], "feat-x");
        let loaded = load_all(dir).unwrap();
        assert_eq!(loaded[0].file.branch, "feat-x");
        // The unknown `satisfied` array round-trips through the stamp rewrite.
        let n = loaded[0]
            .file
            .extra
            .get("satisfied")
            .and_then(|v| v.as_array())
            .map(|a| a.len());
        assert_eq!(n, Some(1), "unknown fields survive the stamp");
    }

    #[test]
    fn set_action_on_a_deleted_file_does_not_resurrect_it() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("20260712T000000Z-coverage.json");
        std::fs::write(
            &path,
            r#"{"stage":"coverage","findings":[{"title":"x","verdict":"confirmed"}]}"#,
        )
        .unwrap();
        let mut loaded = load_all(tmp.path()).unwrap();
        // A concurrent reset-plan deletes the file underneath the stale copy.
        std::fs::remove_file(&path).unwrap();
        set_action(&mut loaded, 0, 0, "fixed").unwrap();
        assert!(!path.exists(), "deleted file is not resurrected");
        assert_eq!(
            loaded[0].file.findings[0].action, "fixed",
            "in-memory change is still applied"
        );
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
        // (skip_serializing_if: external emitters' files stay minimal).
        set_action(&mut loaded, 0, 0, "fixed").unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("let x = 1;"));
        assert_eq!(text.matches("snippet").count(), 1);
    }

    #[test]
    fn answer_reason_roundtrip_and_absence_stays_absent() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("20260713T000001Z-plan-review.json");
        std::fs::write(
            &path,
            r#"{"stage":"plan-review",
                "findings":[{"title":"queued","answer":"auto","reason":"why"},
                            {"title":"bare"}]}"#,
        )
        .unwrap();
        let mut loaded = load_all(tmp.path()).unwrap();
        assert_eq!(loaded[0].file.findings[0].answer.as_deref(), Some("auto"));
        assert_eq!(loaded[0].file.findings[0].reason.as_deref(), Some("why"));
        assert_eq!(loaded[0].file.findings[1].answer, None);

        // Rewrite keeps present values and does NOT invent nulls where absent.
        set_action(&mut loaded, 0, 1, "fixed").unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(text.matches("answer").count(), 1, "{text}");
        assert_eq!(text.matches("reason").count(), 1, "{text}");
    }

    #[test]
    fn set_answer_writes_and_clears_plainly() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("20260713T000002Z-plan-review.json");
        std::fs::write(
            &path,
            r#"{"stage":"plan-review","findings":[{"title":"t","plan_step":"Step 1"}]}"#,
        )
        .unwrap();
        let mut loaded = load_all(tmp.path()).unwrap();
        // Queue for the claude batch.
        set_answer(&mut loaded, 0, 0, Some("auto"), None).unwrap();
        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .contains(r#""answer": "auto""#)
        );
        // No toggle: same value sticks.
        set_answer(&mut loaded, 0, 0, Some("auto"), None).unwrap();
        assert_eq!(loaded[0].file.findings[0].answer.as_deref(), Some("auto"));
        // Batch declined: answer cleared, reason recorded.
        set_answer(&mut loaded, 0, 0, None, Some("needs spec change")).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains("answer"), "{text}");
        assert!(text.contains("needs spec change"));
        // An answered finding is still unresolved (never blocks-as-resolved).
        assert!(!loaded[0].file.findings[0].resolved());
        // Out-of-range stays an error.
        assert!(set_answer(&mut loaded, 0, 9, None, None).is_err());
    }

    #[test]
    fn prose_action_migrates_to_reason_on_resolve() {
        // Today's incident, as a regression test: 31 plan-review findings
        // carried their debate resolutions IN `action`; pressing f replaced
        // them all with the word "fixed".
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("20260713T000009Z-plan-review.json");
        std::fs::write(
            &path,
            r#"{"stage":"plan-review","findings":[
                {"title":"t","plan_step":"Step 5",
                 "action":"Resolved by reordering: dump through the running DB, then snapshot."}]}"#,
        )
        .unwrap();
        let mut loaded = load_all(tmp.path()).unwrap();
        set_action(&mut loaded, 0, 0, "fixed").unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains(r#""action": "fixed""#));
        assert!(
            text.contains("Resolved by reordering"),
            "prose lost: {text}"
        );
        assert_eq!(
            loaded[0].file.findings[0].reason.as_deref(),
            Some("Resolved by reordering: dump through the running DB, then snapshot.")
        );

        // An explicit reason beats the inherited prose.
        set_action(&mut loaded, 0, 0, "fixed").unwrap(); // toggle back (clears)
        loaded[0].file.findings[0].action = "Another prose resolution left by a review.".into();
        set_action_with_reason(&mut loaded, 0, 0, "dismissed", Some("typed reason")).unwrap();
        assert_eq!(
            loaded[0].file.findings[0].reason.as_deref(),
            Some("typed reason")
        );

        // Plain lifecycle transitions never invent a reason.
        set_action_with_reason(&mut loaded, 0, 0, "dismissed", None).unwrap(); // toggle -> pending
        set_action(&mut loaded, 0, 0, "fixed").unwrap();
        assert!(!std::fs::read_to_string(&path).unwrap().contains("reason"));
    }

    #[test]
    fn recommend_matrix() {
        let mk = |json: &str| -> Finding { serde_json::from_str(json).unwrap() };
        use Recommendation::*;
        // Confirmed plan finding -> claude queue.
        assert_eq!(
            recommend(&mk(
                r#"{"title":"t","plan_step":"Step 2","verdict":"confirmed"}"#
            )),
            Some(QueueAuto)
        );
        // Case-insensitive verdicts.
        assert_eq!(
            recommend(&mk(
                r#"{"title":"t","plan_step":"Step 2","verdict":"Confirmed"}"#
            )),
            Some(QueueAuto)
        );
        // Confirmed code finding -> manual queue.
        assert_eq!(
            recommend(&mk(
                r#"{"title":"t","file":"src/a.rs","line":3,"verdict":"confirmed"}"#
            )),
            Some(QueueManual)
        );
        // Prose action beats confirmed: it's already a recorded resolution.
        assert_eq!(
            recommend(&mk(
                r#"{"title":"t","plan_step":"Step 2","verdict":"confirmed",
                    "action":"Resolved by narrowing the scope."}"#
            )),
            Some(Archive)
        );
        // Withdrawn/refuted -> dismiss with reason.
        assert!(matches!(
            recommend(&mk(r#"{"title":"t","verdict":"Withdrawn"}"#)),
            Some(Dismiss(_))
        ));
        // plan-review speaks accepted/rebutted/unresolved: same dispositions.
        assert_eq!(
            recommend(&mk(
                r#"{"title":"t","plan_step":"D3","verdict":"accepted"}"#
            )),
            Some(QueueAuto)
        );
        assert_eq!(
            recommend(&mk(r#"{"title":"t","verdict":"rebutted"}"#)),
            Some(Dismiss("rebutted by review".into()))
        );
        assert_eq!(
            recommend(&mk(r#"{"title":"t","verdict":"unresolved"}"#)),
            Some(NeedsYou)
        );
        // Unconfirmed / no location -> the human decides.
        assert_eq!(
            recommend(&mk(r#"{"title":"t","verdict":"unconfirmed"}"#)),
            Some(NeedsYou)
        );
        assert_eq!(
            recommend(&mk(r#"{"title":"t","verdict":"confirmed"}"#)),
            Some(NeedsYou),
            "confirmed with no location has no safe default"
        );
        // Already handled: resolved, triaged, or batch-declined.
        assert_eq!(
            recommend(&mk(
                r#"{"title":"t","verdict":"confirmed","action":"fixed"}"#
            )),
            None
        );
        assert_eq!(
            recommend(&mk(
                r#"{"title":"t","plan_step":"s","verdict":"confirmed","answer":"auto"}"#
            )),
            None
        );
        // Declined by a batch run must NOT re-queue (no decline loop).
        assert_eq!(
            recommend(&mk(r#"{"title":"t","plan_step":"s","verdict":"confirmed",
                    "reason":"needs a spec change"}"#)),
            None
        );
    }

    #[test]
    fn is_prose_action_classifier() {
        assert!(is_prose_action("Resolved by narrowing the scope."));
        assert!(!is_prose_action(""));
        assert!(!is_prose_action("pending"));
        assert!(!is_prose_action("fixed"));
        assert!(!is_prose_action("dismissed"));
    }

    #[test]
    fn dismiss_with_reason_persists_and_toggle_clears() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("20260713T000003Z-plan-review.json");
        std::fs::write(
            &path,
            r#"{"stage":"plan-review","findings":[{"title":"noise"}]}"#,
        )
        .unwrap();
        let mut loaded = load_all(tmp.path()).unwrap();
        set_action_with_reason(&mut loaded, 0, 0, "dismissed", Some("known limitation")).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains(r#""action": "dismissed""#));
        assert!(text.contains("known limitation"));
        assert!(loaded[0].file.findings[0].resolved());
        // Toggling back to pending clears the reason too.
        set_action_with_reason(&mut loaded, 0, 0, "dismissed", None).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains(r#""action": "pending""#));
        assert!(!text.contains("known limitation"), "{text}");
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
