//! `ritual run <stage>`: non-TUI stage execution with live styled output.

use std::path::Path;

use anyhow::{Context, Result};
use chrono::Utc;
use tokio::sync::mpsc;

use crate::config::Config;
use crate::runner::{self, RunRequest};
use crate::scaffold;
use crate::stages::{self, Mode};
use crate::state::{self, RitualDirs, StageId, StageStatus, State};

pub fn execute(
    cfg: &Config,
    dirs: &RitualDirs,
    stage_str: &str,
    arg: Option<&str>,
    force: bool,
    ci: bool,
    model: Option<&str>,
) -> Result<()> {
    let stage = StageId::parse(stage_str)
        .with_context(|| format!("unknown stage '{stage_str}' (spec, plan, plan-review, tests-red, implement, dual-review, coverage)"))?;
    anyhow::ensure!(dirs.exists(), "no .ritual/ here; run `ritual init` first");

    if let Some((spent, budget)) = budget_exceeded(cfg, dirs)
        && !force
    {
        anyhow::bail!(
            "daily budget reached: ${spent:.2} of ${budget:.2} spent today; rerun with --force to override"
        );
    }

    let branch = state::current_branch(&dirs.work_root).unwrap_or_else(|| "detached".to_string());
    let slug = state::branch_slug(&branch);
    let mut st = State::load(dirs)?;
    st.feature_for_branch_mut(&branch); // ensure the feature exists
    let title = st
        .features
        .get(&slug)
        .map(|f| f.title.clone())
        .unwrap_or_default();

    // Session parity with the TUI: pin a fresh id for tests-red so a later
    // `run implement` can resume THAT session (never the fragile `--continue`).
    let session: Option<String> = match stage {
        StageId::TestsRed => {
            let sid = crate::export::fresh_session_id();
            st.set_stage_session_id(&slug, StageId::TestsRed, Some(sid.clone()));
            Some(sid)
        }
        StageId::Implement => st.stage_session_id(&slug, StageId::TestsRed),
        _ => None,
    };
    let cmd = stages::build(stage, cfg, dirs, &slug, arg, model, session.as_deref())?;

    if cmd.needs_codex && !cfg.offline && !stages::codex_ready(cfg) {
        anyhow::bail!(
            "codex is not authenticated. Run `codex login` first (stage '{}' talks to Codex via MCP)",
            stage.label()
        );
    }

    match cmd.mode {
        Mode::Local => run_spec_stage(dirs, &slug, &title, &mut st, &branch),
        Mode::Interactive => run_interactive(cfg, dirs, stage, cmd.argv, &mut st, &branch),
        Mode::Headless => run_headless(cfg, dirs, stage, cmd, &mut st, &branch, &title, ci),
    }
}

/// Follow a (possibly detached) run to completion, rendering each event.
/// This is the shared tail loop behind `ritual run`, `ritual chat`, and `ritual
/// attach`. Ctrl-C here leaves the daemon alive.
pub fn follow_run(
    cfg: &Config,
    dirs: &RitualDirs,
    agent: runner::AgentKind,
    run_id: &str,
) -> Result<runner::RunOutcome> {
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        let (tx, mut rx) = mpsc::channel(256);
        let dirs2 = dirs.clone();
        let rid = run_id.to_string();
        let handle = tokio::spawn(async move { runner::tail_run(&dirs2, agent, &rid, tx).await });
        while let Some(ev) = rx.recv().await {
            crate::output::render_event(cfg, &ev);
        }
        handle.await?
    })
}

/// `ritual ps`: live detached runs (pipeline and chat alike).
pub fn ps(dirs: &RitualDirs) -> Result<()> {
    let live = runner::live_runs(dirs);
    if live.is_empty() {
        println!("no live runs");
        return Ok(());
    }
    println!(
        "{:<44} {:<12} {:<16} {:>8} {:>6}",
        "run_id", "stage", "branch", "pid", "age"
    );
    for (run_id, status) in live {
        println!(
            "{:<44} {:<12} {:<16} {:>8} {:>6}",
            run_id,
            status.stage,
            status.branch,
            status.pid,
            run_age(&run_id)
        );
    }
    Ok(())
}

/// Humanized age from the run id's millisecond timestamp prefix.
fn run_age(run_id: &str) -> String {
    let Some(ts) = run_id.split('-').next() else {
        return "?".into();
    };
    let Ok(t) = chrono::NaiveDateTime::parse_from_str(ts, "%Y%m%dT%H%M%S%3fZ") else {
        return "?".into();
    };
    let secs = (Utc::now().naive_utc() - t).num_seconds().max(0);
    match secs {
        s if s < 60 => format!("{s}s"),
        s if s < 3600 => format!("{}m", s / 60),
        s => format!("{}h", s / 3600),
    }
}

