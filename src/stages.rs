use std::path::Path;

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

/// plan-review's tool grant, plus the third-model consensus tool when the
/// (dark-by-default) escalation tier is enabled — the skill only escalates
/// genuinely contested findings, and only when the pal MCP server exists.
fn plan_review_tools(cfg: &Config) -> String {
    if cfg.consensus_enabled {
        format!("{PLAN_REVIEW_TOOLS} mcp__pal__consensus")
    } else {
        PLAN_REVIEW_TOOLS.to_string()
    }
}
/// The doc-chat agent may read anything but edit ONLY the one document it's
/// given. Path rules are gitignore-style; a single leading '/' anchors at the
/// settings source, so a filesystem-absolute path needs '//'. Enforced by
/// `dontAsk` mode — `acceptEdits` would auto-approve edits everywhere and
/// defeat the scoping (verified against the permission docs).
fn doc_chat_tools(doc_path: &Path) -> String {
    let p = doc_path.display().to_string(); // absolute, starts with '/'
    format!("Read,Edit(/{p}),Write(/{p})")
}

/// Sandbox argv prefix for a run: only headless runs are wrapped (interactive
/// stages own the user's terminal — bubblewrap-style isolation would break
/// them), and only when `[sandbox]` is enabled with a wrapper configured.
pub fn wrapper_argv(cfg: &Config, mode: Mode) -> Vec<String> {
    if cfg.sandbox_enabled && mode == Mode::Headless {
        cfg.sandbox_wrapper.clone()
    } else {
        Vec::new()
    }
}

/// The project constitution rides along once it has real content (bullets,
/// not the scaffold's comments). Re-injected into every review stage so a
/// standing constraint can never silently fall out of context.
pub fn meaningful_invariants(dirs: &RitualDirs) -> Option<std::path::PathBuf> {
    let path = dirs.invariants_file();
    let text = std::fs::read_to_string(&path).ok()?;
    crate::spec::has_meaningful_content(&text).then_some(path)
}

/// Which ritual document a chat edit targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocKind {
    Spec,
    Plan,
}

impl DocKind {
    pub fn label(self) -> &'static str {
        match self {
            DocKind::Spec => "spec",
            DocKind::Plan => "plan",
        }
    }
}

/// How much of a document a chat edit may touch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scope {
    Whole,
    /// A single `##` section, identified by its heading text.
    Section(String),
}

