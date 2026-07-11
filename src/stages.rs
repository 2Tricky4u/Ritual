use anyhow::Result;

use crate::config::Config;
use crate::runner::AgentKind;
use crate::state::{RitualDirs, StageId};

/// How a stage runs: piped + parsed, or attached to the user's terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Headless,
    Interactive,
    /// Handled by ritual itself (spec editing), no agent process.
    Local,
}

#[derive(Debug, Clone)]
pub struct StageCommand {
    pub mode: Mode,
    pub agent: AgentKind,
    pub argv: Vec<String>,
    pub env: Vec<(String, String)>,
    /// Whether this stage talks to Codex via MCP (needs codex auth preflight).
    pub needs_codex: bool,
}

const PLAN_REVIEW_TOOLS: &str =
    "Read Glob Grep Edit Write Bash(git *) mcp__codex__codex mcp__codex__codex-reply";
const DUAL_REVIEW_TOOLS: &str =
    "Task Read Glob Grep Edit Write Bash mcp__codex__codex mcp__codex__codex-reply";

/// Build the exact command for a stage. `arg` is the optional user argument
/// (plan path for plan-review, base ref for dual-review).
pub fn build(
    stage: StageId,
    cfg: &Config,
    dirs: &RitualDirs,
    slug: &str,
    arg: Option<&str>,
) -> Result<StageCommand> {
    let claude = cfg.claude_cmd.clone();
    let findings_env = (
        "RITUAL_FINDINGS_DIR".to_string(),
        dirs.findings_dir().display().to_string(),
    );

    let mut cmd = match stage {
        StageId::Spec => StageCommand {
            mode: Mode::Local,
            agent: AgentKind::Claude,
            argv: vec![],
            env: vec![],
            needs_codex: false,
        },
        StageId::Plan => {
            let spec = dirs.spec_file(slug);
            let plan = dirs.plan_file(slug);
            let prompt = format!(
                "Read {} and plan the implementation. When the plan is approved, save it to {} before finishing.",
                spec.display(),
                plan.display()
            );
            StageCommand {
                mode: Mode::Interactive,
                agent: AgentKind::Claude,
                argv: [
                    claude,
                    vec!["--permission-mode".into(), "plan".into(), prompt],
                ]
                .concat(),
                env: vec![],
                needs_codex: false,
            }
        }
        StageId::PlanReview => {
            let plan = arg
                .map(str::to_string)
                .unwrap_or_else(|| dirs.plan_file(slug).display().to_string());
            anyhow::ensure!(
                std::path::Path::new(&plan).exists(),
                "plan file not found: {plan} — run the plan stage first (or pass a path)"
            );
            StageCommand {
                mode: Mode::Headless,
                agent: AgentKind::Claude,
                argv: [
                    claude,
                    vec![
                        "-p".into(),
                        format!("/plan-review {plan}"),
                        "--output-format".into(),
                        "stream-json".into(),
                        "--verbose".into(),
                        "--permission-mode".into(),
                        "acceptEdits".into(),
                        "--allowedTools".into(),
                        PLAN_REVIEW_TOOLS.into(),
                        "--max-budget-usd".into(),
                        cfg.budget_plan_review_usd.to_string(),
                    ],
                ]
                .concat(),
                env: vec![findings_env],
                needs_codex: true,
            }
        }
        StageId::TestsRed => {
            let plan = dirs.plan_file(slug);
            StageCommand {
                mode: Mode::Interactive,
                agent: AgentKind::Claude,
                argv: [claude, vec![format!("/tdd {}", plan.display())]].concat(),
                env: vec![],
                needs_codex: true,
            }
        }
        StageId::Implement => StageCommand {
            mode: Mode::Interactive,
            agent: AgentKind::Claude,
            argv: [claude, vec!["--continue".into()]].concat(),
            env: vec![],
            needs_codex: false,
        },
        StageId::DualReview => {
            let base = arg
                .map(str::to_string)
                .unwrap_or_else(|| cfg.base_ref.clone());
            StageCommand {
                mode: Mode::Headless,
                agent: AgentKind::Claude,
                argv: [
                    claude,
                    vec![
                        "-p".into(),
                        format!("/dual-review {base}"),
                        "--output-format".into(),
                        "stream-json".into(),
                        "--verbose".into(),
                        "--permission-mode".into(),
                        "acceptEdits".into(),
                        "--allowedTools".into(),
                        DUAL_REVIEW_TOOLS.into(),
                        "--max-budget-usd".into(),
                        cfg.budget_dual_review_usd.to_string(),
                    ],
                ]
                .concat(),
                env: vec![findings_env],
                needs_codex: true,
            }
        }
    };
    // Per-stage model routing ([models] config table).
    if let Some(model) = cfg.models.get(stage.label())
        && !cmd.argv.is_empty()
    {
        cmd.argv.push("--model".into());
        cmd.argv.push(model.clone());
    }
    Ok(cmd)
}

/// Codex auth preflight: `codex login status` exits 0 when logged in.
pub fn codex_ready(cfg: &Config) -> bool {
    let Some((bin, args)) = cfg.codex_cmd.split_first() else {
        return false;
    };
    std::process::Command::new(bin)
        .args(args)
        .args(["login", "status"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::RitualDirs;

    fn setup() -> (tempfile::TempDir, Config, RitualDirs) {
        let tmp = tempfile::tempdir().unwrap();
        let dirs = RitualDirs::new(tmp.path());
        (tmp, Config::default(), dirs)
    }

    #[test]
    fn plan_review_command_shape() {
        let (_tmp, cfg, dirs) = setup();
        std::fs::create_dir_all(dirs.feature_dir("feat-x")).unwrap();
        std::fs::write(dirs.plan_file("feat-x"), "# plan").unwrap();
        let cmd = build(StageId::PlanReview, &cfg, &dirs, "feat-x", None).unwrap();
        assert_eq!(cmd.mode, Mode::Headless);
        assert!(cmd.needs_codex);
        assert!(cmd.argv.iter().any(|a| a.starts_with("/plan-review ")));
        assert!(cmd.argv.contains(&"stream-json".to_string()));
        assert!(cmd.argv.contains(&"--max-budget-usd".to_string()));
        assert!(cmd.env.iter().any(|(k, _)| k == "RITUAL_FINDINGS_DIR"));
    }

    #[test]
    fn plan_review_requires_plan_file() {
        let (_tmp, cfg, dirs) = setup();
        assert!(build(StageId::PlanReview, &cfg, &dirs, "feat-x", None).is_err());
    }

    #[test]
    fn dual_review_uses_base_ref() {
        let (_tmp, cfg, dirs) = setup();
        let cmd = build(StageId::DualReview, &cfg, &dirs, "s", None).unwrap();
        assert!(cmd.argv.contains(&"/dual-review main".to_string()));
        let cmd = build(StageId::DualReview, &cfg, &dirs, "s", Some("develop")).unwrap();
        assert!(cmd.argv.contains(&"/dual-review develop".to_string()));
    }

    #[test]
    fn interactive_stages_have_no_stream_flags() {
        let (_tmp, cfg, dirs) = setup();
        for stage in [StageId::Plan, StageId::TestsRed, StageId::Implement] {
            let cmd = build(stage, &cfg, &dirs, "s", None).unwrap();
            assert_eq!(cmd.mode, Mode::Interactive);
            assert!(!cmd.argv.contains(&"stream-json".to_string()));
        }
    }
}
