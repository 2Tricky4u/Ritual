use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use anyhow::{Context, Result};

use crate::state::RitualDirs;

const CHECK_RUST: &str = include_str!("../templates/check-rust.sh");
const CHECK_PYTHON: &str = include_str!("../templates/check-python.sh");
const CHECK_NODE: &str = include_str!("../templates/check-node.sh");
const CHECK_MIXED: &str = include_str!("../templates/check-mixed.sh");
pub const SPEC_TEMPLATE: &str = include_str!("../templates/spec-template.md");
pub const INVARIANTS_TEMPLATE: &str = include_str!("../templates/invariants-template.md");
const CLAUDE_SNIPPET: &str = include_str!("../templates/claude-snippet.md");

const GITIGNORE_ENTRIES: &[&str] = &[
    ".ritual/runs/",
    ".ritual/logs/",
    ".ritual/ci/",
    ".ritual/state.json",
    ".ritual/features/*/.*.undo",
    ".ritual/features/*/.undo/",
    ".ritual/features/*/.redo/",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stack {
    Rust,
    Python,
    Node,
    Mixed,
    Unknown,
}

impl Stack {
    pub fn label(&self) -> &'static str {
        match self {
            Stack::Rust => "rust",
            Stack::Python => "python",
            Stack::Node => "node",
            Stack::Mixed => "mixed",
            Stack::Unknown => "unknown",
        }
    }

    fn template(&self) -> &'static str {
        match self {
            Stack::Rust => CHECK_RUST,
            Stack::Python => CHECK_PYTHON,
            Stack::Node => CHECK_NODE,
            // Unknown gets the mixed dispatcher: harmless no-op until
            // manifests appear.
            Stack::Mixed | Stack::Unknown => CHECK_MIXED,
        }
    }
}

pub fn detect_stack(dir: &Path) -> Stack {
    let rust = dir.join("Cargo.toml").exists();
    let python = dir.join("pyproject.toml").exists() || dir.join("setup.py").exists();
    let node = dir.join("package.json").exists();
    match (rust, python, node) {
        (true, false, false) => Stack::Rust,
        (false, true, false) => Stack::Python,
        (false, false, true) => Stack::Node,
        (false, false, false) => Stack::Unknown,
        _ => Stack::Mixed,
    }
}

/// What `ritual init` did, for rendering.
#[derive(Debug, Default)]
pub struct InitReport {
    pub stack: Option<Stack>,
    pub actions: Vec<String>,
    pub skipped: Vec<String>,
}

pub fn init(project_root: &Path, force: bool) -> Result<InitReport> {
    let mut report = InitReport::default();
    let dirs = RitualDirs::new(project_root);

    // .ritual skeleton
    for dir in [
        dirs.root(),
        dirs.findings_dir(),
        dirs.runs_dir(),
        dirs.logs_dir(),
        dirs.root().join("features"),
    ] {
        if !dir.exists() {
            std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
            report
                .actions
                .push(format!("created {}", rel(project_root, &dir)));
        }
    }

    // state.json
    if !dirs.state_file().exists() {
        crate::state::State::default().save(&dirs)?;
        report.actions.push("created .ritual/state.json".into());
    }

    // invariants.md: only create, never touch; it's the user's constitution.
    if !dirs.invariants_file().exists() {
        std::fs::write(dirs.invariants_file(), INVARIANTS_TEMPLATE)?;
        report
            .actions
            .push("created .ritual/invariants.md (project constitution)".into());
    }

    // check.sh
    let stack = detect_stack(project_root);
    report.stack = Some(stack);
    let check = project_root.join("check.sh");
    if check.exists() && !force {
        report
            .skipped
            .push("check.sh exists (use --force to overwrite)".into());
    } else {
        std::fs::write(&check, stack.template())?;
        let mut perms = std::fs::metadata(&check)?.permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&check, perms)?;
        report.actions.push(format!(
            "wrote check.sh ({} template, executable)",
            stack.label()
        ));
    }

    // .gitignore entries (idempotent append)
    let gitignore = project_root.join(".gitignore");
    let existing = std::fs::read_to_string(&gitignore).unwrap_or_default();
    let missing: Vec<&str> = GITIGNORE_ENTRIES
        .iter()
        .copied()
        .filter(|e| !existing.lines().any(|l| l.trim() == *e))
        .collect();
    if !missing.is_empty() {
        let mut content = existing;
        if !content.is_empty() && !content.ends_with('\n') {
            content.push('\n');
        }
        content.push_str("# ritual\n");
        for e in &missing {
            content.push_str(e);
            content.push('\n');
        }
        std::fs::write(&gitignore, content)?;
        report
            .actions
            .push(format!(".gitignore: added {} entries", missing.len()));
    }

    // CLAUDE.md: only create, never touch an existing one.
    let claude_md = project_root.join("CLAUDE.md");
    if claude_md.exists() {
        report
            .skipped
            .push("CLAUDE.md exists (left untouched)".into());
    } else {
        std::fs::write(&claude_md, CLAUDE_SNIPPET)?;
        report
            .actions
            .push("created CLAUDE.md with workflow snippet".into());
    }

    Ok(report)
}