/// `ritual attach <run-id>`: follow a live detached run from any terminal
/// (or --kill it); finished runs print their summary.
pub fn attach(cfg: &Config, dirs: &RitualDirs, run_id: &str, kill: bool) -> Result<()> {
    match runner::run_state(dirs, run_id) {
        runner::RunState::Running(status) => {
            if kill {
                let killed = runner::kill_run(dirs, run_id);
                anyhow::ensure!(killed, "could not signal {run_id}");
                println!("{run_id} ({}) killed", status.stage);
                return Ok(());
            }
            // The agent lives in the persisted request, not the status file.
            let agent = runner::load_request(dirs, run_id)
                .map(|r| r.agent)
                .unwrap_or(runner::AgentKind::Claude);
            println!(
                "attached to {run_id} ({} on {})",
                status.stage, status.branch
            );
            let outcome = follow_run(cfg, dirs, agent, run_id)?;
            crate::output::render_run_summary(cfg, &outcome.meta, &[]);
            anyhow::ensure!(outcome.meta.ok, "run '{run_id}' failed");
            Ok(())
        }
        runner::RunState::Finished(meta) => {
            anyhow::ensure!(!kill, "run '{run_id}' already finished");
            crate::output::render_run_summary(cfg, &meta, &[]);
            anyhow::ensure!(meta.ok, "run '{run_id}' failed");
            Ok(())
        }
        runner::RunState::Vanished => {
            let any_trace = ["jsonl", "request.json", "status"]
                .iter()
                .any(|ext| dirs.runs_dir().join(format!("{run_id}.{ext}")).exists());
            if any_trace {
                anyhow::bail!("run '{run_id}' vanished (daemon died before writing meta)");
            }
            anyhow::bail!("no such run '{run_id}'; see `ritual ps` or `ritual history`");
        }
    }
}

