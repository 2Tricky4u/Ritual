use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

pub const PIPELINE: &[StageId] = &[
    StageId::Spec,
    StageId::Plan,
    StageId::PlanReview,
    StageId::TestsRed,
    StageId::Implement,
    StageId::DualReview,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StageId {
    Spec,
    Plan,
    PlanReview,
    TestsRed,
    Implement,
    DualReview,
}

impl StageId {
    pub fn label(&self) -> &'static str {
        match self {
            StageId::Spec => "spec",
            StageId::Plan => "plan",
            StageId::PlanReview => "plan-review",
            StageId::TestsRed => "tests-red",
            StageId::Implement => "implement",
            StageId::DualReview => "dual-review",
        }
    }

    #[allow(dead_code)] // used by `ritual run <stage>` (M2)
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "spec" => Some(StageId::Spec),
            "plan" => Some(StageId::Plan),
            "plan-review" | "plan_review" => Some(StageId::PlanReview),
            "tests-red" | "tests_red" | "tdd" => Some(StageId::TestsRed),
            "implement" => Some(StageId::Implement),
            "dual-review" | "dual_review" => Some(StageId::DualReview),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StageStatus {
    #[default]
    Pending,
    Running,
    Done,
    Failed,
    NeedsAttention,
    Skipped,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StageState {
    #[serde(default)]
    pub status: StageStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub runs: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Feature {
    pub branch: String,
    pub title: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[serde(default)]
    pub stages: BTreeMap<StageId, StageState>,
}

impl Feature {
    pub fn new(branch: &str, title: &str) -> Self {
        let now = Utc::now();
        let mut stages = BTreeMap::new();
        for id in PIPELINE {
            stages.insert(*id, StageState::default());
        }
        Self {
            branch: branch.to_string(),
            title: title.to_string(),
            created_at: now,
            updated_at: now,
            stages,
        }
    }

    pub fn stage(&self, id: StageId) -> StageState {
        self.stages.get(&id).cloned().unwrap_or_default()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct State {
    pub version: u32,
    #[serde(default)]
    pub features: BTreeMap<String, Feature>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            version: 1,
            features: BTreeMap::new(),
        }
    }
}

impl State {
    pub fn load(dirs: &RitualDirs) -> Result<Self> {
        let path = dirs.state_file();
        if !path.exists() {
            return Ok(State::default());
        }
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))
    }

    /// Atomic save: write tmp file in the same dir, then rename over.
    pub fn save(&self, dirs: &RitualDirs) -> Result<()> {
        let path = dirs.state_file();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = path.with_extension("json.tmp");
        let text = serde_json::to_string_pretty(self)?;
        std::fs::write(&tmp, text).with_context(|| format!("writing {}", tmp.display()))?;
        std::fs::rename(&tmp, &path).with_context(|| format!("renaming to {}", path.display()))?;
        Ok(())
    }

    pub fn feature_for_branch_mut(&mut self, branch: &str) -> &mut Feature {
        let slug = branch_slug(branch);
        self.features
            .entry(slug)
            .or_insert_with(|| Feature::new(branch, branch))
    }
}

/// All `.ritual/` paths for one project.
///
/// `project_root` is where `.ritual/` lives. In a git-worktree setup that is
/// always the MAIN repository root, so every worktree shares one state.
/// `work_root` is where commands (check.sh, agents) actually run: the
/// current checkout, which may be a worktree.
#[derive(Debug, Clone)]
pub struct RitualDirs {
    pub project_root: PathBuf,
    pub work_root: PathBuf,
}

impl RitualDirs {
    pub fn new(project_root: impl Into<PathBuf>) -> Self {
        let root = project_root.into();
        Self {
            work_root: root.clone(),
            project_root: root,
        }
    }

    /// Walk up from cwd to find an existing `.ritual/`; in a linked worktree,
    /// the MAIN repository root wins (shared state across worktrees), even
    /// when committed `.ritual` files (invariants.md, config.toml, specs)
    /// materialize a `.ritual/` inside the worktree checkout.
    pub fn discover(cwd: &Path) -> Self {
        let work_root = git_root(cwd).unwrap_or_else(|| cwd.to_path_buf());
        if let Some(main_root) = git_main_root(cwd)
            && main_root != work_root
            && main_root.join(".ritual").is_dir()
        {
            return Self {
                project_root: main_root,
                work_root,
            };
        }
        let mut dir = Some(cwd);
        while let Some(d) = dir {
            if d.join(".ritual").is_dir() {
                return Self {
                    project_root: d.to_path_buf(),
                    work_root,
                };
            }
            dir = d.parent();
        }
        Self {
            project_root: work_root.clone(),
            work_root,
        }
    }

    pub fn root(&self) -> PathBuf {
        self.project_root.join(".ritual")
    }
    pub fn state_file(&self) -> PathBuf {
        self.root().join("state.json")
    }
    pub fn findings_dir(&self) -> PathBuf {
        self.root().join("findings")
    }
    /// The project constitution: non-negotiable constraints reviewers enforce.
    pub fn invariants_file(&self) -> PathBuf {
        self.root().join("invariants.md")
    }
    /// Generated review memory (ritual lessons) the dual-review skill reads.
    pub fn lessons_file(&self) -> PathBuf {
        self.root().join("lessons.md")
    }
    pub fn runs_dir(&self) -> PathBuf {
        self.root().join("runs")
    }
    pub fn logs_dir(&self) -> PathBuf {
        self.root().join("logs")
    }
    pub fn feature_dir(&self, slug: &str) -> PathBuf {
        self.root().join("features").join(slug)
    }
    pub fn spec_file(&self, slug: &str) -> PathBuf {
        self.feature_dir(slug).join("spec.md")
    }
    #[allow(dead_code)] // used by stage commands (M2)
    pub fn plan_file(&self, slug: &str) -> PathBuf {
        self.feature_dir(slug).join("plan.md")
    }
    pub fn exists(&self) -> bool {
        self.root().is_dir()
    }
}

/// `feat/user-auth` -> `feat-user-auth`; anything not [a-zA-Z0-9._-] becomes '-'.
pub fn branch_slug(branch: &str) -> String {
    let mut slug: String = branch
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect();
    while slug.contains("--") {
        slug = slug.replace("--", "-");
    }
    let slug = slug.trim_matches('-').to_string();
    if slug.is_empty() {
        "detached".into()
    } else {
        slug
    }
}

/// The MAIN repository root, even when `dir` is inside a linked worktree
/// (`--git-common-dir` points at the main checkout's .git).
pub fn git_main_root(dir: &Path) -> Option<PathBuf> {
    let out = Command::new("git")
        .args(["rev-parse", "--git-common-dir"])
        .current_dir(dir)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let common = String::from_utf8(out.stdout).ok()?.trim().to_string();
    let common_path = if Path::new(&common).is_absolute() {
        PathBuf::from(common)
    } else {
        dir.join(common)
    };
    common_path.parent().map(|p| p.to_path_buf())
}

/// branch -> checkout dir for every worktree of this repo.
pub fn worktrees(dir: &Path) -> Vec<(String, PathBuf)> {
    let Some(out) = Command::new("git")
        .args(["worktree", "list", "--porcelain"])
        .current_dir(dir)
        .output()
        .ok()
        .filter(|o| o.status.success())
    else {
        return Vec::new();
    };
    let text = String::from_utf8_lossy(&out.stdout);
    let mut result = Vec::new();
    let mut current_path: Option<PathBuf> = None;
    for line in text.lines() {
        if let Some(p) = line.strip_prefix("worktree ") {
            current_path = Some(PathBuf::from(p));
        } else if let Some(b) = line.strip_prefix("branch refs/heads/")
            && let Some(p) = current_path.take()
        {
            result.push((b.to_string(), p));
        }
    }
    result
}

pub fn git_root(dir: &Path) -> Option<PathBuf> {
    let out = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(dir)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8(out.stdout).ok()?;
    Some(PathBuf::from(s.trim()))
}

pub fn current_branch(dir: &Path) -> Option<String> {
    let out = Command::new("git")
        .args(["branch", "--show-current"])
        .current_dir(dir)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8(out.stdout).ok()?.trim().to_string();
    if s.is_empty() { None } else { Some(s) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slug_edge_cases() {
        assert_eq!(branch_slug("feat/user-auth"), "feat-user-auth");
        assert_eq!(branch_slug("fix//weird///name"), "fix-weird-name");
        assert_eq!(branch_slug("héllo wörld"), "h-llo-w-rld");
        assert_eq!(branch_slug("///"), "detached");
        assert_eq!(branch_slug("v1.2.3_rc"), "v1.2.3_rc");
    }

    #[test]
    fn state_round_trip() {
        let tmp = tempfile::tempdir().unwrap();
        let dirs = RitualDirs::new(tmp.path());
        let mut state = State::default();
        let feature = state.feature_for_branch_mut("feat/x");
        feature.stages.insert(
            StageId::PlanReview,
            StageState {
                status: StageStatus::Running,
                started_at: Some(Utc::now()),
                finished_at: None,
                runs: vec!["20260711T000000Z-plan-review".into()],
            },
        );
        state.save(&dirs).unwrap();

        let loaded = State::load(&dirs).unwrap();
        assert_eq!(loaded.version, 1);
        let f = loaded.features.get("feat-x").unwrap();
        assert_eq!(f.branch, "feat/x");
        assert_eq!(f.stage(StageId::PlanReview).status, StageStatus::Running);
        assert_eq!(f.stage(StageId::PlanReview).runs.len(), 1);
        // Untouched stages default to pending.
        assert_eq!(f.stage(StageId::Spec).status, StageStatus::Pending);
    }

    #[test]
    fn load_missing_is_default() {
        let tmp = tempfile::tempdir().unwrap();
        let dirs = RitualDirs::new(tmp.path());
        let state = State::load(&dirs).unwrap();
        assert!(state.features.is_empty());
    }

    #[test]
    fn discover_walks_up() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join(".ritual")).unwrap();
        let nested = tmp.path().join("src/deep");
        std::fs::create_dir_all(&nested).unwrap();
        let dirs = RitualDirs::discover(&nested);
        assert_eq!(
            dirs.project_root.canonicalize().unwrap(),
            tmp.path().canonicalize().unwrap()
        );
    }

    #[test]
    fn load_corrupt_state_is_an_error_not_a_silent_reset() {
        let tmp = tempfile::tempdir().unwrap();
        let dirs = RitualDirs::new(tmp.path());
        std::fs::create_dir_all(dirs.root()).unwrap();
        std::fs::write(dirs.state_file(), "{ definitely not json").unwrap();
        // Silently defaulting would orphan every feature's pipeline state.
        assert!(State::load(&dirs).is_err());
    }

    proptest::proptest! {
        #[test]
        fn branch_slug_is_always_fs_safe_and_idempotent(branch in "\\PC{0,64}") {
            let slug = branch_slug(&branch);
            proptest::prop_assert!(!slug.is_empty());
            proptest::prop_assert!(
                slug.chars().all(|c| c.is_ascii_alphanumeric() || ".-_".contains(c)),
                "unsafe char in {slug:?}"
            );
            proptest::prop_assert!(!slug.starts_with('-') && !slug.ends_with('-'));
            proptest::prop_assert_eq!(branch_slug(&slug), slug);
        }
    }

    #[test]
    fn branch_slug_degenerate_inputs() {
        assert_eq!(branch_slug(""), "detached");
        assert_eq!(branch_slug("---"), "detached");
        // Leading/trailing separators trim; inner dots survive.
        assert_eq!(branch_slug("/feat/x/"), "feat-x");
        assert_eq!(branch_slug("release/v0.5.0"), "release-v0.5.0");
    }
}
