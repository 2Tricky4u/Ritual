//! The vendored multi-LLM workbench: every skill, agent, and hook the
//! workflow needs, compiled into the binary so one `git clone` + `ritual
//! init --skills` reproduces the whole setup on a fresh machine. The repo's
//! `workbench/` directory is the source of truth; provenance hashes derive
//! from [`SKILLS`].

use std::path::Path;

use anyhow::{Context, Result};

/// (skill name, SKILL.md body), installed to `~/.claude/skills/<name>/SKILL.md`.
pub const SKILLS: &[(&str, &str)] = &[
    (
        "brainstorm",
        include_str!("../workbench/skills/brainstorm/SKILL.md"),
    ),
    (
        "changelog",
        include_str!("../workbench/skills/changelog/SKILL.md"),
    ),
    (
        "commit",
        include_str!("../workbench/skills/commit/SKILL.md"),
    ),
    (
        "consensus",
        include_str!("../workbench/skills/consensus/SKILL.md"),
    ),
    (
        "coverage",
        include_str!("../workbench/skills/coverage/SKILL.md"),
    ),
    ("debug", include_str!("../workbench/skills/debug/SKILL.md")),
    (
        "deps-audit",
        include_str!("../workbench/skills/deps-audit/SKILL.md"),
    ),
    ("docs", include_str!("../workbench/skills/docs/SKILL.md")),
    (
        "document",
        include_str!("../workbench/skills/document/SKILL.md"),
    ),
    (
        "dual-review",
        include_str!("../workbench/skills/dual-review/SKILL.md"),
    ),
    (
        "plan-review",
        include_str!("../workbench/skills/plan-review/SKILL.md"),
    ),
    ("pr", include_str!("../workbench/skills/pr/SKILL.md")),
    ("spec", include_str!("../workbench/skills/spec/SKILL.md")),
    ("tdd", include_str!("../workbench/skills/tdd/SKILL.md")),
];

/// Non-skill workbench files, installed relative to `~/.claude/`.
pub struct VendoredFile {
    pub rel: &'static str,
    pub body: &'static str,
    pub exec: bool,
}

pub const EXTRAS: &[VendoredFile] = &[
    VendoredFile {
        rel: "agents/code-reviewer.md",
        body: include_str!("../workbench/agents/code-reviewer.md"),
        exec: false,
    },
    VendoredFile {
        rel: "hooks/check-on-edit.sh",
        body: include_str!("../workbench/hooks/check-on-edit.sh"),
        exec: true,
    },
    VendoredFile {
        rel: "hooks/secrets-guard.py",
        body: include_str!("../workbench/hooks/secrets-guard.py"),
        exec: true,
    },
];

/// Reference for the settings.json blocks: printed as a pointer, never
/// merged automatically (`ritual doctor` checks for the hook block).
pub const SETTINGS_SNIPPET: &str = include_str!("../workbench/settings-snippet.json");

/// One skill's installed-vs-vendored state (`ritual skills diff`).
#[derive(Debug)]
pub enum SkillDiff {
    Identical,
    Missing,
    Differs {
        repo_lines: usize,
        installed_lines: usize,
        /// 1-based first differing line + both sides' content there.
        first: (usize, String, String),
    },
}

/// Compare every vendored skill against the installed copy under
/// `<claude_home>/skills/<name>/SKILL.md`, a read-only companion to
/// `install()` (doctor only hashes; this says WHERE they diverge).
pub fn diff(claude_home: &Path) -> Vec<(&'static str, SkillDiff)> {
    SKILLS
        .iter()
        .map(|(name, repo)| {
            let path = claude_home.join("skills").join(name).join("SKILL.md");
            let status = match std::fs::read_to_string(&path) {
                Err(_) => SkillDiff::Missing,
                Ok(installed) if installed == *repo => SkillDiff::Identical,
                Ok(installed) => {
                    let mut first: Option<(usize, String, String)> = None;
                    for (i, pair) in repo
                        .lines()
                        .map(Some)
                        .chain(std::iter::repeat(None))
                        .zip(installed.lines().map(Some).chain(std::iter::repeat(None)))
                        .enumerate()
                    {
                        match pair {
                            (None, None) => break,
                            (ra, ia) if ra != ia => {
                                first = Some((
                                    i + 1,
                                    ra.unwrap_or("<end of file>").to_string(),
                                    ia.unwrap_or("<end of file>").to_string(),
                                ));
                                break;
                            }
                            _ => {}
                        }
                    }
                    // Bytes differ but no LINE differs: the divergence is in
                    // line endings or a trailing newline, so say so instead of
                    // reporting an empty "line 1" diff.
                    let first = first.unwrap_or_else(|| {
                        let marker = "<line endings / trailing newline differ>".to_string();
                        (repo.lines().count().max(1), marker.clone(), marker)
                    });
                    SkillDiff::Differs {
                        repo_lines: repo.lines().count(),
                        installed_lines: installed.lines().count(),
                        first,
                    }
                }
            };
            (*name, status)
        })
        .collect()
}

#[derive(Debug, Default)]
pub struct InstallReport {
    pub created: Vec<String>,
    pub updated: Vec<String>,
    pub identical: Vec<String>,
    /// Local file differs from the vendored one, left alone (use --force).
    pub skipped: Vec<String>,
}