/// `ritual complete [--check]`. With `--check`, evaluate the CURRENT
/// completeness state (token-free) and return whether the feature is done - the
/// CI gate. Without it, run one fresh coverage judge pass first (P7 wraps the
/// bounded auto-fix loop around this), then evaluate. Returns `is_complete`.
pub fn complete(cfg: &Config, dirs: &RitualDirs, check: bool) -> Result<bool> {
    anyhow::ensure!(dirs.exists(), "no .ritual/ here; run `ritual init` first");
    let branch = state::current_branch(&dirs.work_root).unwrap_or_else(|| "detached".to_string());
    let slug = state::branch_slug(&branch);

    if !check {
        let title = {
            let mut st = State::load(dirs)?;
            st.feature_for_branch_mut(&branch);
            st.features
                .get(&slug)
                .map(|f| f.title.clone())
                .unwrap_or_default()
        };
        let bounds = crate::complete::Bounds {
            max_rounds: cfg.complete_max_rounds,
            clean_rounds: cfg.complete_clean_rounds,
            round_scope: cfg.complete_round_scope,
            max_attempts: cfg.complete_max_attempts_per_item,
        };
        // The run-ids that already existed - so the loop's own spend is the cost
        // of runs on THIS branch created after we started, not the whole day's
        // spend across every feature/terminal (which would stop the loop early).
        let runs_before: std::collections::HashSet<String> =
            crate::history::load_all(&dirs.runs_dir())
                .unwrap_or_default()
                .iter()
                .map(|m| m.run_id.clone())
                .collect();
        let mut ds = crate::complete::DriveState::default();
        loop {
            let spent: f64 = crate::history::load_all(&dirs.runs_dir())
                .unwrap_or_default()
                .iter()
                .filter(|m| m.branch == branch && !runs_before.contains(&m.run_id))
                .filter_map(|m| m.total_cost_usd)
                .sum();
            if budget_exceeded(cfg, dirs).is_some() {
                println!("daily budget reached; stopping the complete loop");
                break;
            }
            if cfg.budget_complete_usd > 0.0 && spent >= cfg.budget_complete_usd {
                println!(
                    "complete budget (${:.2}) exhausted after ${spent:.2}; stopping",
                    cfg.budget_complete_usd
                );
                break;
            }
            // One coverage judge pass (ticks satisfied boxes, sets the stage).
            let cov_before = coverage_files(&dirs.findings_dir());
            {
                let mut st = State::load(dirs)?;
                st.feature_for_branch_mut(&branch);
                let cmd = stages::build(StageId::Coverage, cfg, dirs, &slug, None, None, None)?;
                run_headless(
                    cfg,
                    dirs,
                    StageId::Coverage,
                    cmd,
                    &mut st,
                    &branch,
                    &title,
                    false,
                )?;
            }
            // A judge that wrote NO new report produced nothing to trust; an
            // empty `latest_report` would otherwise read as a false "clean".
            // Set-difference (not a count) so it composes with the supersede
            // sweep that deletes the OLD coverage files during the run.
            let cov_after = coverage_files(&dirs.findings_dir());
            if cov_after.difference(&cov_before).next().is_none() {
                println!("stopped: the coverage judge produced no report this round");
                break;
            }
            let report = crate::coverage::latest_report(&dirs.findings_dir()).unwrap_or_default();
            match crate::complete::plan_round(&mut ds, &report, &bounds) {
                crate::complete::RoundAction::Done => {
                    println!("✓ coverage clean; all deliverables satisfied");
                    break;
                }
                crate::complete::RoundAction::MaxRounds => {
                    println!("stopped: reached the {}-round cap", bounds.max_rounds);
                    break;
                }
                crate::complete::RoundAction::Stuck(ids) => {
                    println!(
                        "stopped: {} deliverable(s) unresolved after {} attempts each: {}",
                        ids.len(),
                        bounds.max_attempts,
                        ids.join(", ")
                    );
                    break;
                }
                crate::complete::RoundAction::Drive(ids) if ids.is_empty() => continue,
                crate::complete::RoundAction::Drive(ids) => {
                    let gaps: Vec<&crate::coverage::Gap> = report
                        .gaps
                        .iter()
                        .filter(|g| ids.contains(&g.deliverable))
                        .collect();
                    println!(
                        "round {}: building {} deliverable(s): {}",
                        ds.round,
                        gaps.len(),
                        ids.join(", ")
                    );
                    drive_gaps(cfg, dirs, &branch, &slug, &title, &gaps)?;
                }
            }
        }
    }

    let findings = crate::findings::load_all(&dirs.findings_dir())?;
    // Completeness is derived from EVIDENCE, never the Coverage stage status:
    // the latest coverage report must be genuinely zero-gap AND the plan must
    // declare a real `## Deliverables` checklist (the deterministic backstop).
    let report = crate::coverage::latest_report(&dirs.findings_dir());
    let coverage_clean = report.as_ref().is_some_and(|r| r.gaps.is_empty());
    let plan_text = std::fs::read_to_string(dirs.plan_file(&slug)).unwrap_or_default();
    let deliverables = crate::spec::deliverables_gate(&plan_text);
    let deliverables_ok = deliverables.is_ok();
    let no_open = !crate::findings::has_open_confirmed(&findings);
    // Only spend a check.sh run once coverage + deliverables pass.
    let green =
        coverage_clean && deliverables_ok && check_green(&dirs.work_root, cfg.check_timeout_secs);
    let done = crate::coverage::feature_complete(coverage_clean, deliverables_ok, &findings, green);

    if done {
        println!(
            "✓ complete: all deliverables satisfied, check.sh green, no open confirmed findings"
        );
    } else {
        println!("✗ not complete:");
        if let Err(why) = &deliverables {
            println!("  deliverables: {why}");
        }
        if deliverables_ok && !coverage_clean {
            match &report {
                Some(rep) => {
                    println!(
                        "  coverage: {} deliverable gap(s) (run `ritual complete` to fix)",
                        rep.gaps.len()
                    );
                    for g in rep.gaps.iter().take(20) {
                        println!("    - {}: {}", g.deliverable, g.finding.title);
                    }
                }
                None => println!("  coverage: not judged yet (run `ritual complete`)"),
            }
        }
        if coverage_clean && deliverables_ok && !green {
            println!("  check.sh: red");
        }
        if !no_open {
            println!("  findings: open confirmed finding(s) remain");
        }
    }
    Ok(done)
}

