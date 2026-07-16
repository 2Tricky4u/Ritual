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
    /// Prompt payload piped to the agent's stdin instead of argv. Anything
    /// unbounded (an embedded diff) MUST travel here: a single oversized argv
    /// element kills the exec with E2BIG ("Argument list too long").
    pub stdin: Option<String>,
}

const PLAN_REVIEW_TOOLS: &str =
    "Read Glob Grep Edit Write Bash(git *) mcp__codex__codex mcp__codex__codex-reply";
const DUAL_REVIEW_TOOLS: &str =
    "Task Read Glob Grep Edit Write Bash mcp__codex__codex mcp__codex__codex-reply";
/// The code-fix batch edits source broadly and runs check.sh, so it needs the
/// full edit + shell grant (like dual-review, minus codex; plus MultiEdit).
const CODE_FIX_TOOLS: &str = "Task Read Glob Grep Edit Write MultiEdit Bash";
/// Hard denials layered over the fixer's Bash grant: the prompt already
/// forbids these, but only HEAD movement was mechanically detected - a
/// `git push`/`git clean` that leaves HEAD in place was prompt-discipline
/// only. Denials beat allows in the permission engine; probe-verified on
/// CLI 2.1.205 (a bare `Bash` allow + this exact space-separated list:
/// `git commit` denied, `git log` allowed). The plan-fix needs no list -
/// its `doc_chat_tools` grant excludes Bash, though the CLI's built-in
/// SAFE read-only command list (echo, git log/status...) still executes
/// under dontAsk even with no Bash grant at all (probe-verified).
const CODE_FIX_DISALLOWED_TOOLS: &str =
    "Bash(git push:*) Bash(git commit:*) Bash(git reset:*) Bash(git rebase:*) Bash(git clean:*)";
/// The re-review is strictly READ-ONLY with NO shell: it inspects the change
/// (handed to it inline) and reads code for context, but cannot edit OR run git,
/// so it can neither paper over its own verdict nor mutate the tree. Dropping
/// the old `Bash(git *)` grant also sidesteps the risk that the internal space
/// shatters when the CLI splits `--allowedTools`; the reviewer never needs git
/// because the full diff (incl. untracked edits) is embedded in the prompt.
const CODE_REVIEW_TOOLS: &str = "Read Glob Grep";

/// plan-review's tool grant, plus the third-model consensus tool when the
/// (dark-by-default) escalation tier is enabled; the skill only escalates
/// genuinely contested findings, and only when the pal MCP server exists.
fn plan_review_tools(cfg: &Config) -> String {
    if cfg.consensus_enabled {
        format!("{PLAN_REVIEW_TOOLS} mcp__pal__consensus")
    } else {
        PLAN_REVIEW_TOOLS.to_string()
    }
}
/// The doc-chat agent's grant: Read anything, Edit/Write for the one document
/// it targets. The rules USED to be path-scoped (`Edit(//abs/doc)`), but under
/// `--permission-mode dontAsk` the current CLI (verified empirically on
/// 2.1.205) never matches a path-scoped Edit/Write rule - every scoped edit is
/// denied and the stage dies doing nothing. So the file tools are granted
/// BARE. dontAsk denies other UN-granted tools (no unsafe Bash, no Task),
/// but the CLI auto-allows its built-in SAFE read-only Bash list (echo,
/// git log/status...) regardless of grants - verified on 2.1.205. Document
/// scoping is therefore enforced ritual-side: the TUI batch path runs the
/// section-confinement gate (`spec::edits_confined_multi`), the headless
/// complete loop runs a git containment gate + undo revert, and the
/// coverage/plan-review re-judges catch in-document drift.
fn doc_chat_tools(_doc_path: &Path) -> String {
    "Read,Edit,Write".to_string()
}

/// The coverage judge may READ anything (to inspect the built tree) and WRITE
/// its one findings JSON - no shell, no Task. Write is granted BARE for the
/// same reason as [`doc_chat_tools`]: dontAsk never matches path-scoped
/// Write rules (verified on CLI 2.1.205; the old `Write(//findings/**)` rule
/// denied the findings file and the whole coverage verdict was lost).
fn coverage_tools(_findings_dir: &Path) -> String {
    "Read,Glob,Grep,Write".to_string()
}

/// Sandbox argv prefix for a run: only headless runs are wrapped (interactive
/// stages own the user's terminal; bubblewrap-style isolation would break
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
/// conversation" block (may be empty). Everything (paths, scope, message)
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
            "INVARIANTS_FILE: {} (non-negotiable constraints; never write content that contradicts them)\n",
            p.display()
        ),
        None => String::new(),
    };
    let ctx_block = if context.trim().is_empty() {
        String::new()
    } else {
        format!("\n\nRECENT CONVERSATION (context only; do NOT re-apply):\n{context}")
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
                // instantly (no headless hang); the doc is the only
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
        stdin: None,
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

/// The slice of a plan-review finding that rides in a fix prompt.
pub struct FindingBrief<'a> {
    pub title: &'a str,
    pub severity: &'a str,
    pub scenario: &'a str,
    pub plan_step: &'a str,
    pub snippet: Option<&'a str>,
}

/// Build the headless command for the BATCH plan fix: all queued findings in
/// one run against one plan snapshot (so no fix can rot the next finding's
/// anchor). Same tool lock and routing as `finding_fix_command`; the REQUEST
/// carries every finding numbered plus the ANSWERS contract the caller
/// parses back per finding (`crate::answers::parse_answers`). `sections` is
/// prompt-level scoping only - the caller enforces the union mechanically
/// (`spec::edits_confined_multi`). Budget: `budget_finding_fix_usd` caps the
/// whole RUN, not each finding.
pub fn findings_batch_fix_command(
    cfg: &Config,
    plan_path: &Path,
    sections: &[&str],
    briefs: &[(u32, FindingBrief)],
    spec_path: Option<&Path>,
    invariants: Option<&Path>,
) -> StageCommand {
    let scope_line = if sections.is_empty() {
        "SCOPE: whole".to_string()
    } else {
        format!(
            "SCOPE: sections {}",
            sections
                .iter()
                .map(|s| format!("\"{s}\""))
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    let spec_line = match spec_path {
        Some(p) => format!("SPEC_FILE: {}\n", p.display()),
        None => String::new(),
    };
    let inv_line = match invariants {
        Some(p) => format!(
            "INVARIANTS_FILE: {} (non-negotiable constraints; never write content that contradicts them)\n",
            p.display()
        ),
        None => String::new(),
    };
    let mut findings_block = String::new();
    for (n, f) in briefs {
        let snippet = match f.snippet {
            Some(s) => format!("snippet:\n{s}\n"),
            None => String::new(),
        };
        findings_block.push_str(&format!(
            "FINDING #{n}:\n\
             severity: {}\n\
             title: {}\n\
             plan step: {}\n\
             scenario: {}\n\
             {snippet}\n",
            f.severity, f.title, f.plan_step, f.scenario,
        ));
    }
    let prompt = format!(
        "/spec Apply one scoped change to this ritual document.\n\n\
         DOC_FILE: {}\n\
         DOC_KIND: plan\n\
         {spec_line}{inv_line}{scope_line}\n\n\
         {findings_block}\
         REQUEST:\n\
         Fix the plan so these findings no longer apply. They may interact - \
         resolve them coherently in ONE pass. Read the whole plan, the spec, \
         and the invariants for consistency, but EDIT ONLY the scoped \
         sections. For any finding whose correct fix requires changes outside \
         those sections, make NO edit for that finding's concern and decline \
         it instead (you may still fix the others). END your final message \
         with exactly this block, one line per finding, every number present:\n\
         ANSWERS:\n\
         #<n>: FIXED\n\
         #<n>: DECLINED <one-line reason>",
        plan_path.display(),
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
                "--permission-mode".into(),
                "dontAsk".into(),
                "--allowedTools".into(),
                doc_chat_tools(plan_path),
                "--max-budget-usd".into(),
                cfg.budget_finding_fix_usd.to_string(),
            ],
        ]
        .concat(),
        env: vec![],
        needs_codex: false,
        stdin: None,
    };
    if let Some(model) = cfg.models.get(DocKind::Plan.label()) {
        cmd.argv.push("--model".into());
        cmd.argv.push(model.clone());
    }
    if let Some(effort) = cfg.effort.get("plan-fix") {
        cmd.argv.push("--effort".into());
        cmd.argv.push(effort.clone());
    }
    if let Some(fb) = &cfg.fallback_model {
        cmd.argv.push("--fallback-model".into());
        cmd.argv.push(fb.clone());
    }
    cmd
}