fn rel(root: &Path, p: &Path) -> String {
    p.strip_prefix(root).unwrap_or(p).display().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn touch(dir: &Path, name: &str) {
        std::fs::write(dir.join(name), "x").unwrap();
    }

    #[test]
    fn stack_detection_matrix() {
        let t = tempfile::tempdir().unwrap();
        assert_eq!(detect_stack(t.path()), Stack::Unknown);
        touch(t.path(), "Cargo.toml");
        assert_eq!(detect_stack(t.path()), Stack::Rust);
        touch(t.path(), "package.json");
        assert_eq!(detect_stack(t.path()), Stack::Mixed);

        let t2 = tempfile::tempdir().unwrap();
        touch(t2.path(), "pyproject.toml");
        assert_eq!(detect_stack(t2.path()), Stack::Python);
        let t3 = tempfile::tempdir().unwrap();
        touch(t3.path(), "package.json");
        assert_eq!(detect_stack(t3.path()), Stack::Node);
    }

    #[test]
    fn init_scaffolds_and_is_idempotent() {
        let t = tempfile::tempdir().unwrap();
        touch(t.path(), "Cargo.toml");
        let r1 = init(t.path(), false).unwrap();
        assert!(t.path().join(".ritual/findings").is_dir());
        assert!(t.path().join(".ritual/state.json").exists());
        assert!(t.path().join(".ritual/invariants.md").exists());
        assert!(t.path().join("check.sh").exists());
        let mode = std::fs::metadata(t.path().join("check.sh"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o111, 0o111, "check.sh must be executable");
        let gi = std::fs::read_to_string(t.path().join(".gitignore")).unwrap();
        assert!(gi.contains(".ritual/runs/"));
        assert!(r1.actions.iter().any(|a| a.contains("rust template")));

        // Second run: nothing rewritten, check.sh preserved without --force.
        std::fs::write(t.path().join("check.sh"), "#custom").unwrap();
        std::fs::write(t.path().join(".ritual/invariants.md"), "- mine\n").unwrap();
        let r2 = init(t.path(), false).unwrap();
        assert_eq!(
            std::fs::read_to_string(t.path().join(".ritual/invariants.md")).unwrap(),
            "- mine\n",
            "the constitution is never overwritten"
        );
        assert_eq!(
            std::fs::read_to_string(t.path().join("check.sh")).unwrap(),
            "#custom"
        );
        assert!(r2.skipped.iter().any(|s| s.contains("check.sh exists")));
        let gi2 = std::fs::read_to_string(t.path().join(".gitignore")).unwrap();
        assert_eq!(
            gi.matches(".ritual/runs/").count(),
            gi2.matches(".ritual/runs/").count()
        );

        // --force overwrites.
        let r3 = init(t.path(), true).unwrap();
        assert!(
            std::fs::read_to_string(t.path().join("check.sh"))
                .unwrap()
                .contains("cargo fmt")
        );
        assert!(r3.actions.iter().any(|a| a.contains("check.sh")));
    }
}