/// Drive a batch of coverage gaps to fixes: code gaps (a file to build/fix)
/// through the broad-edit code-fix run, plan gaps (the plan/spec itself)
/// through the plan-fix run. Each fix agent runs `./check.sh` itself; the next
/// coverage pass is the verification, so this reuses the command builders and
/// `follow_run` without re-implementing the 3-leg TUI gate.
fn drive_gaps(
    cfg: &Config,
    dirs: &RitualDirs,
    branch: &str,
    slug: &str,
    title: &str,
    gaps: &[&crate::coverage::Gap],
) -> Result<()> {
    let invariants = stages::meaningful_invariants(dirs);

    // A gap with neither a file nor a plan_step route can't be driven; report it
    // so the eventual STUCK has a visible cause (add a `route:` hint) instead of
    // silently burning attempts.
    for f in gaps.iter().map(|g| &g.finding) {
        if f.file.is_none() && f.plan_step.is_none() {
            println!(
                "  skipping unroutable gap (no file/plan_step route): {}",
                f.title
            );
        }
    }

    let code: Vec<&crate::findings::Finding> = gaps
        .iter()
        .map(|g| &g.finding)
        .filter(|f| f.file.is_some())
        .collect();
    if !code.is_empty() {
        let sev: Vec<String> = code
            .iter()
            .map(|f| f.severity.label().to_string())
            .collect();
        let briefs: Vec<(u32, stages::CodeFindingBrief)> = code
            .iter()
            .enumerate()
            .map(|(i, f)| {
                (
                    i as u32 + 1,
                    stages::CodeFindingBrief {
                        title: &f.title,
                        severity: &sev[i],
                        scenario: &f.scenario,
                        file: f.file.as_deref().unwrap_or(""),
                        line: f.line,
                        snippet: f.snippet.as_deref(),
                    },
                )
            })
            .collect();
        // Parity with the TUI code-fix: the prompt forbids commit/reset and
        // promises ritual checks HEAD; headlessly we at least warn if the agent
        // moved it (snapshot fails-open, so an unborn HEAD is harmless).
        let snap = crate::git::snapshot(&dirs.work_root).ok();
        let cmd = stages::findings_code_fix_command(cfg, &briefs, invariants.as_deref());
        run_fix(cfg, dirs, branch, title, "code-fix", cmd)?;
        if let Some(snap) = snap
            && crate::git::head_moved(&dirs.work_root, &snap)
        {
            println!(
                "  warning: the code-fix agent moved HEAD (commit/reset); inspect with git reflog"
            );
        }
    }

    let plan: Vec<&crate::findings::Finding> = gaps
        .iter()
        .map(|g| &g.finding)
        .filter(|f| f.file.is_none() && f.plan_step.is_some())
        .collect();
    if !plan.is_empty() {
        let sev: Vec<String> = plan
            .iter()
            .map(|f| f.severity.label().to_string())
            .collect();
        let steps: Vec<&str> = plan
            .iter()
            .map(|f| f.plan_step.as_deref().unwrap_or(""))
            .collect();
        let briefs: Vec<(u32, stages::FindingBrief)> = plan
            .iter()
            .enumerate()
            .map(|(i, f)| {
                (
                    i as u32 + 1,
                    stages::FindingBrief {
                        title: &f.title,
                        severity: &sev[i],
                        scenario: &f.scenario,
                        plan_step: steps[i],
                        snippet: f.snippet.as_deref(),
                    },
                )
            })
            .collect();
        let plan_path = dirs.plan_file(slug);
        let spec = dirs.spec_file(slug);
        let cmd = stages::findings_batch_fix_command(
            cfg,
            &plan_path,
            &steps,
            &briefs,
            Some(&spec),
            invariants.as_deref(),
        );
        run_fix(cfg, dirs, branch, title, "plan-fix", cmd)?;
    }
    Ok(())
}

/// Spawn a fix command as a detached run and follow it to completion (no stage
/// bookkeeping - the completeness loop owns the state).
fn run_fix(
    cfg: &Config,
    dirs: &RitualDirs,
    branch: &str,
    title: &str,
    stage_label: &str,
    cmd: stages::StageCommand,
) -> Result<()> {
    let req = RunRequest {
        agent: cmd.agent,
        argv: cmd.argv,
        env: cmd.env,
        stage: stage_label.into(),
        feature: title.into(),
        branch: branch.into(),
        redact: cfg.redaction,
        repro: None,
        cwd: dirs.work_root.clone(),
        wrapper: stages::wrapper_argv(cfg, cmd.mode),
    };
    let run_id = runner::new_run_id(stage_label);
    runner::spawn_detached(dirs, &req, &run_id)?;
    let outcome = follow_run(cfg, dirs, req.agent, &run_id)?;
    if !outcome.meta.ok {
        // Surface WHY (budget cap, tool denial, ...) instead of swallowing it;
        // the gap will recur and eventually go STUCK, but the user needs the
        // actionable reason. Never bail - that would abort the whole loop.
        println!(
            "  {stage_label} run did not succeed: {}",
            crate::history::decode_failure(&outcome.meta)
        );
    }
    Ok(())
}

/// `ritual reset-plan [--force]`: re-plan from the spec. Without `--force`, print
/// what WOULD change; with it, delete plan.md, reset the plan-derived stages to
/// pending, and clear the plan findings + plan undo stack. Never touches code.
pub fn reset_plan(dirs: &RitualDirs, force: bool) -> Result<()> {
    anyhow::ensure!(dirs.exists(), "no .ritual/ here; run `ritual init` first");
    let branch = state::current_branch(&dirs.work_root).unwrap_or_else(|| "detached".to_string());
    let slug = state::branch_slug(&branch);
    let mut st = State::load(dirs)?;

    if !force {
        let p = crate::reset::preview(dirs, &st, &branch);
        println!(
            "reset-plan (dry run) for '{slug}': would delete plan.md ({}), reset {} stage(s) to pending, remove {} plan finding file(s), and clear the plan undo stack.",
            if p.plan_deleted { "present" } else { "absent" },
            p.stages_reset,
            p.findings_removed,
        );
        println!("spec.md and git-tracked code are untouched. Re-run with --force to apply.");
        return Ok(());
    }

    // A live plan-fix/chat could be mid-write to plan.md; don't race the delete
    // (parity with the TUI's fix_running guard).
    let live_plan_edit = runner::live_runs(dirs)
        .into_iter()
        .any(|(_, s)| s.branch == branch && (s.stage.contains("plan") || s.stage.contains("chat")));
    anyhow::ensure!(
        !live_plan_edit,
        "a plan edit (plan-fix/chat) is running on '{branch}'; wait for it to finish before reset-plan"
    );

    let sum = crate::reset::reset_plan(dirs, &mut st, &branch);
    st.save(dirs)?;
    println!(
        "reset plan for '{slug}': plan.md {}, {} stage(s) reset, {} plan finding file(s) removed. Re-run the plan stage to start fresh.",
        if sum.plan_deleted {
            "deleted"
        } else {
            "already absent"
        },
        sum.stages_reset,
        sum.findings_removed,
    );
    Ok(())
}