/// The slice of a dual-review CODE finding that rides in a code-fix prompt.
pub struct CodeFindingBrief<'a> {
    pub title: &'a str,
    pub severity: &'a str,
    pub scenario: &'a str,
    pub file: &'a str,
    pub line: Option<u32>,
    pub snippet: Option<&'a str>,
}

impl CodeFindingBrief<'_> {
    fn location(&self) -> String {
        match self.line {
            Some(l) => format!("{}:{l}", self.file),
            None => self.file.to_string(),
        }
    }
}

fn code_findings_block(briefs: &[(u32, CodeFindingBrief)]) -> String {
    let mut block = String::new();
    for (n, f) in briefs {
        let snippet = match f.snippet {
            Some(s) => format!("snippet:\n{s}\n"),
            None => String::new(),
        };
        block.push_str(&format!(
            "FINDING #{n}:\n\
             severity: {}\n\
             title: {}\n\
             location: {}\n\
             scenario: {}\n\
             {snippet}\n",
            f.severity,
            f.title,
            f.location(),
            f.scenario,
        ));
    }
    block
}

/// Build the headless command for the BATCH code fix: an LLM edits source to
/// resolve all queued CODE findings in ONE pass, then the caller verifies with
/// `./check.sh` + an independent re-review (a passing fix is left in the
/// worktree for git; a failing one is auto-reverted). Broad edit grant (unlike
/// the plan-fix, which is locked to plan.md). `budget_code_fix_usd` caps the
/// whole RUN. Same ANSWERS contract (`crate::answers::parse_answers`).
pub fn findings_code_fix_command(
    cfg: &Config,
    briefs: &[(u32, CodeFindingBrief)],
    invariants: Option<&Path>,
) -> StageCommand {
    let inv_line = match invariants {
        Some(p) => format!(
            "INVARIANTS_FILE: {} (non-negotiable constraints; never write code that contradicts them)\n",
            p.display()
        ),
        None => String::new(),
    };
    let prompt = format!(
        "Fix these code review findings in the current repository.\n\n\
         {inv_line}\
         {}\
         REQUEST:\n\
         Fix the code so these findings no longer apply. They may interact - \
         resolve them coherently in ONE pass. Read broadly for global context \
         and integration; make the MINIMAL changes needed. Do NOT commit, push, \
         reset, rebase, or run any destructive command (no `rm -rf`, no `git \
         clean`): ritual verifies HEAD did not move and FAILS the whole batch \
         if you commit or reset. Run `./check.sh` yourself and make it pass before finishing. \
         For any finding you cannot fix cleanly, make NO edit for it and decline \
         it instead (still fix the others). END your final message with exactly \
         this block, one line per finding, every number present:\n\
         ANSWERS:\n\
         #<n>: FIXED\n\
         #<n>: DECLINED <one-line reason>",
        code_findings_block(briefs),
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
                "--permission-mode".into(),
                "acceptEdits".into(),
                "--allowedTools".into(),
                CODE_FIX_TOOLS.into(),
                "--disallowedTools".into(),
                CODE_FIX_DISALLOWED_TOOLS.into(),
                "--max-budget-usd".into(),
                cfg.budget_code_fix_usd.to_string(),
            ],
        ]
        .concat(),
        env: vec![],
        needs_codex: false,
        stdin: None,
    };
    if let Some(model) = cfg.models.get("code") {
        cmd.argv.push("--model".into());
        cmd.argv.push(model.clone());
    }
    if let Some(effort) = cfg.effort.get("code-fix") {
        cmd.argv.push("--effort".into());
        cmd.argv.push(effort.clone());
    }
    if let Some(fb) = &cfg.fallback_model {
        cmd.argv.push("--fallback-model".into());
        cmd.argv.push(fb.clone());
    }
    cmd
}

/// Build the headless READ-ONLY re-review of a code fix: given the fix's diff
/// and the findings it targeted, an independent agent confirms each finding is
/// actually resolved and that nothing new broke. Verdict parsed by
/// `crate::review::parse_review`.
pub fn code_fix_review_command(
    cfg: &Config,
    diff: &str,
    briefs: &[(u32, CodeFindingBrief)],
) -> StageCommand {
    let prompt = format!(
        "Review this code change for correctness and completeness.\n\n\
         {}\
         The following diff was made to resolve the findings above:\n\n\
         ```diff\n{diff}\n```\n\n\
         REQUEST:\n\
         For EACH finding, decide from the diff (read the surrounding code if \
         needed - you are READ-ONLY, do not edit) whether it is genuinely \
         resolved. Then judge whether the change introduces any regression, \
         breakage, or new problem in the wider codebase. END your final message \
         with exactly this block, every finding number present:\n\
         REVIEW:\n\
         #<n>: RESOLVED\n\
         #<n>: UNRESOLVED <one-line reason>\n\
         REGRESSIONS: NONE\n\
         (or) REGRESSIONS: <one-line description of what breaks>",
        code_findings_block(briefs),
    );
    // The prompt embeds the FULL diff (unbounded), so it rides on stdin: a
    // big change set in argv dies at exec with E2BIG ("Argument list too
    // long") and the daemon vanishes before writing meta. `-p` with no
    // positional prompt reads the prompt from stdin.
    let mut cmd = StageCommand {
        mode: Mode::Headless,
        agent: AgentKind::Claude,
        argv: [
            cfg.claude_cmd.clone(),
            vec![
                "-p".into(),
                "--output-format".into(),
                "stream-json".into(),
                "--verbose".into(),
                "--permission-mode".into(),
                "dontAsk".into(),
                "--allowedTools".into(),
                CODE_REVIEW_TOOLS.into(),
                "--max-budget-usd".into(),
                cfg.budget_code_fix_usd.to_string(),
            ],
        ]
        .concat(),
        env: vec![],
        needs_codex: false,
        stdin: Some(prompt),
    };
    if let Some(model) = cfg.models.get("code") {
        cmd.argv.push("--model".into());
        cmd.argv.push(model.clone());
    }
    if let Some(fb) = &cfg.fallback_model {
        cmd.argv.push("--fallback-model".into());
        cmd.argv.push(fb.clone());
    }
    cmd
}

/// Shared headless-flag tail for the audit legs: routing key, then the
/// standard stream/permission/budget plumbing.
fn audit_headless_argv(
    cfg: &Config,
    prompt: Option<String>,
    tools: &str,
    model_key: &str,
) -> Vec<String> {
    let mut argv = cfg.claude_cmd.clone();
    argv.push("-p".into());
    if let Some(p) = prompt {
        argv.push(p);
    }
    argv.extend([
        "--output-format".into(),
        "stream-json".into(),
        "--verbose".into(),
        "--permission-mode".into(),
        "dontAsk".into(),
        "--allowedTools".into(),
        tools.into(),
    ]);
    argv.push("--max-budget-usd".into());
    argv.push(cfg.budget_audit_usd.to_string());
    if let Some(model) = cfg.models.get(model_key) {
        argv.push("--model".into());
        argv.push(model.clone());
    }
    if let Some(fb) = &cfg.fallback_model {
        argv.push("--fallback-model".into());
        argv.push(fb.clone());
    }
    argv
}