/// Build the headless command for ONE spec/plan chat message. `doc_path` must
/// be absolute (headless runs execute in `work_root`, but the document lives
/// under `project_root/.ritual`). `spec_path` rides along for plan targets so
/// a missing plan can be DRAFTED from the spec. `context` is a short "recent
/// conversation" block (may be empty). Everything — paths, scope, message —
/// rides in the single `-p` prompt, because the agent has no Bash to read env.
#[allow(clippy::too_many_arguments)] // every arg is one prompt line; a params struct would just rename them
pub fn doc_chat_command(
    cfg: &Config,
    doc_path: &Path,
    kind: DocKind,
    scope: &Scope,
    message: &str,
    context: &str,
    spec_path: Option<&Path>,
    invariants: Option<&Path>,
) -> StageCommand {
    let scope_line = match scope {
        Scope::Whole => "SCOPE: whole".to_string(),
        Scope::Section(name) => format!("SCOPE: section \"{name}\""),
    };
    let spec_line = match spec_path {
        Some(p) => format!("SPEC_FILE: {}\n", p.display()),
        None => String::new(),
    };
    let inv_line = match invariants {
        Some(p) => format!(
            "INVARIANTS_FILE: {} (non-negotiable constraints — never write content that contradicts them)\n",
            p.display()
        ),
        None => String::new(),
    };
    let ctx_block = if context.trim().is_empty() {
        String::new()
    } else {
        format!("\n\nRECENT CONVERSATION (context only — do NOT re-apply):\n{context}")
    };
    let prompt = format!(
        "/spec Apply one scoped change to this ritual document.\n\n\
         DOC_FILE: {}\n\
         DOC_KIND: {}\n\
         {spec_line}{inv_line}{scope_line}\n\n\
         REQUEST:\n{message}{ctx_block}",
        doc_path.display(),
        kind.label(),
    );
    let mut cmd = StageCommand {
        mode: Mode::Headless,
        agent: AgentKind::Claude,
        argv: [
            cfg.claude_cmd.clone(),
            vec![
                "-p".into(),
                prompt,
                "--output-format".into(),
                "stream-json".into(),
                "--verbose".into(),
                // dontAsk: everything outside the allow rules is denied
                // instantly (no headless hang) — the doc is the only
                // writable file.
                "--permission-mode".into(),
                "dontAsk".into(),
                "--allowedTools".into(),
                doc_chat_tools(doc_path),
                "--max-budget-usd".into(),
                cfg.budget_doc_chat_usd.to_string(),
            ],
        ]
        .concat(),
        env: vec![],
        needs_codex: false,
    };
    // Per-document model routing ([models] spec / plan).
    if let Some(model) = cfg.models.get(kind.label()) {
        cmd.argv.push("--model".into());
        cmd.argv.push(model.clone());
    }
    if let Some(fb) = &cfg.fallback_model {
        cmd.argv.push("--fallback-model".into());
        cmd.argv.push(fb.clone());
    }
    cmd
}

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
                        plan_review_tools(cfg),
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
    // Overload resilience: headless claude runs retry on a fallback model
    // instead of dying to a 529 hours into a review (interactive runs can
    // negotiate with the user; codex has no such flag).
    if let Some(fb) = &cfg.fallback_model
        && cmd.mode == Mode::Headless
        && cmd.agent == AgentKind::Claude
    {
        cmd.argv.push("--fallback-model".into());
        cmd.argv.push(fb.clone());
    }
    // Both review stages enforce the constitution (skills fall back to the
    // well-known path for interactive stages like tests-red).
    if matches!(stage, StageId::PlanReview | StageId::DualReview)
        && let Some(p) = meaningful_invariants(dirs)
    {
        cmd.env
            .push(("RITUAL_INVARIANTS_FILE".into(), p.display().to_string()));
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
    fn consensus_tool_is_granted_only_when_enabled() {
        let (_tmp, mut cfg, dirs) = setup();
        std::fs::create_dir_all(dirs.feature_dir("s")).unwrap();
        std::fs::write(dirs.plan_file("s"), "# plan").unwrap();

        let cmd = build(StageId::PlanReview, &cfg, &dirs, "s", None).unwrap();
        let tools = cmd.argv.iter().find(|a| a.contains("mcp__codex")).unwrap();
        assert!(!tools.contains("mcp__pal__consensus"), "dark by default");

        cfg.consensus_enabled = true;
        let cmd = build(StageId::PlanReview, &cfg, &dirs, "s", None).unwrap();
        let tools = cmd.argv.iter().find(|a| a.contains("mcp__codex")).unwrap();
        assert!(tools.contains("mcp__pal__consensus"));
    }

    #[test]
    fn plan_review_requires_plan_file() {
        let (_tmp, cfg, dirs) = setup();
        assert!(build(StageId::PlanReview, &cfg, &dirs, "feat-x", None).is_err());
    }

    #[test]
    fn fallback_model_reaches_headless_claude_only() {
        let (_tmp, mut cfg, dirs) = setup();
        std::fs::create_dir_all(dirs.feature_dir("s")).unwrap();
        std::fs::write(dirs.plan_file("s"), "# plan").unwrap();

        // Off by default.
        let cmd = build(StageId::PlanReview, &cfg, &dirs, "s", None).unwrap();
        assert!(!cmd.argv.contains(&"--fallback-model".to_string()));

        cfg.fallback_model = Some("claude-sonnet-5".into());
        let cmd = build(StageId::PlanReview, &cfg, &dirs, "s", None).unwrap();
        let i = cmd
            .argv
            .iter()
            .position(|a| a == "--fallback-model")
            .expect("headless claude gains the flag");
        assert_eq!(cmd.argv[i + 1], "claude-sonnet-5");

        // Interactive stages negotiate with the user — no fallback flag.
        let cmd = build(StageId::TestsRed, &cfg, &dirs, "s", None).unwrap();
        assert!(!cmd.argv.contains(&"--fallback-model".to_string()));

        // Doc-chat is headless claude too.
        let cmd = doc_chat_command(
            &cfg,
            &dirs.spec_file("s"),
            DocKind::Spec,
            &Scope::Whole,
            "msg",
            "",
            None,
            None,
        );
        assert!(cmd.argv.contains(&"--fallback-model".to_string()));
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
    fn doc_chat_command_shape() {
        let (_tmp, mut cfg, dirs) = setup();
        let path = dirs.spec_file("feat-x");
        let cmd = doc_chat_command(
            &cfg,
            &path,
            DocKind::Spec,
            &Scope::Section("Behavior (the contract — WHAT, not HOW)".into()),
            "add a retry invariant",
            "you: earlier thing\nassistant: did it",
            None,
            None,
        );
        assert_eq!(cmd.mode, Mode::Headless);
        assert!(!cmd.needs_codex);
        assert!(
            cmd.env.is_empty(),
            "doc-chat writes no findings, sets no env"
        );
        let prompt = cmd
            .argv
            .iter()
            .find(|a| a.starts_with("/spec"))
            .expect("has a /spec prompt");
        assert!(prompt.contains("DOC_FILE:"));
        assert!(prompt.contains(&path.display().to_string()));
        assert!(prompt.contains("DOC_KIND: spec"));
        assert!(prompt.contains(r#"SCOPE: section "Behavior"#));
        assert!(prompt.contains("add a retry invariant"));
        assert!(prompt.contains("RECENT CONVERSATION"));
        assert!(cmd.argv.contains(&"stream-json".to_string()));
        // Hard scoping: dontAsk mode + Edit/Write rules anchored to THE doc
        // with the '//' filesystem-absolute form; Read stays unrestricted.
        assert!(cmd.argv.contains(&"dontAsk".to_string()));
        assert!(!cmd.argv.contains(&"acceptEdits".to_string()));
        let tools = cmd
            .argv
            .iter()
            .find(|a| a.starts_with("Read,"))
            .expect("allowedTools value");
        assert!(tools.contains(&format!(
            "Edit(//{})",
            path.display().to_string().trim_start_matches('/')
        )));
        assert!(tools.starts_with("Read,Edit(//"));
        assert!(tools.contains("Write(//"));

        // The spec-target prompt never carries SPEC_FILE.
        assert!(!prompt.contains("SPEC_FILE:"));

        // Whole scope + empty context omit the section/context lines; model
        // routing appends --model; plan targets carry SPEC_FILE for drafting.
        cfg.models.insert("plan".into(), "opus".into());
        let spec = dirs.spec_file("feat-x");
        let cmd = doc_chat_command(
            &cfg,
            &path,
            DocKind::Plan,
            &Scope::Whole,
            "tighten step 2",
            "",
            Some(&spec),
            None,
        );
        let prompt = cmd.argv.iter().find(|a| a.starts_with("/spec")).unwrap();
        assert!(prompt.contains("SCOPE: whole"));
        assert!(!prompt.contains("RECENT CONVERSATION"));
        assert!(prompt.contains("DOC_KIND: plan"));
        assert!(prompt.contains(&format!("SPEC_FILE: {}", spec.display())));
        assert!(cmd.argv.windows(2).any(|w| w == ["--model", "opus"]));
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

    #[test]
    fn invariants_env_reaches_review_stages_only_when_meaningful() {
        let (_tmp, cfg, dirs) = setup();
        std::fs::create_dir_all(dirs.feature_dir("s")).unwrap();
        std::fs::write(dirs.plan_file("s"), "# plan").unwrap();
        let has_env =
            |cmd: &StageCommand| cmd.env.iter().any(|(k, _)| k == "RITUAL_INVARIANTS_FILE");

        // Absent file -> no env.
        let cmd = build(StageId::PlanReview, &cfg, &dirs, "s", None).unwrap();
        assert!(!has_env(&cmd));

        // Scaffold template (headings + comments only) -> still no env.
        std::fs::write(dirs.invariants_file(), crate::scaffold::INVARIANTS_TEMPLATE).unwrap();
        let cmd = build(StageId::DualReview, &cfg, &dirs, "s", None).unwrap();
        assert!(!has_env(&cmd));

        // Real bullets -> both review stages carry it; interactive ones don't.
        std::fs::write(dirs.invariants_file(), "# Invariants\n- no panics\n").unwrap();
        for stage in [StageId::PlanReview, StageId::DualReview] {
            let cmd = build(stage, &cfg, &dirs, "s", None).unwrap();
            assert!(
                cmd.env
                    .iter()
                    .any(|(k, v)| k == "RITUAL_INVARIANTS_FILE" && v.ends_with("invariants.md")),
                "{stage:?} must carry the constitution"
            );
        }
        let cmd = build(StageId::TestsRed, &cfg, &dirs, "s", None).unwrap();
        assert!(!has_env(&cmd));
    }

    #[test]
    fn sandbox_wrapper_wraps_headless_only_when_enabled() {
        let (_tmp, mut cfg, _dirs) = setup();
        assert!(
            wrapper_argv(&cfg, Mode::Headless).is_empty(),
            "off by default"
        );

        cfg.sandbox_enabled = true;
        cfg.sandbox_wrapper = vec!["srt".into(), "--settings".into(), "/s.json".into()];
        assert_eq!(
            wrapper_argv(&cfg, Mode::Headless),
            vec!["srt", "--settings", "/s.json"]
        );
        // Interactive stages own the terminal — never wrapped.
        assert!(wrapper_argv(&cfg, Mode::Interactive).is_empty());
        assert!(wrapper_argv(&cfg, Mode::Local).is_empty());

        // Enabled but no wrapper -> nothing prepended (doctor flags this).
        cfg.sandbox_wrapper.clear();
        assert!(wrapper_argv(&cfg, Mode::Headless).is_empty());
    }

    #[test]
    fn doc_chat_prompt_carries_invariants_when_present() {
        let (_tmp, cfg, dirs) = setup();
        let inv = dirs.invariants_file();
        let cmd = doc_chat_command(
            &cfg,
            &dirs.spec_file("s"),
            DocKind::Spec,
            &Scope::Whole,
            "m",
            "",
            None,
            Some(&inv),
        );
        let prompt = cmd.argv.iter().find(|a| a.starts_with("/spec")).unwrap();
        assert!(prompt.contains(&format!("INVARIANTS_FILE: {}", inv.display())));
    }
}