/// Some((spent, budget)) when the daily ceiling is hit.
pub fn budget_exceeded(cfg: &Config, dirs: &RitualDirs) -> Option<(f64, f64)> {
    let budget = cfg.budget_daily_usd?;
    let spent = crate::history::today_spend(&dirs.runs_dir());
    (spent >= budget).then_some((spent, budget))
}

pub(crate) fn set_stage(
    st: &mut State,
    branch: &str,
    stage: StageId,
    status: StageStatus,
    run_id: Option<String>,
) {
    let feature = st.feature_for_branch_mut(branch);
    let entry = feature.stages.entry(stage).or_default();
    match status {
        StageStatus::Running => entry.started_at = Some(Utc::now()),
        _ => entry.finished_at = Some(Utc::now()),
    }
    entry.status = status;
    if let Some(id) = run_id {
        entry.runs.push(id);
    }
    feature.updated_at = Utc::now();
}

fn run_spec_stage(
    dirs: &RitualDirs,
    slug: &str,
    title: &str,
    st: &mut State,
    branch: &str,
) -> Result<()> {
    let spec = dirs.spec_file(slug);
    if !spec.exists() {
        std::fs::create_dir_all(dirs.feature_dir(slug))?;
        std::fs::write(&spec, scaffold::SPEC_TEMPLATE.replace("<title>", title))?;
    }
    let editor = std::env::var("EDITOR").unwrap_or_else(|_| "vi".into());
    let status = std::process::Command::new(&editor)
        .arg(&spec)
        .status()
        .with_context(|| format!("launching $EDITOR ({editor})"))?;
    anyhow::ensure!(status.success(), "editor exited with {status}");

    let content = std::fs::read_to_string(&spec).unwrap_or_default();
    let meaningful = crate::spec::has_meaningful_content(&content);
    let new_status = if meaningful {
        StageStatus::Done
    } else {
        StageStatus::Pending
    };
    set_stage(st, branch, StageId::Spec, new_status, None);
    st.save(dirs)?;
    println!(
        "spec {} ({})",
        if meaningful {
            "done"
        } else {
            "still empty, left pending"
        },
        spec.display()
    );
    Ok(())
}