/// Build the audit DISCOVERY run: enumerate the project's distinct
/// flows/techs/paths and write them to the (user-editable) lanes file.
/// Read-only + bare Write under dontAsk (path-scoped rules never match
/// there, see [`doc_chat_tools`]).
pub fn audit_discover_command(cfg: &Config, lanes_path: &Path) -> StageCommand {
    let prompt = format!(
        "Survey this repository and enumerate its DISTINCT flows, technologies, \
         and end-to-end paths (e.g. a parser pipeline, a process runner, a UI \
         layer, persistence, external integrations). These become independent \
         review lanes for a whole-project audit.\n\n\
         Write AT MOST {} lanes to {} as markdown: one `## <short-lane-name>` \
         heading per lane, followed by 1-3 plain lines describing exactly what \
         that lane covers (its files/modules and its responsibilities). Prefer \
         fewer, well-separated lanes over many overlapping ones. Do not include \
         a `global-overview` lane - ritual adds that one itself. Do not edit \
         anything else.",
        cfg.audit_max_lanes.saturating_sub(1).max(1),
        lanes_path.display(),
    );
    StageCommand {
        mode: Mode::Headless,
        agent: AgentKind::Claude,
        argv: audit_headless_argv(cfg, Some(prompt), "Read,Glob,Grep,Write", "audit"),
        env: vec![],
        needs_codex: false,
        stdin: None,
    }
}

/// Build ONE audit lane run: a focused, BLIND review of a single flow. The
/// lane sees the NAMES of the other lanes (so it can reason about interaction
/// contracts) but never their content or findings - independent parallel
/// reviewers with decorrelated blind spots, same rationale as dual-review.
pub fn audit_lane_command(
    cfg: &Config,
    lane: &crate::audit::Lane,
    other_lane_names: &[&str],
    report_path: &Path,
    invariants: Option<&Path>,
) -> StageCommand {
    let inv_line = match invariants {
        Some(p) => format!(
            "INVARIANTS_FILE: {} (non-negotiable constraints; any violation in this flow is a finding)\n",
            p.display()
        ),
        None => String::new(),
    };
    let prompt = format!(
        "You are ONE lane of a whole-project audit. Audit ONLY this flow:\n\n\
         LANE: {}\n\
         SCOPE: {}\n\
         {inv_line}\
         OTHER LANES (names only - audit their CONTRACTS with your flow, not \
         their internals): {}\n\n\
         REQUEST:\n\
         Examine this flow's internal correctness AND every contract it has \
         with the other lanes and the global architecture (data handed across, \
         ordering/lifecycle assumptions, error propagation, docs vs behavior). \
         You are READ-ONLY on the codebase. For EACH candidate finding record: \
         a one-line title, file:line, a 1-3 line verbatim snippet, a concrete \
         failure scenario (inputs/state -> wrong outcome), severity \
         (critical/major/minor), and an evidence grade - `reproduced` (you \
         traced concrete inputs through the code to the bad outcome), `traced` \
         (the defective path is real but you did not follow a full scenario), \
         or `suspected` (plausible, unverified). Report real defects only, no \
         style nits. Write your full report as markdown to {} (create parent \
         dirs if needed) - the report file is your ONLY output artifact.",
        lane.name,
        if lane.description.is_empty() {
            "(the lane name is the scope)"
        } else {
            &lane.description
        },
        other_lane_names.join(", "),
        report_path.display(),
    );
    StageCommand {
        mode: Mode::Headless,
        agent: AgentKind::Claude,
        argv: audit_headless_argv(cfg, Some(prompt), "Read,Glob,Grep,Write", "audit"),
        env: vec![],
        needs_codex: false,
        stdin: None,
    }
}

/// Build the audit JUDGE run: adversarial adjudication of every lane
/// candidate. The concatenated lane reports ride on STDIN (unbounded; argv
/// dies at E2BIG). Confirmation requires refutation-resistant evidence or an
/// independent cross-vendor (Codex) verdict - the judge must never rubber-
/// stamp its own vendor's candidates (self-preference bias).
pub fn audit_judge_command(
    cfg: &Config,
    findings_dir: &Path,
    lane_count: usize,
    reports_payload: String,
) -> StageCommand {
    let prompt = "Adjudicate the whole-project audit reports arriving below. They were \
         written by independent, blind review lanes and OVER-REPORT by design.\n\n\
         For EACH candidate finding, in this order:\n\
         1. Actively try to REFUTE it: read the code at the cited location, run \
         commands/tests where that settles it. Discard refuted and duplicate \
         candidates (same defect reported by several lanes = ONE finding).\n\
         2. For each survivor, obtain an INDEPENDENT verdict from the `codex` \
         MCP tool: hand it the candidate's title, location, snippet, and \
         scenario - NEVER your own judgment - and ask it to verify or refute \
         against the code. If the codex tool fails because the MODEL is \
         unavailable (model-not-found - NOT an auth error), retry ONCE with \
         model \"gpt-5.5\" and note the downgrade; if codex is entirely \
         unavailable, say so per finding and grade on your evidence alone.\n\
         3. A finding is `confirmed` ONLY when you reproduced/traced it AND it \
         survived refutation, OR codex independently agrees. Everything else \
         is `unconfirmed`.\n\n\
         Then write ONE findings file to FINDINGS_DIR below as `<UTC \
         yyyymmddTHHMMSSZ>-audit.json` (never modify an existing file):\n\
         {\"ritual_findings\": 1, \"stage\": \"audit\", \"branch\": \"<git \
         branch --show-current, or empty>\", \"generated_at\": \"<ISO8601 \
         UTC>\", \"findings\": [{\"id\": 1, \"severity\": \
         \"critical|major|minor\", \"title\": \"<one sentence, <80 chars>\", \
         \"file\": \"src/foo.rs\", \"line\": 42, \"plan_step\": null, \
         \"snippet\": \"<1-3 verbatim source lines>\", \"scenario\": \
         \"<inputs/state -> wrong outcome>\", \"sources\": [\"<lane name(s)>\"], \
         \"verdict\": \"confirmed|unconfirmed\", \"action\": \"pending\"}]}\n\
         `file`+`line` must point at the exact defective line; `snippet` is \
         verbatim. An empty findings list is valid. END with a short human \
         table: finding | lane(s) | evidence | verdict.\n\n\
         LANE REPORTS FOLLOW:"
        .to_string();
    // The ABSOLUTE findings dir rides in the prompt (also exported as
    // RITUAL_FINDINGS_DIR by the caller): a no-shell agent cannot expand the
    // env idiom, and its relative fallback lands in the WRONG .ritual from a
    // linked worktree.
    let prompt = format!("{prompt}\n\nFINDINGS_DIR: {}", findings_dir.display());
    // The prompt itself is bounded, but the reports are not: prompt AND
    // payload both travel on stdin (`-p` with no positional reads stdin).
    let payload = format!("{prompt}\n\n{reports_payload}");
    let mut argv = audit_headless_argv(
        cfg,
        None,
        "Read,Glob,Grep,Bash,Write mcp__codex__codex mcp__codex__codex-reply",
        "audit-judge",
    );
    // Same git guardrail as the code fixer: Bash is needed to reproduce, but
    // history/remote mutation stays hard-denied.
    argv.push("--disallowedTools".into());
    argv.push(CODE_FIX_DISALLOWED_TOOLS.into());
    // The judge adjudicates EVERY lane's candidates (read code, run repros,
    // one codex verdict per survivor), so a single per-leg cap starves it.
    // Live 8-lane data: $3 cap died at $4.12 with work half done; a $15 cap
    // ($3 x (1+8/2)) died at $17.53 with the findings WRITTEN but the final
    // turn cut. One cap-unit per lane plus one spare clears the workload -
    // it is a CEILING, not a spend.
    let judge_cap = cfg.budget_audit_usd * (1.0 + lane_count as f64);
    if let Some(i) = argv.iter().position(|a| a == "--max-budget-usd") {
        argv[i + 1] = judge_cap.to_string();
    }
    StageCommand {
        mode: Mode::Headless,
        agent: AgentKind::Claude,
        argv,
        env: vec![],
        needs_codex: true,
        stdin: Some(payload),
    }
}

