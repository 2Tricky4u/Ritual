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
    StageId::Coverage,
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
    Coverage,
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
            StageId::Coverage => "coverage",
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
            "coverage" => Some(StageId::Coverage),
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
    /// The claude session id this stage owns. Set for interactive stages that
    /// ritual pins with `--session-id` (tests-red) so a later stage can
    /// `--resume` the exact conversation instead of "most recent in cwd".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// Tree fingerprint (HEAD sha + dirty digest) captured when the stage
    /// reached a terminal status. Guidance compares it against the current
    /// tree to flag Done-but-stale review stages. None = unknown (legacy
    /// state, non-git dirs, reconciled runs) - NEVER reported stale.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fingerprint: Option<String>,
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

    /// Atomic save via a writer-unique tmp + rename: the TUI and a CLI
    /// command may save concurrently, and a shared tmp name would let their
    /// writes interleave into corrupt bytes.
    pub fn save(&self, dirs: &RitualDirs) -> Result<()> {
        let text = serde_json::to_string_pretty(self)?;
        crate::fsx::atomic_write(&dirs.state_file(), text.as_bytes())
    }

    pub fn feature_for_branch_mut(&mut self, branch: &str) -> &mut Feature {
        let slug = branch_slug(branch);
        self.features
            .entry(slug)
            .or_insert_with(|| Feature::new(branch, branch))
    }

    /// The claude session id pinned to `stage` of the feature keyed by `slug`.
    pub fn stage_session_id(&self, slug: &str, stage: StageId) -> Option<String> {
        self.features
            .get(slug)
            .and_then(|f| f.stages.get(&stage))
            .and_then(|s| s.session_id.clone())
    }

    /// Pin (or clear) `stage`'s claude session id; creates the stage entry if
    /// absent. Persist with `save` afterwards.
    pub fn set_stage_session_id(&mut self, slug: &str, stage: StageId, id: Option<String>) {
        if let Some(feature) = self.features.get_mut(slug) {
            feature.stages.entry(stage).or_default().session_id = id;
        }
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
        // Both git helpers canonicalize internally, so the worktree check
        // below compares canonical against canonical; the non-git fallback
        // canonicalizes here for the same reason.
        let work_root = git_root(cwd)
            .unwrap_or_else(|| cwd.canonicalize().unwrap_or_else(|_| cwd.to_path_buf()));
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
                    project_root: d.canonicalize().unwrap_or_else(|_| d.to_path_buf()),
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
    /// The generated architecture map that grounds planning (committable).
    pub fn architecture_file(&self) -> PathBuf {
        self.root().join("architecture.md")
    }
    /// Where the architect agent writes; ritual validates + installs it.
    pub fn architecture_candidate_file(&self) -> PathBuf {
        self.root().join("architecture.md.new")
    }
    /// The scoped tree fingerprint the map was generated at (gitignored).
    pub fn architecture_fingerprint_file(&self) -> PathBuf {
        self.root().join("architecture.fingerprint")
    }
    pub fn runs_dir(&self) -> PathBuf {
        self.root().join("runs")
    }
    /// The user-editable lane definitions for `ritual audit`.
    pub fn audit_lanes_file(&self) -> PathBuf {
        self.root().join("audit-lanes.md")
    }
    /// Per-audit lane reports live under here, one subdir per run timestamp.
    pub fn audit_dir(&self) -> PathBuf {
        self.root().join("audit")
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

/// Local branches whose names collapse to the SAME slug (`feat/x` and
/// `feat-x` both slug to `feat-x`): they would silently share one state
/// entry, plan, and findings scope. Grouped for doctor/TUI warnings; empty
/// outside a git repo. Sorted for stable output.
pub fn slug_collisions(dir: &Path) -> Vec<(String, Vec<String>)> {
    let Some(out) = Command::new("git")
        .args(["for-each-ref", "refs/heads", "--format=%(refname:short)"])
        .current_dir(dir)
        .output()
        .ok()
        .filter(|o| o.status.success())
    else {
        return Vec::new();
    };
    let mut groups: std::collections::BTreeMap<String, Vec<String>> = Default::default();
    for branch in String::from_utf8_lossy(&out.stdout).lines() {
        let branch = branch.trim();
        if !branch.is_empty() {
            groups
                .entry(branch_slug(branch))
                .or_default()
                .push(branch.to_string());
        }
    }
    groups.retain(|_, v| v.len() >= 2);
    groups
        .into_iter()
        .map(|(slug, mut branches)| {
            branches.sort();
            (slug, branches)
        })
        .collect()
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
    // From a repo SUBDIRECTORY git returns a RELATIVE common dir ("../.git"):
    // joining leaves a `subdir/..` in the root, and every derived path
    // (plan/spec files) inherits it - which broke the doc-edit tool-lock
    // rules, since the Edit tool reports NORMALIZED paths that never match
    // an unnormalized rule. Canonicalize at the source.
    let common_path = if Path::new(&common).is_absolute() {
        PathBuf::from(common)
    } else {
        dir.join(common)
    };
    common_path
        .parent()
        .map(|p| p.canonicalize().unwrap_or_else(|_| p.to_path_buf()))
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
    // Canonicalize so symlinked cwds compare equal to git_main_root's
    // canonicalized output (discover's worktree check relies on it).
    let p = PathBuf::from(s.trim());
    Some(p.canonicalize().unwrap_or(p))
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
                ..Default::default()
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
    fn stage_session_id_round_trips_and_is_absent_by_default() {
        let tmp = tempfile::tempdir().unwrap();
        let dirs = RitualDirs::new(tmp.path());
        let mut state = State::default();
        state.feature_for_branch_mut("feat/x");
        let slug = branch_slug("feat/x");

        // Absent by default; helper returns None.
        assert_eq!(state.stage_session_id(&slug, StageId::TestsRed), None);

        state.set_stage_session_id(
            &slug,
            StageId::TestsRed,
            Some("11111111-1111-4111-8111-111111111111".into()),
        );
        state.save(&dirs).unwrap();

        let loaded = State::load(&dirs).unwrap();
        assert_eq!(
            loaded.stage_session_id(&slug, StageId::TestsRed).as_deref(),
            Some("11111111-1111-4111-8111-111111111111")
        );
        // Stages without a pinned session omit the key entirely on disk.
        let raw = std::fs::read_to_string(dirs.state_file()).unwrap();
        assert_eq!(
            raw.matches("session_id").count(),
            1,
            "only tests-red carries one"
        );
    }

    #[test]
    fn legacy_state_without_session_id_loads() {
        let tmp = tempfile::tempdir().unwrap();
        let dirs = RitualDirs::new(tmp.path());
        std::fs::create_dir_all(tmp.path().join(".ritual")).unwrap();
        // A state.json written before session_id existed.
        std::fs::write(
            dirs.state_file(),
            r#"{"version":1,"features":{"feat-x":{"branch":"feat/x","title":"x",
                "created_at":"2026-07-11T00:00:00Z","updated_at":"2026-07-11T00:00:00Z",
                "stages":{"tests_red":{"status":"done"}}}}}"#,
        )
        .unwrap();
        let loaded = State::load(&dirs).unwrap();
        assert_eq!(loaded.stage_session_id("feat-x", StageId::TestsRed), None);
        assert_eq!(
            loaded.features["feat-x"].stage(StageId::TestsRed).status,
            StageStatus::Done
        );
        // Same contract for fields added later: legacy = None, never stale.
        assert_eq!(
            loaded.features["feat-x"]
                .stage(StageId::TestsRed)
                .fingerprint,
            None
        );
    }

    #[test]
    fn stage_fingerprint_round_trips_and_stays_optional() {
        let tmp = tempfile::tempdir().unwrap();
        let dirs = RitualDirs::new(tmp.path());
        std::fs::create_dir_all(tmp.path().join(".ritual")).unwrap();
        let mut st = State::default();
        let f = st.feature_for_branch_mut("main");
        f.stages.insert(
            StageId::DualReview,
            StageState {
                status: StageStatus::Done,
                fingerprint: Some("abc123:deadbeef".into()),
                ..Default::default()
            },
        );
        st.save(&dirs).unwrap();
        let loaded = State::load(&dirs).unwrap();
        assert_eq!(
            loaded.features["main"]
                .stage(StageId::DualReview)
                .fingerprint
                .as_deref(),
            Some("abc123:deadbeef")
        );
        // Absent fingerprints are skipped on disk (old binaries stay happy).
        let text = std::fs::read_to_string(dirs.state_file()).unwrap();
        assert_eq!(text.matches("fingerprint").count(), 1, "{text}");
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

    fn git(dir: &Path, args: &[&str]) {
        let out = Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {out:?}");
    }

    fn no_parent_dirs(p: &Path) -> bool {
        !p.components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
    }

    #[test]
    fn slug_collisions_groups_colliding_branches_only() {
        let tmp = tempfile::tempdir().unwrap();
        git(tmp.path(), &["init", "-q", "-b", "main"]);
        git(tmp.path(), &["config", "user.email", "t@t"]);
        git(tmp.path(), &["config", "user.name", "t"]);
        std::fs::write(tmp.path().join("a.txt"), "a").unwrap();
        git(tmp.path(), &["add", "a.txt"]);
        git(tmp.path(), &["commit", "-qm", "x"]);
        git(tmp.path(), &["branch", "feat/x"]);
        git(tmp.path(), &["branch", "feat-x"]);
        git(tmp.path(), &["branch", "lonely"]);
        let groups = slug_collisions(tmp.path());
        assert_eq!(groups.len(), 1, "{groups:?}");
        assert_eq!(groups[0].0, "feat-x");
        assert_eq!(groups[0].1, vec!["feat-x".to_string(), "feat/x".into()]);
        // Outside a git repo: empty, never an error.
        let plain = tempfile::tempdir().unwrap();
        assert!(slug_collisions(plain.path()).is_empty());
    }

    #[test]
    fn git_main_root_from_subdir_is_normalized() {
        // From a repo subdir, `--git-common-dir` is RELATIVE ("../.git");
        // the naive join left `sub/..` in the root, which poisoned the
        // doc-edit tool-lock rules (rules never matched normalized paths).
        let tmp = tempfile::tempdir().unwrap();
        git(tmp.path(), &["init", "-q", "-b", "main"]);
        let deep = tmp.path().join("sub/deep");
        std::fs::create_dir_all(&deep).unwrap();
        let root = git_main_root(&deep).expect("in a repo");
        assert!(no_parent_dirs(&root), "unnormalized root: {root:?}");
        assert_eq!(root, tmp.path().canonicalize().unwrap());
    }

    #[test]
    fn discover_from_repo_subdir_yields_normalized_roots() {
        let tmp = tempfile::tempdir().unwrap();
        git(tmp.path(), &["init", "-q", "-b", "main"]);
        std::fs::create_dir_all(tmp.path().join(".ritual")).unwrap();
        let deep = tmp.path().join("sub/deep");
        std::fs::create_dir_all(&deep).unwrap();
        let dirs = RitualDirs::discover(&deep);
        assert!(
            no_parent_dirs(&dirs.project_root),
            "{:?}",
            dirs.project_root
        );
        assert!(no_parent_dirs(&dirs.work_root), "{:?}", dirs.work_root);
        // The exact path class that broke the Edit(//…) permission rule.
        assert!(no_parent_dirs(&dirs.plan_file("main")));
    }

    #[test]
    fn discover_prefers_main_root_across_worktree() {
        let tmp = tempfile::tempdir().unwrap();
        let main = tmp.path().join("main");
        std::fs::create_dir_all(&main).unwrap();
        git(&main, &["init", "-q", "-b", "main"]);
        std::fs::create_dir_all(main.join(".ritual")).unwrap();
        std::fs::write(main.join("a.txt"), "a").unwrap();
        git(&main, &["add", "."]);
        git(&main, &["commit", "-qm", "init"]);
        let wt = tmp.path().join("wt");
        git(
            &main,
            &["worktree", "add", "-q", wt.to_str().unwrap(), "-b", "feat"],
        );
        let dirs = RitualDirs::discover(&wt);
        assert_eq!(dirs.project_root, main.canonicalize().unwrap());
        assert_eq!(dirs.work_root, wt.canonicalize().unwrap());
        assert!(no_parent_dirs(&dirs.project_root));
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