fn run_interactive(
    cfg: &Config,
    dirs: &RitualDirs,
    stage: StageId,
    argv: Vec<String>,
    st: &mut State,
    branch: &str,
) -> Result<()> {
    let slug = state::branch_slug(branch);
    let plan_mtime_before = mtime(&dirs.plan_file(&slug));

    set_stage(st, branch, stage, StageStatus::Running, None);
    st.save(dirs)?;

    // Interactive `--resume` ignores a positional prompt, so hand the user the
    // kick-off prompt (clipboard, like the TUI) instead of silently launching.
    if stage == StageId::Implement {
        let copied = crate::clipboard::copy(stages::IMPLEMENT_PROMPT);
        println!(
            "kick-off prompt{}:\n  {}",
            if copied { " (copied to clipboard)" } else { "" },
            stages::IMPLEMENT_PROMPT
        );
    }

    let (bin, args) = argv.split_first().context("empty argv")?;
    let status = std::process::Command::new(bin)
        .args(args)
        .current_dir(&dirs.work_root)
        .status()
        .with_context(|| format!("launching {bin}"))?;

    // Completion heuristics per stage (attached runs give us no event stream).
    let new_status = match stage {
        StageId::Plan => {
            if mtime(&dirs.plan_file(&slug)) != plan_mtime_before {
                StageStatus::Done
            } else {
                println!(
                    "plan.md unchanged, marking needs-attention (save the plan to {})",
                    dirs.plan_file(&slug).display()
                );
                StageStatus::NeedsAttention
            }
        }
        StageId::TestsRed => {
            // /tdd writes failing tests then implements in one session. Only
            // auto-advance Implement to Done when the session ran to completion
            // AND the tree is green; a bailed session with a coincidentally
            // green tree must not silently mark implement done.
            if status.success() && check_green(&dirs.work_root, cfg.check_timeout_secs) {
                set_stage(st, branch, StageId::Implement, StageStatus::Done, None);
                println!("check.sh green: tests-red and implement both done");
            } else {
                println!("failing tests in place, ready to implement");
            }
            StageStatus::Done
        }
        StageId::Implement => {
            if check_green(&dirs.work_root, cfg.check_timeout_secs) {
                StageStatus::Done
            } else {
                println!("check.sh still red: implement stays needs-attention");
                StageStatus::NeedsAttention
            }
        }
        _ => {
            if status.success() {
                StageStatus::Done
            } else {
                StageStatus::Failed
            }
        }
    };
    set_stage(st, branch, stage, new_status, None);
    st.save(dirs)?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn run_headless(
    cfg: &Config,
    dirs: &RitualDirs,
    stage: StageId,
    cmd: stages::StageCommand,
    st: &mut State,
    branch: &str,
    title: &str,
    ci: bool,
) -> Result<()> {
    // Pre-review gates run BEFORE the findings snapshot so their artifacts
    // don't count as the agent run's own output: refresh the review memory
    // the dual-review skill reads, then the gitleaks pass over changed files.
    if stage == StageId::DualReview {
        let _ = crate::lessons::refresh(dirs);
        if let Some(msg) = crate::secrets::preflight(cfg, dirs) {
            println!("{msg}");
        }
        // Sequential here (CLI blocks anyway) so the skill sees the file.
        if let Some(msg) = crate::coderabbit::preflight(cfg, dirs) {
            println!("{msg}");
        }
    }
    let findings_before = list_findings(&dirs.findings_dir());

    set_stage(st, branch, stage, StageStatus::Running, None);
    st.save(dirs)?;

    let req = RunRequest {
        agent: cmd.agent,
        argv: cmd.argv,
        env: cmd.env,
        stage: stage.label().into(),
        feature: title.into(),
        branch: branch.into(),
        redact: cfg.redaction,
        repro: Some(crate::provenance::collect(cfg, dirs)),
        cwd: dirs.work_root.clone(),
        wrapper: stages::wrapper_argv(cfg, cmd.mode),
    };

    // Daemonize, then follow along. Ctrl-C here leaves the run alive.
    let run_id = runner::new_run_id(stage.label());
    runner::spawn_detached(dirs, &req, &run_id)?;
    println!("run {run_id} started (detached, survives this terminal)");

    let outcome = follow_run(cfg, dirs, req.agent, &run_id)?;

    let new_findings: Vec<String> = list_findings(&dirs.findings_dir())
        .into_iter()
        .filter(|f| !findings_before.contains(f))
        .collect();

    let new_status = if !outcome.meta.ok {
        StageStatus::Failed
    } else if stage == StageId::Coverage {
        // Coverage is Done ONLY when the judge reports zero gaps (green tests
        // are not enough); it also ticks the satisfied deliverables' boxes.
        finalize_coverage(dirs, branch, &new_findings)
    } else if new_findings.is_empty() {
        // Review stages must leave a findings artifact; an ok run without one
        // means the skill under-delivered (asked a question, hit a wall...).
        println!("run finished ok but wrote no findings file; needs attention");
        StageStatus::NeedsAttention
    } else {
        StageStatus::Done
    };
    set_stage(
        st,
        branch,
        stage,
        new_status,
        Some(outcome.meta.run_id.clone()),
    );
    st.save(dirs)?;

    // CI mode: JUnit XML from the findings this run produced.
    if ci {
        let parsed: Vec<crate::findings::FindingsFile> = new_findings
            .iter()
            .filter_map(|name| {
                std::fs::read_to_string(dirs.findings_dir().join(name))
                    .ok()
                    .and_then(|t| serde_json::from_str(&t).ok())
            })
            .collect();
        let refs: Vec<&crate::findings::FindingsFile> = parsed.iter().collect();
        let junit = crate::ci::write_junit(
            &dirs.root().join("ci"),
            &outcome.meta.run_id,
            stage.label(),
            &refs,
            !outcome.meta.ok,
        )?;
        println!(
            "junit: {} ({} tests, {} failures)",
            junit.path.display(),
            junit.tests,
            junit.failures
        );
        if junit.failures > 0 {
            anyhow::bail!("{} blocking finding(s); see JUnit report", junit.failures);
        }
    }

    crate::output::render_run_summary(cfg, &outcome.meta, &new_findings);
    crate::notify::notify(
        cfg.notifications,
        &format!(
            "ritual: {} {}",
            stage.label(),
            match new_status {
                StageStatus::Done => "done",
                StageStatus::NeedsAttention => "needs attention",
                _ => "failed",
            }
        ),
        &format!(
            "{}: {} new findings, ${:.2}",
            branch,
            new_findings.len(),
            outcome.meta.total_cost_usd.unwrap_or(0.0)
        ),
    );
    if !outcome.meta.permission_denials.is_empty() {
        println!(
            "  ⚠ permission denials: {}; tune allowedTools or permission mode",
            outcome.meta.permission_denials.len()
        );
    }
    if !outcome.meta.ok {
        if let Some(sid) = &outcome.meta.session_id {
            println!("  take over interactively: claude --resume {sid}");
        }
        anyhow::bail!("stage '{}' failed", stage.label());
    }
    Ok(())
}

/// `ritual chat <message>`: one spec/plan chat edit, headless. Mirrors
/// `run_headless` but builds via `stages::doc_chat_command`, writes no
/// findings, and finalizes the stage by whether the document actually changed.
pub fn run_doc_chat(
    cfg: &Config,
    dirs: &RitualDirs,
    message: &str,
    plan: bool,
    section: Option<String>,
    force: bool,
) -> Result<()> {
    anyhow::ensure!(dirs.exists(), "no .ritual/ here; run `ritual init` first");
    anyhow::ensure!(!message.trim().is_empty(), "usage: ritual chat <message>");

    if let Some((spent, budget)) = budget_exceeded(cfg, dirs)
        && !force
    {
        anyhow::bail!(
            "daily budget reached: ${spent:.2} of ${budget:.2} spent today; rerun with --force to override"
        );
    }

    let branch = state::current_branch(&dirs.work_root).unwrap_or_else(|| "detached".to_string());
    let slug = state::branch_slug(&branch);
    let mut st = State::load(dirs)?;
    st.feature_for_branch_mut(&branch);
    let title = st
        .features
        .get(&slug)
        .map(|f| f.title.clone())
        .unwrap_or_default();

    let (kind, stage_id, doc_path) = if plan {
        (stages::DocKind::Plan, StageId::Plan, dirs.plan_file(&slug))
    } else {
        (stages::DocKind::Spec, StageId::Spec, dirs.spec_file(&slug))
    };
    let scope = match section {
        Some(name) => stages::Scope::Section(name),
        None => stages::Scope::Whole,
    };
    std::fs::create_dir_all(dirs.feature_dir(&slug))?;
    let doc_before = std::fs::read_to_string(&doc_path).unwrap_or_default();
    // Same pre-edit snapshot stack the TUI pushes: CLI chats are undoable too.
    let _ = crate::undo::push(dirs, &slug, kind.label(), &doc_before);

    // Plan targets carry the spec path so a missing plan drafts from it.
    let spec_path = (kind == stages::DocKind::Plan && dirs.spec_file(&slug).exists())
        .then(|| dirs.spec_file(&slug));
    let invariants = stages::meaningful_invariants(dirs);
    let cmd = stages::doc_chat_command(
        cfg,
        &doc_path,
        kind,
        &scope,
        message,
        "",
        spec_path.as_deref(),
        invariants.as_deref(),
    );
    let stage_label = format!("{}-chat", kind.label());
    let req = RunRequest {
        agent: cmd.agent,
        argv: cmd.argv,
        env: cmd.env,
        stage: stage_label.clone(),
        feature: title,
        branch: branch.clone(),
        redact: cfg.redaction,
        repro: None, // chat edits are frequent + small, so skip provenance probes
        cwd: dirs.work_root.clone(),
        wrapper: stages::wrapper_argv(cfg, cmd.mode),
    };
    let run_id = runner::new_run_id(&stage_label);
    runner::spawn_detached(dirs, &req, &run_id)?;
    println!("chat {run_id} started (detached, survives this terminal)");

    let outcome = follow_run(cfg, dirs, req.agent, &run_id)?;

    if !outcome.meta.ok {
        anyhow::bail!("chat edit failed; see the stream above");
    }

    // Done iff the document actually changed to something meaningful; never
    // downgrade a stage that was already further along.
    let content = std::fs::read_to_string(&doc_path).unwrap_or_default();
    if content != doc_before && crate::spec::has_meaningful_content(&content) {
        set_stage(&mut st, &branch, stage_id, StageStatus::Done, Some(run_id));
        st.save(dirs)?;
        println!("{} updated ({})", kind.label(), doc_path.display());
    } else {
        println!("no change to {} ({})", kind.label(), doc_path.display());
    }
    Ok(())
}

fn mtime(p: &Path) -> Option<std::time::SystemTime> {
    std::fs::metadata(p).and_then(|m| m.modified()).ok()
}

/// Finalize a coverage run: parse the judge's report, tick the satisfied
/// deliverables into `plan.md` (confined to the section, undo-pushed), and
/// return Done iff zero gaps remain, else NeedsAttention.
/// The set of `-coverage.json` filenames currently in the findings dir.
fn coverage_files(dir: &Path) -> std::collections::HashSet<String> {
    let mut out = std::collections::HashSet::new();
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let n = e.file_name().to_string_lossy().into_owned();
            if n.ends_with("-coverage.json") {
                out.insert(n);
            }
        }
    }
    out
}