/// Build the exact command for a stage. `arg` is the optional user argument
/// (plan path for plan-review, base ref for dual-review). `model_override`
/// (retry-with-model, `run --model`) beats the `[models]` routing table.
/// Suggested first message for `implement`, shown in the launch overlay for
/// the user to copy/paste - an interactive `claude --resume` can't be handed a
/// prompt, so ritual surfaces it instead of auto-sending it.
pub const IMPLEMENT_PROMPT: &str = "Implement the code to make the failing tests from the tests-red step pass. Follow the plan and run ./check.sh before finishing.";

#[allow(clippy::too_many_arguments)] // one optional knob per stage concern; a params struct would just rename them
pub fn build(
    stage: StageId,
    cfg: &Config,
    dirs: &RitualDirs,
    slug: &str,
    arg: Option<&str>,
    model_override: Option<&str>,
    // The claude session to pin/resume. For `tests-red` this becomes
    // `--session-id <sid>` (ritual owns the id); for `implement`, `--resume
    // <sid>`. `None` on implement opens the `--resume` picker so the user
    // chooses - never the fragile `--continue` ("most recent in cwd").
    session: Option<&str>,
    // The checkout the run will execute in - the TUI passes its (possibly
    // linked-worktree) run cwd so git preflights probe the RIGHT tree;
    // `None` falls back to `dirs.work_root` (CLI / bench).
    run_cwd: Option<&Path>,
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
            stdin: None,
        },
        StageId::Plan => {
            let spec = dirs.spec_file(slug);
            let plan = dirs.plan_file(slug);
            let prompt = format!(
                "Read {} and plan the implementation. Include a `## Deliverables` \
                 checklist - one item per concrete deliverable, each \
                 `- [ ] <ID>: <description> - accept: <measurable pass/fail criterion> \
                 - route: <path or §Section>` (stable ids like D1) - so completeness \
                 can be verified against the built tree. When the plan is approved, \
                 save it to {} before finishing.",
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
                stdin: None,
            }
        }
        StageId::PlanReview => {
            let plan = arg
                .map(str::to_string)
                .unwrap_or_else(|| dirs.plan_file(slug).display().to_string());
            anyhow::ensure!(
                std::path::Path::new(&plan).exists(),
                "plan file not found: {plan}; run the plan stage first (or pass a path)"
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
                stdin: None,
            }
        }
        StageId::TestsRed => {
            let plan = dirs.plan_file(slug);
            // Pin the conversation to a ritual-owned id so `implement` can
            // resume this exact session later (flags precede the prompt).
            let mut tail: Vec<String> = Vec::new();
            if let Some(sid) = session {
                tail.push("--session-id".into());
                tail.push(sid.to_string());
            }
            // red-only: the skill runs the full red->green loop by default,
            // but this stage must STOP at failing tests - implementation
            // belongs to the `implement` stage, which resumes this session.
            tail.push(format!("/tdd {} red-only", plan.display()));
            StageCommand {
                mode: Mode::Interactive,
                agent: AgentKind::Claude,
                argv: [claude, tail].concat(),
                env: vec![],
                needs_codex: true,
                stdin: None,
            }
        }
        StageId::Implement => {
            // Resume the exact tests-red conversation (by id, or the picker
            // when unpinned) and let the user drive: an interactive
            // `claude --resume` IGNORES any positional prompt (that only works
            // with `--print`), and a token after a bare `--resume` would be
            // taken as the picker's search term. So never a prompt here, and
            // never `--continue`.
            let mut tail: Vec<String> = vec!["--resume".into()];
            if let Some(sid) = session {
                tail.push(sid.to_string());
            }
            StageCommand {
                mode: Mode::Interactive,
                agent: AgentKind::Claude,
                argv: [claude, tail].concat(),
                env: vec![],
                needs_codex: false,
                stdin: None,
            }
        }
        StageId::DualReview => {
            let base = arg
                .map(str::to_string)
                .unwrap_or_else(|| cfg.base_ref.clone());
            // Refuse a provably vacuous review BEFORE spending budget: a
            // missing base or a tree identical to the merge-base used to
            // produce an empty diff, findings [], and a Done stage that had
            // reviewed zero lines. Probes the run's OWN checkout.
            crate::git::dual_review_preflight(run_cwd.unwrap_or(&dirs.work_root), &base)?;
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
                stdin: None,
            }
        }
        StageId::Coverage => {
            let plan = dirs.plan_file(slug);
            anyhow::ensure!(
                std::path::Path::new(&plan).exists(),
                "plan file not found: {}; run the plan stage first",
                plan.display()
            );
            StageCommand {
                mode: Mode::Headless,
                agent: AgentKind::Claude,
                argv: [
                    claude,
                    vec![
                        "-p".into(),
                        // The ABSOLUTE findings dir rides in the prompt (env
                        // is also set): a no-shell agent can't expand the
                        // skill's ${RITUAL_FINDINGS_DIR} idiom, and its
                        // relative fallback lands in the WRONG .ritual from
                        // a linked worktree.
                        format!(
                            "/coverage {}\nFINDINGS_DIR (absolute; write the findings JSON here): {}",
                            plan.display(),
                            dirs.findings_dir().display()
                        ),
                        "--output-format".into(),
                        "stream-json".into(),
                        "--verbose".into(),
                        "--permission-mode".into(),
                        "dontAsk".into(),
                        "--allowedTools".into(),
                        coverage_tools(&dirs.findings_dir()),
                        "--max-budget-usd".into(),
                        cfg.budget_coverage_usd.to_string(),
                    ],
                ]
                .concat(),
                env: vec![findings_env],
                needs_codex: false,
                stdin: None,
            }
        }
    };
    // Per-stage model routing: an explicit override (retry-with-model,
    // `run --model`) beats the [models] config table.
    let model = model_override
        .map(str::to_string)
        .or_else(|| cfg.models.get(stage.label()).cloned());
    if let Some(model) = model
        && !cmd.argv.is_empty()
    {
        cmd.argv.push("--model".into());
        cmd.argv.push(model);
    }
    // Per-stage reasoning effort ([effort] table -> claude --effort <level>).
    // Local stages (spec) have no CLI to carry the flag.
    if let Some(effort) = cfg.effort.get(stage.label())
        && !cmd.argv.is_empty()
    {
        cmd.argv.push("--effort".into());
        cmd.argv.push(effort.clone());
    }
    // Overload resilience: retry on a fallback model instead of dying to a 529
    // mid-run. Headless reviews always opt in (no human to negotiate); the
    // interactive `plan` stage opts in too, so a pinned planning model (e.g.
    // Fable 5) has an Opus safety net. Other interactive stages and codex don't.
    if let Some(fb) = &cfg.fallback_model
        && cmd.agent == AgentKind::Claude
        && (cmd.mode == Mode::Headless || stage == StageId::Plan)
    {
        cmd.argv.push("--fallback-model".into());
        cmd.argv.push(fb.clone());
    }
    // Both review stages enforce the constitution (skills fall back to the
    // well-known path for interactive stages like tests-red).
    if matches!(
        stage,
        StageId::PlanReview | StageId::DualReview | StageId::Coverage
    ) && let Some(p) = meaningful_invariants(dirs)
    {
        cmd.env
            .push(("RITUAL_INVARIANTS_FILE".into(), p.display().to_string()));
    }
    Ok(cmd)
}

