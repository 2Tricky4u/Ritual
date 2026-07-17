//! `ritual doctor`: one command that checks every prerequisite the workflow
//! depends on: agents, auth, MCP wiring, skills drift, hooks, check.sh, disk
//! pressure, and budget sanity. Hard failures (things nothing works without)
//! exit nonzero; drift and optional pieces warn.

use std::path::Path;

use crate::agents_status;
use crate::config::Config;
use crate::provenance::sha256_hex;
use crate::state::RitualDirs;
use crate::workbench;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckStatus {
    Pass,
    Warn,
    Fail,
    Skipped,
}

#[derive(Debug)]
pub struct CheckResult {
    pub name: &'static str,
    pub status: CheckStatus,
    pub detail: String,
}

fn check(name: &'static str, status: CheckStatus, detail: impl Into<String>) -> CheckResult {
    CheckResult {
        name,
        status,
        detail: detail.into(),
    }
}

/// Run every check. `deep` also runs `./check.sh fast`. The caller exits
/// nonzero when any result is `Fail`.
pub fn run(cfg: &Config, dirs: &RitualDirs, deep: bool) -> Vec<CheckResult> {
    let mut out = Vec::new();

    // -- project ------------------------------------------------------------
    out.push(if dirs.exists() {
        check("project", CheckStatus::Pass, ".ritual/ present")
    } else {
        check(
            "project",
            CheckStatus::Fail,
            "no .ritual/ here; run `ritual init`",
        )
    });

    let check_sh = dirs.work_root.join("check.sh");
    out.push(if !check_sh.exists() {
        check(
            "check.sh",
            CheckStatus::Fail,
            "missing: the whole loop depends on it (`ritual init`)",
        )
    } else if !is_executable(&check_sh) {
        check("check.sh", CheckStatus::Fail, "not executable (chmod +x)")
    } else {
        check("check.sh", CheckStatus::Pass, "present + executable")
    });

    // -- agents (skipped offline) --------------------------------------------
    if cfg.offline {
        for name in ["claude", "codex", "codex mcp", "pal mcp"] {
            let name: &'static str = match name {
                "claude" => "claude",
                "codex" => "codex",
                "codex mcp" => "codex mcp",
                _ => "pal mcp",
            };
            out.push(check(name, CheckStatus::Skipped, "offline = true"));
        }
    } else {
        let version = agents_status::run_capture(&cfg.claude_cmd, &["--version"])
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());
        out.push(match (version, agents_status::probe_claude_auth(cfg)) {
            (Some(v), Some(auth)) if auth.logged_in => check(
                "claude",
                CheckStatus::Pass,
                format!(
                    "{v} · logged in{}",
                    auth.subscription_type
                        .map(|s| format!(" ({s})"))
                        .unwrap_or_default()
                ),
            ),
            (Some(v), _) => check(
                "claude",
                CheckStatus::Fail,
                format!("{v} · NOT logged in; run `claude` once to auth"),
            ),
            (None, _) => check(
                "claude",
                CheckStatus::Fail,
                format!("`{}` not runnable", cfg.claude_cmd.join(" ")),
            ),
        });

        out.push(match agents_status::probe_codex_cli(cfg) {
            Some(true) => check("codex", CheckStatus::Pass, "logged in"),
            Some(false) => check(
                "codex",
                CheckStatus::Warn,
                "not logged in (`codex login`). Cross-model stages unavailable",
            ),
            None => check(
                "codex",
                CheckStatus::Warn,
                format!("`{}` not runnable", cfg.codex_cmd.join(" ")),
            ),
        });

        out.push(match agents_status::probe_mcp_server(cfg, "codex") {
            Some(true) => check("codex mcp", CheckStatus::Pass, "connected"),
            Some(false) => check(
                "codex mcp",
                CheckStatus::Warn,
                "registered but not connected",
            ),
            None => check(
                "codex mcp",
                CheckStatus::Warn,
                "not registered (`claude mcp add --scope user codex -- codex mcp-server`)",
            ),
        });

        out.push(match agents_status::probe_mcp_server(cfg, "pal") {
            Some(true) => check("pal mcp", CheckStatus::Pass, "connected (consensus tier)"),
            Some(false) => check("pal mcp", CheckStatus::Warn, "registered but not connected"),
            None if cfg.consensus_enabled => check(
                "pal mcp",
                CheckStatus::Warn,
                "consensus enabled but pal is not registered; see the guide",
            ),
            None => check(
                "pal mcp",
                CheckStatus::Skipped,
                "not registered (optional consensus tier)",
            ),
        });
    }

    // -- nvim -----------------------------------------------------------------
    out.push(match crate::nvim::discover(cfg.nvim_server.as_deref()) {
        Some(sock) => check("nvim", CheckStatus::Pass, sock.display().to_string()),
        None => check(
            "nvim",
            CheckStatus::Warn,
            "no running nvim found (o/Q keys inert)",
        ),
    });

    // -- mutation gate -----------------------------------------------------------
    if cfg.mutants_enabled {
        let runnable = agents_status::run_capture(&cfg.mutants_cmd, &["--version"])
            .is_some_and(|o| o.status.success());
        out.push(if runnable {
            check("mutants", CheckStatus::Pass, "runner available")
        } else {
            check(
                "mutants",
                CheckStatus::Warn,
                format!(
                    "`{}` not runnable (cargo install cargo-mutants)",
                    cfg.mutants_cmd.join(" ")
                ),
            )
        });
    }

    // -- architecture map ---------------------------------------------------------
    // Advisory only (Warn, never Fail): plans ground themselves in the map,
    // but the pipeline runs fine without one.
    out.push(if !cfg.architect_enabled {
        check(
            "architecture",
            CheckStatus::Skipped,
            "nudges disabled in [architect]",
        )
    } else {
        use crate::architect::ArchStatus;
        let meaningful = crate::stages::meaningful_architecture(dirs).is_some();
        let (stamp, current) = if meaningful {
            (
                crate::architect::read_stamp(dirs),
                crate::provenance::arch_fingerprint(&dirs.work_root),
            )
        } else {
            (None, None)
        };
        match crate::architect::status(meaningful, stamp.as_deref(), current.as_deref()) {
            ArchStatus::Missing => check(
                "architecture",
                CheckStatus::Warn,
                "no map - run `ritual architect` so plans ground themselves in it",
            ),
            ArchStatus::Stale => check(
                "architecture",
                CheckStatus::Warn,
                "map stale (code changed since generation) - run `ritual architect`",
            ),
            ArchStatus::Fresh => check("architecture", CheckStatus::Pass, "map fresh"),
            ArchStatus::Unknown => check(
                "architecture",
                CheckStatus::Pass,
                "map present (fingerprint unknown: not a git tree or never stamped)",
            ),
        }
    });

    // -- sandbox wrapper ----------------------------------------------------------
    if cfg.sandbox_enabled {
        out.push(if cfg.sandbox_wrapper.is_empty() {
            check(
                "sandbox",
                CheckStatus::Fail,
                "[sandbox] enabled but no wrapper configured; see docs/srt-settings.example.json",
            )
        } else if which(&cfg.sandbox_wrapper[0]) {
            check(
                "sandbox",
                CheckStatus::Pass,
                format!(
                    "headless runs wrapped in `{}`",
                    cfg.sandbox_wrapper.join(" ")
                ),
            )
        } else {
            check(
                "sandbox",
                CheckStatus::Fail,
                format!(
                    "wrapper `{}` not on PATH: headless runs would fail to spawn",
                    cfg.sandbox_wrapper[0]
                ),
            )
        });
        for dep in ["bwrap", "socat"] {
            if !which(dep) {
                out.push(check(
                    "sandbox",
                    CheckStatus::Warn,
                    format!("`{dep}` missing (pacman -S bubblewrap socat ripgrep); srt needs it"),
                ));
            }
        }
    }

    // -- coderabbit third reviewer -------------------------------------------------
    if cfg.coderabbit_enabled {
        out.push(if crate::coderabbit::available(cfg) {
            check(
                "coderabbit",
                CheckStatus::Pass,
                "third reviewer runs before dual-review (3/hour on free tier)",
            )
        } else {
            check(
                "coderabbit",
                CheckStatus::Warn,
                format!(
                    "`{}` not runnable; install + `coderabbit auth login` (see the guide)",
                    cfg.coderabbit_cmd.join(" ")
                ),
            )
        });
    }

    // -- secrets gate -------------------------------------------------------------
    // The scan lists changed files VIA GIT: outside a repo it scans nothing,
    // so a green here would be a lie. The probe must check STDOUT, not just
    // the exit code - inside .git/ or a bare repo the command exits 0 and
    // prints "false" (same contract as git::dual_review_preflight).
    let in_git_repo = std::process::Command::new("git")
        .args(["rev-parse", "--is-inside-work-tree"])
        .current_dir(&dirs.work_root)
        .output()
        .is_ok_and(|o| o.status.success() && String::from_utf8_lossy(&o.stdout).trim() == "true");
    out.push(if !cfg.secrets_enabled {
        check("secrets", CheckStatus::Skipped, "disabled in [secrets]")
    } else if !crate::secrets::available(cfg) {
        check(
            "secrets",
            CheckStatus::Warn,
            format!(
                "`{}` not runnable; gate silently skipped (pacman -S gitleaks)",
                cfg.gitleaks_cmd.join(" ")
            ),
        )
    } else if !in_git_repo {
        check(
            "secrets",
            CheckStatus::Warn,
            "not a git repo: the changed-files scan is inert (nothing gets scanned)",
        )
    } else {
        check(
            "secrets",
            CheckStatus::Pass,
            "gitleaks scans changed files before dual-review",
        )
    });

    // -- branch slug collisions -------------------------------------------------
    // `feat/x` and `feat-x` share one slug = one state entry/plan/findings
    // scope; warn before that silently merges two features' bookkeeping.
    let collisions = crate::state::slug_collisions(&dirs.work_root);
    if !collisions.is_empty() {
        let desc = collisions
            .iter()
            .map(|(slug, branches)| format!("{} → '{slug}'", branches.join(" + ")))
            .collect::<Vec<_>>()
            .join("; ");
        out.push(check(
            "slug",
            CheckStatus::Warn,
            format!("branches share a state slug ({desc}); rename one branch"),
        ));
    } else {
        out.push(check(
            "slug",
            CheckStatus::Pass,
            "every local branch has its own state slug",
        ));
    }

    // -- invariants constitution -----------------------------------------------
    out.push(match crate::stages::meaningful_invariants(dirs) {
        Some(_) => check(
            "invariants",
            CheckStatus::Pass,
            ".ritual/invariants.md enforced by review stages",
        ),
        None => check(
            "invariants",
            CheckStatus::Skipped,
            "none: optional; add bullets to .ritual/invariants.md (ritual init scaffolds it)",
        ),
    });

    // -- workbench drift -------------------------------------------------------
    out.push(workbench_check());
    out.push(hooks_check());

    // -- disk ------------------------------------------------------------------
    out.push(disk_check(&dirs.project_root));

    // -- budget sanity -----------------------------------------------------------
    out.push(budget_check(cfg));

    // -- deep: actually run the fast checks ---------------------------------------
    if deep {
        out.push(if check_sh.exists() {
            let mut cmd = std::process::Command::new("./check.sh");
            cmd.arg("fast")
                .current_dir(&dirs.work_root)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null());
            match crate::run_cmd::run_with_timeout(cmd, cfg.check_timeout_secs) {
                Some(s) if s.success() => check("check.sh fast", CheckStatus::Pass, "green"),
                Some(_) => check(
                    "check.sh fast",
                    CheckStatus::Fail,
                    "red: fix before running stages",
                ),
                None => check("check.sh fast", CheckStatus::Fail, "timed out"),
            }
        } else {
            check("check.sh fast", CheckStatus::Skipped, "no check.sh")
        });
    }

    out
}

fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p)
        .map(|m| m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

/// PATH lookup (absolute/relative paths checked directly).
fn which(bin: &str) -> bool {
    if bin.contains('/') {
        return is_executable(Path::new(bin));
    }
    std::env::var_os("PATH")
        .is_some_and(|paths| std::env::split_paths(&paths).any(|d| is_executable(&d.join(bin))))
}

/// Installed skills vs the vendored set (hash compare).
fn workbench_check() -> CheckResult {
    let Some(home) = claude_home() else {
        return check("skills", CheckStatus::Warn, "no home directory");
    };
    let mut missing = Vec::new();
    let mut drifted = Vec::new();
    for (name, body) in workbench::SKILLS {
        let p = home.join(format!("skills/{name}/SKILL.md"));
        match std::fs::read(&p) {
            Err(_) => missing.push(*name),
            Ok(bytes) if sha256_hex(&bytes) != sha256_hex(body.as_bytes()) => drifted.push(*name),
            Ok(_) => {}
        }
    }
    if missing.is_empty() && drifted.is_empty() {
        check(
            "skills",
            CheckStatus::Pass,
            format!(
                "{} installed, all match the vendored set",
                workbench::SKILLS.len()
            ),
        )
    } else {
        let mut parts = Vec::new();
        if !missing.is_empty() {
            parts.push(format!("missing: {}", missing.join(", ")));
        }
        if !drifted.is_empty() {
            parts.push(format!("drifted: {}", drifted.join(", ")));
        }
        check(
            "skills",
            CheckStatus::Warn,
            format!("{} (`ritual init --skills`)", parts.join(" · ")),
        )
    }
}

/// The PostToolUse check hook must be wired in settings.json.
fn hooks_check() -> CheckResult {
    let Some(home) = claude_home() else {
        return check("hooks", CheckStatus::Warn, "no home directory");
    };
    let settings = std::fs::read_to_string(home.join("settings.json")).unwrap_or_default();
    if settings.contains("check-on-edit") {
        check(
            "hooks",
            CheckStatus::Pass,
            "check-on-edit wired in settings.json",
        )
    } else {
        check(
            "hooks",
            CheckStatus::Warn,
            "check-on-edit hook not in settings.json; see workbench/settings-snippet.json",
        )
    }
}

/// `~/.claude`, honoring the same test seam as `init --skills`.
fn claude_home() -> Option<std::path::PathBuf> {
    workbench::claude_home()
}

/// Disk pressure on the project filesystem: a chronically full disk is a
/// real failure mode here (archives, builds).
fn disk_check(root: &Path) -> CheckResult {
    match nix::sys::statvfs::statvfs(root) {
        Ok(vfs) => {
            let free = vfs.blocks_available() * vfs.fragment_size();
            let gib = free as f64 / (1024.0 * 1024.0 * 1024.0);
            if free < 500 * 1024 * 1024 {
                check(
                    "disk",
                    CheckStatus::Fail,
                    format!("{gib:.1} GiB free; runs and builds will start failing"),
                )
            } else if gib < 5.0 {
                check("disk", CheckStatus::Warn, format!("{gib:.1} GiB free"))
            } else {
                check("disk", CheckStatus::Pass, format!("{gib:.0} GiB free"))
            }
        }
        Err(e) => check("disk", CheckStatus::Warn, format!("statvfs failed: {e}")),
    }
}

fn budget_check(cfg: &Config) -> CheckResult {
    let Some(daily) = cfg.budget_daily_usd else {
        return check("budget", CheckStatus::Pass, "no daily ceiling configured");
    };
    let mut over = Vec::new();
    for (name, per_run) in [
        ("plan-review", cfg.budget_plan_review_usd),
        ("dual-review", cfg.budget_dual_review_usd),
        ("doc-chat", cfg.budget_doc_chat_usd),
        ("finding-fix", cfg.budget_finding_fix_usd),
        ("code-fix", cfg.budget_code_fix_usd),
        ("coverage", cfg.budget_coverage_usd),
        ("audit (per leg)", cfg.budget_audit_usd),
        ("architect", cfg.budget_architect_usd),
    ] {
        if per_run > daily {
            over.push(format!("{name} (${per_run})"));
        }
    }
    if over.is_empty() {
        check("budget", CheckStatus::Pass, format!("daily ${daily}"))
    } else {
        check(
            "budget",
            CheckStatus::Warn,
            format!(
                "per-run cap exceeds the daily ceiling (${daily}): {}",
                over.join(", ")
            ),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn budget_sanity_flags_inverted_caps() {
        let mut cfg = Config {
            budget_daily_usd: Some(1.0),
            ..Default::default()
        };
        let r = budget_check(&cfg);
        assert_eq!(r.status, CheckStatus::Warn); // default per-run caps are larger
        assert!(r.detail.contains("plan-review"));

        cfg.budget_daily_usd = Some(100.0);
        assert_eq!(budget_check(&cfg).status, CheckStatus::Pass);
        // The code-fix cap participates in the same daily-ceiling check.
        cfg.budget_daily_usd = Some(4.0); // below the 5.0 code-fix default
        let r = budget_check(&cfg);
        assert_eq!(r.status, CheckStatus::Warn);
        assert!(r.detail.contains("code-fix"));
        cfg.budget_daily_usd = None;
        assert_eq!(budget_check(&cfg).status, CheckStatus::Pass);
    }

    #[test]
    fn workbench_check_detects_missing_and_drift() {
        let tmp = tempfile::tempdir().unwrap();
        // SAFETY: single-threaded test process for env mutation purposes is
        // not guaranteed, but the seam var is test-only and scoped here.
        unsafe { std::env::set_var("RITUAL_CLAUDE_HOME", tmp.path()) };
        let r = workbench_check();
        assert_eq!(r.status, CheckStatus::Warn);
        assert!(r.detail.contains("missing"));

        crate::workbench::install(tmp.path(), false).unwrap();
        let r = workbench_check();
        assert_eq!(r.status, CheckStatus::Pass, "{}", r.detail);

        std::fs::write(tmp.path().join("skills/tdd/SKILL.md"), "drifted").unwrap();
        let r = workbench_check();
        assert_eq!(r.status, CheckStatus::Warn);
        assert!(r.detail.contains("drifted: tdd"));
        unsafe { std::env::remove_var("RITUAL_CLAUDE_HOME") };
    }

    #[test]
    fn disk_check_reports_something() {
        let r = disk_check(Path::new("/"));
        assert!(matches!(
            r.status,
            CheckStatus::Pass | CheckStatus::Warn | CheckStatus::Fail
        ));
        assert!(r.detail.contains("free") || r.detail.contains("statvfs"));
    }
}