fn finalize_coverage(dirs: &RitualDirs, branch: &str, new_findings: &[String]) -> StageStatus {
    let (status, msgs) = crate::coverage::finalize(dirs, branch, new_findings);
    for m in msgs {
        println!("{m}");
    }
    status
}

fn check_green(work_root: &Path, timeout_secs: u64) -> bool {
    let check = work_root.join("check.sh");
    if !check.exists() {
        return false;
    }
    let mut cmd = std::process::Command::new("./check.sh");
    cmd.current_dir(work_root)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    run_with_timeout(cmd, timeout_secs)
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Spawn, poll, and kill on deadline: a hung check.sh (wedged build, dead
/// board on a HIL rig) must never wedge the pipeline. None = timeout/error.
pub(crate) fn run_with_timeout(
    mut cmd: std::process::Command,
    secs: u64,
) -> Option<std::process::ExitStatus> {
    let mut child = cmd.spawn().ok()?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(secs);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Some(status),
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            Err(_) => return None,
        }
    }
}

fn list_findings(dir: &Path) -> Vec<String> {
    std::fs::read_dir(dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dirs_with_run(cost: f64, when: chrono::DateTime<Utc>) -> (tempfile::TempDir, RitualDirs) {
        let tmp = tempfile::tempdir().unwrap();
        let runs = tmp.path().join(".ritual/runs");
        std::fs::create_dir_all(&runs).unwrap();
        std::fs::write(
            runs.join("20260711T000000Z-x.meta.json"),
            format!(
                r#"{{"run_id":"r","stage":"plan-review","ok":true,"total_cost_usd":{cost},"started_at":"{}"}}"#,
                when.to_rfc3339()
            ),
        )
        .unwrap();
        let dirs = RitualDirs::new(tmp.path());
        (tmp, dirs)
    }

    #[test]
    fn budget_none_when_unset() {
        let (_t, dirs) = dirs_with_run(99.0, Utc::now());
        let cfg = Config {
            budget_daily_usd: None,
            ..Config::default()
        };
        assert_eq!(budget_exceeded(&cfg, &dirs), None);
    }

    #[test]
    fn budget_none_when_under_ceiling() {
        let (_t, dirs) = dirs_with_run(0.50, Utc::now());
        let cfg = Config {
            budget_daily_usd: Some(5.0),
            ..Config::default()
        };
        assert_eq!(budget_exceeded(&cfg, &dirs), None);
    }

    #[test]
    fn budget_trips_when_at_or_over_ceiling() {
        let (_t, dirs) = dirs_with_run(6.0, Utc::now());
        let cfg = Config {
            budget_daily_usd: Some(5.0),
            ..Config::default()
        };
        let (spent, budget) = budget_exceeded(&cfg, &dirs).expect("should trip");
        assert_eq!(budget, 5.0);
        assert!(spent >= 5.0);
    }

    #[test]
    fn budget_ignores_yesterdays_spend() {
        let yesterday = Utc::now() - chrono::Duration::days(1);
        let (_t, dirs) = dirs_with_run(100.0, yesterday);
        let cfg = Config {
            budget_daily_usd: Some(5.0),
            ..Config::default()
        };
        // Only today's spend counts toward the daily ceiling.
        assert_eq!(budget_exceeded(&cfg, &dirs), None);
    }

    #[test]
    fn set_stage_records_timestamps_and_run_ids() {
        let mut st = State::default();
        set_stage(
            &mut st,
            "main",
            StageId::PlanReview,
            StageStatus::Running,
            None,
        );
        let running = st.features["main"].stage(StageId::PlanReview);
        assert_eq!(running.status, StageStatus::Running);
        assert!(running.started_at.is_some());
        assert!(running.finished_at.is_none());
        assert!(running.runs.is_empty());

        set_stage(
            &mut st,
            "main",
            StageId::PlanReview,
            StageStatus::Done,
            Some("run-42".into()),
        );
        let done = st.features["main"].stage(StageId::PlanReview);
        assert_eq!(done.status, StageStatus::Done);
        assert!(done.finished_at.is_some());
        assert_eq!(done.runs, vec!["run-42".to_string()]);
    }

    #[test]
    fn timeout_returns_status_for_fast_command() {
        let mut cmd = std::process::Command::new("true");
        let status = run_with_timeout(cmd, 5).expect("fast command should complete");
        assert!(status.success());
        // The `mut` binding is consumed by run_with_timeout; re-bind to prove
        // a failing command reports non-success rather than None.
        cmd = std::process::Command::new("false");
        let status = run_with_timeout(cmd, 5).expect("completes, just non-zero");
        assert!(!status.success());
    }

    #[test]
    fn timeout_kills_a_hung_command() {
        let mut cmd = std::process::Command::new("sleep");
        cmd.arg("30");
        // 0s deadline: the first poll is already past due, so it is killed.
        assert!(run_with_timeout(cmd, 0).is_none());
    }
}