/// Install the workbench into `claude_home` (normally `~/.claude`).
/// Semantics per file: absent → write; byte-identical → no-op; differs →
/// skip with a warning unless `force`.
pub fn install(claude_home: &Path, force: bool) -> Result<InstallReport> {
    let mut report = InstallReport::default();
    let mut put = |rel: String, body: &str, exec: bool| -> Result<()> {
        let path = claude_home.join(&rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        let bucket = if !path.exists() {
            &mut report.created
        } else if std::fs::read_to_string(&path)
            .map(|c| c == body)
            .unwrap_or(false)
        {
            report.identical.push(rel);
            return Ok(());
        } else if force {
            &mut report.updated
        } else {
            report.skipped.push(rel);
            return Ok(());
        };
        std::fs::write(&path, body).with_context(|| format!("writing {}", path.display()))?;
        if exec {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&path)?.permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&path, perms)?;
        }
        bucket.push(rel);
        Ok(())
    };

    for (name, body) in SKILLS {
        put(format!("skills/{name}/SKILL.md"), body, false)?;
    }
    for f in EXTRAS {
        put(f.rel.to_string(), f.body, f.exec)?;
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The git contracts the pipeline relies on must not silently drift out
    /// of the vendored skills: dual-review reviews the WORKING TREE (a
    /// committed-only diff skipped every uncommitted implementation), tdd
    /// commits only its own test files, and pr flags uncommitted work.
    #[test]
    fn skill_git_contracts_survive_edits() {
        let body = |name: &str| {
            SKILLS
                .iter()
                .find(|(n, _)| *n == name)
                .map(|(_, b)| *b)
                .unwrap()
        };
        let dual = body("dual-review");
        assert!(dual.contains("merge-base"), "worktree-vs-merge-base diff");
        assert!(dual.contains("ls-files --others"), "untracked files read");
        let tdd = body("tdd");
        assert!(!tdd.contains("Commit the failing tests if in a git repo"));
        assert!(tdd.contains("git add <each test file path>"));
        assert!(tdd.contains("red-only"), "ritual's tests-red stop point");
        let pr = body("pr");
        assert!(pr.contains("status --porcelain"), "dirty-tree PR warning");
    }

    #[test]
    fn install_covers_all_dispositions_and_is_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();

        // Fresh home: everything created.
        let r = install(home, false).unwrap();
        assert_eq!(r.created.len(), SKILLS.len() + EXTRAS.len());
        assert!(r.updated.is_empty() && r.skipped.is_empty());
        assert!(home.join("skills/spec/SKILL.md").exists());
        assert!(home.join("skills/consensus/SKILL.md").exists());

        // Hooks carry the exec bit.
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(home.join("hooks/check-on-edit.sh"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o111, 0o111, "hook not executable");

        // Second run: all identical, nothing rewritten.
        let r = install(home, false).unwrap();
        assert_eq!(r.identical.len(), SKILLS.len() + EXTRAS.len());
        assert!(r.created.is_empty());

        // Local edit: skipped without --force, updated with it.
        std::fs::write(home.join("skills/tdd/SKILL.md"), "locally modified").unwrap();
        let r = install(home, false).unwrap();
        assert_eq!(r.skipped, vec!["skills/tdd/SKILL.md".to_string()]);
        assert!(
            std::fs::read_to_string(home.join("skills/tdd/SKILL.md"))
                .unwrap()
                .contains("locally modified")
        );
        let r = install(home, true).unwrap();
        assert_eq!(r.updated, vec!["skills/tdd/SKILL.md".to_string()]);
        assert!(
            std::fs::read_to_string(home.join("skills/tdd/SKILL.md"))
                .unwrap()
                .contains("name: tdd")
        );
    }

    #[test]
    fn vendored_skills_have_matching_frontmatter_names() {
        for (name, body) in SKILLS {
            assert!(
                body.contains(&format!("name: {name}")),
                "workbench/skills/{name}/SKILL.md frontmatter name mismatch"
            );
        }
    }

    #[test]
    fn diff_reports_eol_only_and_eof_divergence() {
        let home = tempfile::tempdir().unwrap();
        install(home.path(), false).unwrap();
        let tdd = home.path().join("skills/tdd/SKILL.md");
        let original = std::fs::read_to_string(&tdd).unwrap();

        // Trailing-newline-only divergence (final \n stripped, the one case
        // where bytes differ but no LINE does): named, not an empty "line 1".
        std::fs::write(&tdd, original.trim_end_matches('\n')).unwrap();
        let d = diff(home.path());
        let (_, status) = d.iter().find(|(n, _)| *n == "tdd").unwrap();
        match status {
            SkillDiff::Differs {
                first: (line, a, b),
                ..
            } => {
                assert!(a.contains("trailing newline"), "{a}");
                assert_eq!(a, b);
                assert!(*line >= 1);
            }
            other => panic!("expected Differs, got {other:?}"),
        }

        // Extra installed line -> the repo side hits <end of file>.
        std::fs::write(&tdd, format!("{original}EXTRA\n")).unwrap();
        let d = diff(home.path());
        let (_, status) = d.iter().find(|(n, _)| *n == "tdd").unwrap();
        match status {
            SkillDiff::Differs {
                repo_lines,
                installed_lines,
                first: (line, a, b),
            } => {
                assert_eq!(*installed_lines, *repo_lines + 1);
                assert_eq!(*line, *repo_lines + 1);
                assert_eq!(a, "<end of file>");
                assert_eq!(b, "EXTRA");
            }
            other => panic!("expected Differs, got {other:?}"),
        }
    }
}