/// `offline = true` blocks every agent spawn (metered/plane mode) - the
/// promise the guide has always made. The auth-preflight skips elsewhere
/// stay: preflights are moot once runs are blocked.
pub fn ensure_online(cfg: &Config) -> Result<()> {
    anyhow::ensure!(
        !cfg.offline,
        "offline = true blocks agent runs (settings: S -> offline, or [offline] in .ritual/config.toml)"
    );
    Ok(())
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

    /// Like `setup`, but inside a git repo on branch `main` with one commit
    /// and a dirty tracked file - the dual-review preflight needs a real,
    /// reviewable checkout.
    fn setup_git() -> (tempfile::TempDir, Config, RitualDirs) {
        let (tmp, cfg, dirs) = setup();
        let p = tmp.path();
        for args in [
            &["init", "-q", "-b", "main"][..],
            &["config", "user.email", "t@t"][..],
            &["config", "user.name", "t"][..],
        ] {
            std::process::Command::new("git")
                .args(args)
                .current_dir(p)
                .output()
                .unwrap();
        }
        std::fs::write(p.join("a.rs"), "one\n").unwrap();
        for args in [&["add", "a.rs"][..], &["commit", "-qm", "x"][..]] {
            std::process::Command::new("git")
                .args(args)
                .current_dir(p)
                .output()
                .unwrap();
        }
        std::fs::write(p.join("a.rs"), "two\n").unwrap(); // reviewable dirt
        (tmp, cfg, dirs)
    }

    #[test]
    fn plan_review_command_shape() {
        let (_tmp, cfg, dirs) = setup();
        std::fs::create_dir_all(dirs.feature_dir("feat-x")).unwrap();
        std::fs::write(dirs.plan_file("feat-x"), "# plan").unwrap();
        let cmd = build(
            StageId::PlanReview,
            &cfg,
            &dirs,
            "feat-x",
            None,
            None,
            None,
            None,
        )
        .unwrap();
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

        let cmd = build(
            StageId::PlanReview,
            &cfg,
            &dirs,
            "s",
            None,
            None,
            None,
            None,
        )
        .unwrap();
        let tools = cmd.argv.iter().find(|a| a.contains("mcp__codex")).unwrap();
        assert!(!tools.contains("mcp__pal__consensus"), "dark by default");

        cfg.consensus_enabled = true;
        let cmd = build(
            StageId::PlanReview,
            &cfg,
            &dirs,
            "s",
            None,
            None,
            None,
            None,
        )
        .unwrap();
        let tools = cmd.argv.iter().find(|a| a.contains("mcp__codex")).unwrap();
        assert!(tools.contains("mcp__pal__consensus"));
    }

    #[test]
    fn plan_review_requires_plan_file() {
        let (_tmp, cfg, dirs) = setup();
        assert!(
            build(
                StageId::PlanReview,
                &cfg,
                &dirs,
                "feat-x",
                None,
                None,
                None,
                None
            )
            .is_err()
        );
    }

    #[test]
    fn fallback_model_reaches_headless_and_interactive_plan() {
        let (_tmp, mut cfg, dirs) = setup();
        std::fs::create_dir_all(dirs.feature_dir("s")).unwrap();
        std::fs::write(dirs.plan_file("s"), "# plan").unwrap();

        // Off by default.
        let cmd = build(
            StageId::PlanReview,
            &cfg,
            &dirs,
            "s",
            None,
            None,
            None,
            None,
        )
        .unwrap();
        assert!(!cmd.argv.contains(&"--fallback-model".to_string()));

        cfg.fallback_model = Some("claude-sonnet-5".into());
        let cmd = build(
            StageId::PlanReview,
            &cfg,
            &dirs,
            "s",
            None,
            None,
            None,
            None,
        )
        .unwrap();
        let i = cmd
            .argv
            .iter()
            .position(|a| a == "--fallback-model")
            .expect("headless claude gains the flag");
        assert_eq!(cmd.argv[i + 1], "claude-sonnet-5");

        // The interactive `plan` stage opts in: a pinned planning model needs
        // its Opus safety net too.
        let cmd = build(StageId::Plan, &cfg, &dirs, "s", None, None, None, None).unwrap();
        let i = cmd
            .argv
            .iter()
            .position(|a| a == "--fallback-model")
            .expect("interactive plan gains the flag");
        assert_eq!(cmd.argv[i + 1], "claude-sonnet-5");

        // Other interactive stages negotiate with the user; no fallback flag.
        let cmd = build(StageId::TestsRed, &cfg, &dirs, "s", None, None, None, None).unwrap();
        assert!(!cmd.argv.contains(&"--fallback-model".to_string()));
        let cmd = build(StageId::Implement, &cfg, &dirs, "s", None, None, None, None).unwrap();
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
    fn effort_routes_to_cli_stages_and_skips_local_spec() {
        let (_tmp, mut cfg, dirs) = setup();
        std::fs::create_dir_all(dirs.feature_dir("s")).unwrap();
        std::fs::write(dirs.plan_file("s"), "# plan").unwrap();
        cfg.effort.insert("plan".into(), "xhigh".into());

        // Pinned stage carries `--effort xhigh`.
        let cmd = build(StageId::Plan, &cfg, &dirs, "s", None, None, None, None).unwrap();
        let i = cmd
            .argv
            .iter()
            .position(|a| a == "--effort")
            .expect("plan gains --effort");
        assert_eq!(cmd.argv[i + 1], "xhigh");

        // A stage with no [effort] entry stays at the session default.
        let cmd = build(
            StageId::PlanReview,
            &cfg,
            &dirs,
            "s",
            None,
            None,
            None,
            None,
        )
        .unwrap();
        assert!(!cmd.argv.contains(&"--effort".to_string()));

        // Local `spec` has no CLI (empty argv) - never gains the flag even if set.
        cfg.effort.insert("spec".into(), "xhigh".into());
        let cmd = build(StageId::Spec, &cfg, &dirs, "s", None, None, None, None).unwrap();
        assert!(cmd.argv.is_empty());
    }

    #[test]
    fn dual_review_uses_base_ref() {
        let (_tmp, cfg, dirs) = setup_git();
        let cmd = build(
            StageId::DualReview,
            &cfg,
            &dirs,
            "s",
            None,
            None,
            None,
            None,
        )
        .unwrap();
        assert!(cmd.argv.contains(&"/dual-review main".to_string()));
        // An explicit base must exist (the preflight verifies it).
        std::process::Command::new("git")
            .args(["branch", "develop"])
            .current_dir(_tmp.path())
            .output()
            .unwrap();
        let cmd = build(
            StageId::DualReview,
            &cfg,
            &dirs,
            "s",
            Some("develop"),
            None,
            None,
            None,
        )
        .unwrap();
        assert!(cmd.argv.contains(&"/dual-review develop".to_string()));
    }

    #[test]
    fn dual_review_preflight_gates_vacuous_and_broken_runs() {
        // Dirty tracked file -> reviewable.
        let (tmp, cfg, dirs) = setup_git();
        assert!(
            build(
                StageId::DualReview,
                &cfg,
                &dirs,
                "s",
                None,
                None,
                None,
                None
            )
            .is_ok()
        );

        // Missing base ref -> a clear error, not a vacuous review.
        let err = build(
            StageId::DualReview,
            &cfg,
            &dirs,
            "s",
            Some("no-such-ref"),
            None,
            None,
            None,
        )
        .unwrap_err();
        assert!(format!("{err:#}").contains("base ref"), "{err:#}");

        // Clean tree identical to the merge-base -> "nothing to review".
        std::process::Command::new("git")
            .args(["checkout", "-q", "--", "a.rs"])
            .current_dir(tmp.path())
            .output()
            .unwrap();
        let err = build(
            StageId::DualReview,
            &cfg,
            &dirs,
            "s",
            None,
            None,
            None,
            None,
        )
        .unwrap_err();
        assert!(format!("{err:#}").contains("nothing to review"), "{err:#}");

        // Untracked-only dirt is still reviewable (the skill reads those).
        std::fs::write(tmp.path().join("new.rs"), "brand new\n").unwrap();
        assert!(
            build(
                StageId::DualReview,
                &cfg,
                &dirs,
                "s",
                None,
                None,
                None,
                None
            )
            .is_ok()
        );

        // Outside git: dual-review refuses...
        let (_t2, cfg2, dirs2) = setup();
        let err = build(
            StageId::DualReview,
            &cfg2,
            &dirs2,
            "s",
            None,
            None,
            None,
            None,
        )
        .unwrap_err();
        assert!(
            format!("{err:#}").contains("not a git repository"),
            "{err:#}"
        );
        // ...but other stages still build fine there (git is optional for them).
        std::fs::create_dir_all(dirs2.feature_dir("s")).unwrap();
        std::fs::write(dirs2.plan_file("s"), "# plan").unwrap();
        assert!(
            build(
                StageId::PlanReview,
                &cfg2,
                &dirs2,
                "s",
                None,
                None,
                None,
                None
            )
            .is_ok()
        );
    }

    #[test]
    fn doc_chat_command_shape() {
        let (_tmp, mut cfg, dirs) = setup();
        let path = dirs.spec_file("feat-x");
        let cmd = doc_chat_command(
            &cfg,
            &path,
            DocKind::Spec,
            &Scope::Section("Behavior (the contract: WHAT, not HOW)".into()),
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
        // dontAsk denies every tool outside the grant. The Edit/Write rules
        // are BARE: the CLI (2.1.205) never matches path-scoped Edit/Write
        // rules under dontAsk, so scoping is enforced ritual-side (section
        // confinement gate), not by the permission engine.
        assert!(cmd.argv.contains(&"dontAsk".to_string()));
        assert!(!cmd.argv.contains(&"acceptEdits".to_string()));
        let tools = cmd
            .argv
            .iter()
            .find(|a| a.starts_with("Read,"))
            .expect("allowedTools value");
        assert_eq!(tools, "Read,Edit,Write");
        assert!(
            !tools.contains("(/"),
            "no path-scoped file rules (unmatched under dontAsk)"
        );

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
    fn findings_batch_fix_command_shape() {
        let (_tmp, mut cfg, dirs) = setup();
        cfg.budget_finding_fix_usd = 2.0;
        cfg.models.insert("plan".into(), "claude-fable-5".into());
        cfg.effort.insert("plan-fix".into(), "high".into());
        let plan = dirs.plan_file("feat-x");
        let briefs = vec![
            (
                1,
                FindingBrief {
                    title: "step 2 deletes chained runs",
                    severity: "major",
                    scenario: "verify-log breaks",
                    plan_step: "Step 2 (deletion)",
                    snippet: Some("2. Enumerate"),
                },
            ),
            (
                2,
                FindingBrief {
                    title: "risks section is vague",
                    severity: "minor",
                    scenario: "unbounded blast radius",
                    plan_step: "Risks section",
                    snippet: None,
                },
            ),
        ];
        let cmd = findings_batch_fix_command(
            &cfg,
            &plan,
            &["Steps", "Risks / open decision"],
            &briefs,
            None,
            None,
        );
        assert_eq!(cmd.mode, Mode::Headless);
        let prompt = cmd
            .argv
            .iter()
            .find(|a| a.starts_with("/spec"))
            .expect("has a /spec prompt");
        // Both findings ride numbered; the SCOPE line lists every section.
        assert!(prompt.contains(r#"SCOPE: sections "Steps", "Risks / open decision""#));
        assert!(prompt.contains("FINDING #1:"));
        assert!(prompt.contains("FINDING #2:"));
        assert!(prompt.contains("title: step 2 deletes chained runs"));
        assert!(prompt.contains("snippet:\n2. Enumerate"));
        // The contract the caller parses back.
        assert!(prompt.contains("resolve them coherently in ONE pass"));
        assert!(prompt.contains("ANSWERS:"));
        assert!(prompt.contains("#<n>: FIXED"));
        assert!(prompt.contains("#<n>: DECLINED <one-line reason>"));
        // Same grant + the fix budget capping the whole run (scoping is
        // ritual-side; path-scoped rules never match under dontAsk).
        assert!(cmd.argv.contains(&"dontAsk".to_string()));
        let tools = cmd
            .argv
            .iter()
            .find(|a| a.starts_with("Read,"))
            .expect("allowedTools value");
        assert_eq!(tools, "Read,Edit,Write");
        let i = cmd
            .argv
            .iter()
            .position(|a| a == "--max-budget-usd")
            .unwrap();
        assert_eq!(cmd.argv[i + 1], "2");
        // Routing composes like the single-fix command.
        assert!(
            cmd.argv
                .windows(2)
                .any(|w| w == ["--model", "claude-fable-5"])
        );
        assert!(cmd.argv.windows(2).any(|w| w == ["--effort", "high"]));
        // Empty section list degrades to whole-doc scope.
        let cmd = findings_batch_fix_command(&cfg, &plan, &[], &briefs, None, None);
        let prompt = cmd.argv.iter().find(|a| a.starts_with("/spec")).unwrap();
        assert!(prompt.contains("SCOPE: whole"));
    }

    #[test]
    fn interactive_stages_have_no_stream_flags() {
        let (_tmp, cfg, dirs) = setup();
        for stage in [StageId::Plan, StageId::TestsRed, StageId::Implement] {
            let cmd = build(stage, &cfg, &dirs, "s", None, None, None, None).unwrap();
            assert_eq!(cmd.mode, Mode::Interactive);
            assert!(!cmd.argv.contains(&"stream-json".to_string()));
        }
    }

    #[test]
    fn override_routing_and_fallback_compose_on_one_build() {
        let (_tmp, mut cfg, dirs) = setup();
        std::fs::create_dir_all(dirs.feature_dir("s")).unwrap();
        std::fs::write(dirs.plan_file("s"), "# plan").unwrap();
        cfg.models.insert("plan-review".into(), "opus".into());
        cfg.fallback_model = Some("claude-sonnet-5".into());

        let cmd = build(
            StageId::PlanReview,
            &cfg,
            &dirs,
            "s",
            None,
            Some("claude-fable-5"),
            None,
            None,
        )
        .unwrap();
        // Exactly ONE --model, carrying the override; fallback still rides.
        assert_eq!(
            cmd.argv.iter().filter(|a| *a == "--model").count(),
            1,
            "{:?}",
            cmd.argv
        );
        assert!(
            cmd.argv
                .windows(2)
                .any(|w| w == ["--model", "claude-fable-5"])
        );
        assert!(!cmd.argv.contains(&"opus".to_string()));
        assert!(
            cmd.argv
                .windows(2)
                .any(|w| w == ["--fallback-model", "claude-sonnet-5"])
        );
    }

    #[test]
    fn plan_and_implement_argv_content() {
        let (_tmp, cfg, dirs) = setup();
        let cmd = build(StageId::Plan, &cfg, &dirs, "s", None, None, None, None).unwrap();
        assert_eq!(cmd.argv[0], "claude");
        assert!(
            cmd.argv
                .windows(2)
                .any(|w| w == ["--permission-mode", "plan"])
        );
        let prompt = cmd.argv.last().unwrap();
        assert!(prompt.contains("spec.md"), "{prompt}");
        assert!(prompt.contains("plan.md"), "{prompt}");

        // No pinned session → BARE resume picker: appending the prompt would
        // make it the picker's search term, not a message.
        let cmd = build(StageId::Implement, &cfg, &dirs, "s", None, None, None, None).unwrap();
        assert_eq!(cmd.argv, vec!["claude", "--resume"]);
        assert!(!cmd.argv.iter().any(|a| a == "--continue"));
    }

    #[test]
    fn session_pins_tests_red_and_resumes_in_implement() {
        let (_tmp, cfg, dirs) = setup();
        let sid = "abcdef00-1111-4222-8333-444455556666";

        // tests-red pins the ritual-owned session id, then the /tdd prompt.
        let cmd = build(
            StageId::TestsRed,
            &cfg,
            &dirs,
            "s",
            None,
            None,
            Some(sid),
            None,
        )
        .unwrap();
        let i = cmd
            .argv
            .iter()
            .position(|a| a == "--session-id")
            .expect("tests-red pins the session");
        assert_eq!(cmd.argv[i + 1], sid);
        assert!(cmd.argv.last().unwrap().starts_with("/tdd "));
        // Without a session id, tests-red is unchanged.
        let bare = build(StageId::TestsRed, &cfg, &dirs, "s", None, None, None, None).unwrap();
        assert!(!bare.argv.iter().any(|a| a == "--session-id"));

        // implement resumes that exact session (no prompt - the CLI ignores a
        // positional on interactive resume; ritual surfaces it for paste).
        let cmd = build(
            StageId::Implement,
            &cfg,
            &dirs,
            "s",
            None,
            None,
            Some(sid),
            None,
        )
        .unwrap();
        assert_eq!(cmd.argv, vec!["claude", "--resume", sid]);
    }

    #[test]
    fn invariants_with_only_a_multiline_comment_stay_dark() {
        let (_tmp, cfg, dirs) = setup_git();
        std::fs::create_dir_all(dirs.feature_dir("s")).unwrap();
        std::fs::write(dirs.plan_file("s"), "# plan").unwrap();
        // The false-positive shape the T1 fix closed: block-comment inner
        // lines must not activate the constitution.
        std::fs::write(
            dirs.invariants_file(),
            "# Invariants\n<!--\n- looks like a bullet\nfill this in\n-->\n",
        )
        .unwrap();
        let cmd = build(
            StageId::DualReview,
            &cfg,
            &dirs,
            "s",
            None,
            None,
            None,
            None,
        )
        .unwrap();
        assert!(
            !cmd.env.iter().any(|(k, _)| k == "RITUAL_INVARIANTS_FILE"),
            "template comments alone must not inject the constitution"
        );
    }

    #[test]
    fn invariants_env_reaches_review_stages_only_when_meaningful() {
        let (_tmp, cfg, dirs) = setup_git();
        std::fs::create_dir_all(dirs.feature_dir("s")).unwrap();
        std::fs::write(dirs.plan_file("s"), "# plan").unwrap();
        let has_env =
            |cmd: &StageCommand| cmd.env.iter().any(|(k, _)| k == "RITUAL_INVARIANTS_FILE");

        // Absent file -> no env.
        let cmd = build(
            StageId::PlanReview,
            &cfg,
            &dirs,
            "s",
            None,
            None,
            None,
            None,
        )
        .unwrap();
        assert!(!has_env(&cmd));

        // Scaffold template (headings + comments only) -> still no env.
        std::fs::write(dirs.invariants_file(), crate::scaffold::INVARIANTS_TEMPLATE).unwrap();
        let cmd = build(
            StageId::DualReview,
            &cfg,
            &dirs,
            "s",
            None,
            None,
            None,
            None,
        )
        .unwrap();
        assert!(!has_env(&cmd));

        // Real bullets -> both review stages carry it; interactive ones don't.
        std::fs::write(dirs.invariants_file(), "# Invariants\n- no panics\n").unwrap();
        for stage in [StageId::PlanReview, StageId::DualReview] {
            let cmd = build(stage, &cfg, &dirs, "s", None, None, None, None).unwrap();
            assert!(
                cmd.env
                    .iter()
                    .any(|(k, v)| k == "RITUAL_INVARIANTS_FILE" && v.ends_with("invariants.md")),
                "{stage:?} must carry the constitution"
            );
        }
        let cmd = build(StageId::TestsRed, &cfg, &dirs, "s", None, None, None, None).unwrap();
        assert!(!has_env(&cmd));
    }

    #[test]
    fn model_override_beats_routing_table() {
        let (_tmp, mut cfg, dirs) = setup();
        std::fs::create_dir_all(dirs.feature_dir("s")).unwrap();
        std::fs::write(dirs.plan_file("s"), "# plan").unwrap();
        cfg.models.insert("plan-review".into(), "opus".into());

        let cmd = build(
            StageId::PlanReview,
            &cfg,
            &dirs,
            "s",
            None,
            None,
            None,
            None,
        )
        .unwrap();
        assert!(cmd.argv.windows(2).any(|w| w == ["--model", "opus"]));

        let cmd = build(
            StageId::PlanReview,
            &cfg,
            &dirs,
            "s",
            None,
            Some("claude-sonnet-5"),
            None,
            None,
        )
        .unwrap();
        assert!(
            cmd.argv
                .windows(2)
                .any(|w| w == ["--model", "claude-sonnet-5"])
        );
        assert!(!cmd.argv.contains(&"opus".to_string()));
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
        // Interactive stages own the terminal; never wrapped.
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

    fn code_brief() -> Vec<(u32, CodeFindingBrief<'static>)> {
        vec![(
            1,
            CodeFindingBrief {
                title: "race in save",
                severity: "critical",
                scenario: "two writers",
                file: "src/state.rs",
                line: Some(42),
                snippet: Some("let st = load()?;"),
            },
        )]
    }

    #[test]
    fn code_fix_command_is_broad_edit_and_carries_the_answers_contract() {
        let (_tmp, mut cfg, _dirs) = setup();
        cfg.budget_code_fix_usd = 5.0;
        cfg.models.insert("code".into(), "opus".into());
        cfg.effort.insert("code-fix".into(), "high".into());
        let cmd = findings_code_fix_command(&cfg, &code_brief(), None);
        assert_eq!(cmd.mode, Mode::Headless);
        // Broad edit grant + acceptEdits (NOT the plan.md-locked doc_chat_tools).
        let i = cmd.argv.iter().position(|a| a == "--allowedTools").unwrap();
        assert_eq!(cmd.argv[i + 1], CODE_FIX_TOOLS);
        assert!(cmd.argv[i + 1].contains("Edit") && cmd.argv[i + 1].contains("Bash"));
        // Hard denials on top of the Bash grant: history/remote mutation is
        // enforced by the permission engine, not just the prompt.
        let d = cmd
            .argv
            .iter()
            .position(|a| a == "--disallowedTools")
            .expect("fixer carries the git denial list");
        assert!(cmd.argv[d + 1].contains("git push") && cmd.argv[d + 1].contains("git commit"));
        let p = cmd
            .argv
            .iter()
            .position(|a| a == "--permission-mode")
            .unwrap();
        assert_eq!(cmd.argv[p + 1], "acceptEdits");
        // Code budget, not the plan-fix budget.
        let b = cmd
            .argv
            .iter()
            .position(|a| a == "--max-budget-usd")
            .unwrap();
        assert_eq!(cmd.argv[b + 1], "5");
        // Routing + prompt contents.
        assert!(cmd.argv.windows(2).any(|w| w == ["--model", "opus"]));
        assert!(cmd.argv.windows(2).any(|w| w == ["--effort", "high"]));
        let prompt = cmd.argv.iter().find(|a| a.contains("REQUEST:")).unwrap();
        assert!(prompt.contains("src/state.rs:42"));
        assert!(prompt.contains("ANSWERS:"));
        assert!(
            prompt.contains("Do NOT commit"),
            "destructive-command guardrail"
        );
        assert!(prompt.contains("./check.sh"));
    }

    #[test]
    fn code_review_command_is_read_only_and_embeds_the_diff() {
        let (_tmp, cfg, _dirs) = setup();
        let cmd = code_fix_review_command(&cfg, "diff --git a/x b/x\n+broken", &code_brief());
        let i = cmd.argv.iter().position(|a| a == "--allowedTools").unwrap();
        assert_eq!(cmd.argv[i + 1], CODE_REVIEW_TOOLS);
        assert!(
            !cmd.argv[i + 1].contains("Edit"),
            "reviewer must be read-only"
        );
        assert!(!cmd.argv[i + 1].contains("Write"));
        assert!(
            !cmd.argv[i + 1].contains("Bash"),
            "reviewer has no shell (can't mutate the tree or run git)"
        );
        // The prompt embeds the FULL (unbounded) diff, so it must ride on
        // stdin, never argv: a big diff in argv kills the exec with E2BIG.
        // Regression: run 20260716T114958540Z-f22-1-code-fix-review died at
        // spawn with "Argument list too long".
        let prompt = cmd.stdin.as_deref().expect("prompt rides on stdin");
        assert!(prompt.contains("REVIEW:"));
        assert!(prompt.contains("diff --git a/x b/x"));
        assert!(prompt.contains("REGRESSIONS:"));
        assert!(prompt.contains("src/state.rs:42"));
        assert!(
            !cmd.argv.iter().any(|a| a.contains("REVIEW:")),
            "no prompt in argv"
        );
        // `-p` with no positional prompt = read the prompt from stdin.
        assert!(cmd.argv.contains(&"-p".to_string()));
        // The read-only reviewer has no Bash, so no denial list to carry.
        assert!(!cmd.argv.contains(&"--disallowedTools".to_string()));
    }

    #[test]
    fn tests_red_stage_is_red_only() {
        let (_tmp, cfg, dirs) = setup();
        let cmd = build(StageId::TestsRed, &cfg, &dirs, "s", None, None, None, None).unwrap();
        let prompt = cmd.argv.last().unwrap();
        assert!(prompt.starts_with("/tdd "));
        assert!(
            prompt.contains("red-only"),
            "tests-red must tell the skill to stop at failing tests: {prompt}"
        );
    }

    fn audit_lane(name: &str, desc: &str) -> crate::audit::Lane {
        crate::audit::Lane {
            name: name.into(),
            description: desc.into(),
        }
    }

    #[test]
    fn audit_discover_command_shape() {
        let (_tmp, mut cfg, _dirs) = setup();
        cfg.budget_audit_usd = 1.5;
        cfg.models.insert("audit".into(), "haiku".into());
        let cmd = audit_discover_command(&cfg, Path::new("/proj/.ritual/audit-lanes.md"));
        assert_eq!(cmd.mode, Mode::Headless);
        assert!(!cmd.needs_codex);
        let i = cmd.argv.iter().position(|a| a == "--allowedTools").unwrap();
        // Bare names only: path-scoped rules never match under dontAsk.
        assert_eq!(cmd.argv[i + 1], "Read,Glob,Grep,Write");
        assert!(cmd.argv.contains(&"dontAsk".to_string()));
        assert!(
            cmd.argv
                .windows(2)
                .any(|w| w == ["--max-budget-usd", "1.5"])
        );
        assert!(cmd.argv.windows(2).any(|w| w == ["--model", "haiku"]));
        let prompt = cmd.argv.iter().find(|a| a.contains("lanes")).unwrap();
        assert!(prompt.contains("/proj/.ritual/audit-lanes.md"));
        assert!(prompt.contains("AT MOST 7"), "cap minus the global lane");
        assert!(prompt.contains("global-overview"), "told not to write one");
    }

    #[test]
    fn audit_lane_command_is_blind_and_demands_evidence() {
        let (_tmp, cfg, dirs) = setup();
        std::fs::create_dir_all(dirs.root()).unwrap();
        std::fs::write(dirs.invariants_file(), "- never block the render loop\n").unwrap();
        let lane = audit_lane("runner", "daemon lifecycle SECRET-A");
        let cmd = audit_lane_command(
            &cfg,
            &lane,
            &["tui", "findings"],
            Path::new("/tmp/audit/runner.md"),
            Some(&dirs.invariants_file()),
        );
        let i = cmd.argv.iter().position(|a| a == "--allowedTools").unwrap();
        assert_eq!(cmd.argv[i + 1], "Read,Glob,Grep,Write");
        assert!(!cmd.needs_codex);
        let prompt = cmd.argv.iter().find(|a| a.contains("LANE:")).unwrap();
        // Its own scope, the OTHER lanes by name only, the report path.
        assert!(prompt.contains("runner") && prompt.contains("SECRET-A"));
        assert!(prompt.contains("tui, findings"));
        assert!(prompt.contains("/tmp/audit/runner.md"));
        assert!(prompt.contains("INVARIANTS_FILE"));
        // The evidence-grade contract.
        for grade in ["reproduced", "traced", "suspected"] {
            assert!(prompt.contains(grade), "missing evidence grade {grade}");
        }
        // No invariants file -> no dangling reference.
        let bare = audit_lane_command(&cfg, &lane, &[], Path::new("/tmp/r.md"), None);
        let p = bare.argv.iter().find(|a| a.contains("LANE:")).unwrap();
        assert!(!p.contains("INVARIANTS_FILE"));
    }

    #[test]
    fn audit_judge_command_adjudicates_via_stdin_with_codex() {
        let (_tmp, mut cfg, _dirs) = setup();
        cfg.models.insert("audit-judge".into(), "opus".into());
        let reports = "== lane runner ==\nGIANT-REPORT-MARKER\n".repeat(4);
        let cmd = audit_judge_command(&cfg, Path::new("/abs/proj/.ritual/findings"), 8, reports);
        assert!(cmd.needs_codex, "cross-vendor verdicts need codex MCP");
        // The unbounded payload rides on stdin, never argv (E2BIG).
        let payload = cmd.stdin.as_deref().expect("payload on stdin");
        assert!(payload.contains("GIANT-REPORT-MARKER"));
        assert!(!cmd.argv.iter().any(|a| a.contains("GIANT-REPORT-MARKER")));
        // The adjudication contract. The findings dir is ABSOLUTE in the
        // prompt: a no-shell agent can't expand an env idiom, and its
        // relative fallback lands in the wrong .ritual from a worktree.
        for needle in [
            "REFUTE",
            "codex",
            "confirmed|unconfirmed",
            "\"stage\": \"audit\"",
            "FINDINGS_DIR: /abs/proj/.ritual/findings",
        ] {
            assert!(payload.contains(needle), "judge contract missing {needle}");
        }
        // The judge's CAP scales with the reports it adjudicates: 8 lanes at
        // the default $3 per leg -> $3 x (1 + 8) = $27 (a ceiling, not a
        // spend; live 8-lane smokes starved at both $3 and $15 caps).
        let b = cmd
            .argv
            .iter()
            .position(|a| a == "--max-budget-usd")
            .unwrap();
        assert_eq!(cmd.argv[b + 1], "27");
        // Bash to reproduce, git mutation hard-denied, codex tools granted.
        let i = cmd.argv.iter().position(|a| a == "--allowedTools").unwrap();
        assert!(cmd.argv[i + 1].contains("Bash"));
        assert!(cmd.argv[i + 1].contains("mcp__codex__codex"));
        let d = cmd
            .argv
            .iter()
            .position(|a| a == "--disallowedTools")
            .expect("git guardrail present");
        assert!(cmd.argv[d + 1].contains("git push"));
        assert!(cmd.argv.windows(2).any(|w| w == ["--model", "opus"]));
    }
}
