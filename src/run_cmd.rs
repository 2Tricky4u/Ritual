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
    let cmd = stages::build(
        stage,
        cfg,
        dirs,
        &slug,
        arg,
        model,
        session.as_deref(),
        None,
    )?;

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
                let cmd =
                    stages::build(StageId::Coverage, cfg, dirs, &slug, None, None, None, None)?;
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
            let mut report =
                crate::coverage::latest_report(&dirs.findings_dir(), &slug).unwrap_or_default();
            // An unchecked deliverable the judge silently skipped is an unverified
            // gap, not a pass: drive it like any other gap.
            let plan_text = std::fs::read_to_string(dirs.plan_file(&slug)).unwrap_or_default();
            crate::coverage::reconcile_missing(&mut report, &plan_text);
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
    let plan_text = std::fs::read_to_string(dirs.plan_file(&slug)).unwrap_or_default();
    let mut report = crate::coverage::latest_report(&dirs.findings_dir(), &slug);
    // Fold in the deliverables the judge silently dropped BEFORE deciding clean.
    if let Some(r) = &mut report {
        crate::coverage::reconcile_missing(r, &plan_text);
    }
    let coverage_clean = report.as_ref().is_some_and(|r| r.gaps.is_empty());
    let deliverables = crate::spec::deliverables_gate(&plan_text);
    let deliverables_ok = deliverables.is_ok();
    let no_open = !crate::findings::has_open_confirmed(&findings, &slug);
    // Only spend a check.sh run once coverage + deliverables pass.
    let green =
        coverage_clean && deliverables_ok && check_green(&dirs.work_root, cfg.check_timeout_secs);
    let done = crate::coverage::feature_complete(coverage_clean, deliverables_ok, no_open, green);

    if done {
        println!(
            "✓ complete: all deliverables satisfied, check.sh green, no open confirmed findings"
        );
        // Close the loop: refresh the architecture map while the feature's
        // context is hot, so the NEXT feature's plan is grounded in
        // post-feature reality. Never on --check (the token-free CI gate),
        // never a completion failure (the map is advisory).
        if !check && cfg.architect_auto_refresh {
            if cfg.offline || budget_exceeded(cfg, dirs).is_some() {
                println!("architecture.md refresh skipped (offline/daily budget)");
            } else if let Err(e) = architect(cfg, dirs) {
                println!("warning: architecture.md refresh failed: {e:#}");
            }
        }
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
        // promises ritual FAILS the batch - enforce it. The snapshot is
        // fail-closed (a git error aborts rather than gating against the
        // wrong base); outside a work tree there is nothing to enforce.
        let in_git = crate::git::in_work_tree(&dirs.work_root);
        let snap = in_git
            .then(|| crate::git::snapshot(&dirs.work_root, &[]))
            .transpose()
            .context("snapshotting before the code-fix (fail closed)")?;
        let cmd = stages::findings_code_fix_command(cfg, &briefs, invariants.as_deref());
        run_fix(cfg, dirs, branch, title, "code-fix", cmd)?;
        if let Some(snap) = snap
            && crate::git::head_moved(&dirs.work_root, &snap)
        {
            anyhow::bail!(
                "the code-fix agent moved HEAD (commit/reset); batch rejected. \
                 Inspect with `git reflog`, restore, then rerun `ritual complete`"
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
        // The bare Edit/Write grant has NO path lock (dontAsk never matched
        // the scoped rules), so scoping is enforced ritual-side, mirroring
        // the TUI batch: undo snapshot before, containment gate after -
        // any change OUTSIDE plan.md rejects the batch, and a checklist item
        // the agent ticked itself ([ ]->[x]) reverts the plan (checked items
        // are TRUSTED by the coverage reconcile - self-certification would
        // fabricate completeness). In-plan drift is caught by the re-judge.
        let before = std::fs::read_to_string(&plan_path).unwrap_or_default();
        let _ = crate::undo::push(dirs, slug, stages::DocKind::Plan.label(), &before);
        let in_git = crate::git::in_work_tree(&dirs.work_root);
        let plan_rel = plan_path
            .strip_prefix(&dirs.work_root)
            .unwrap_or(&plan_path)
            .to_path_buf();
        let snap = in_git
            .then(|| crate::git::snapshot(&dirs.work_root, std::slice::from_ref(&plan_rel)))
            .transpose()
            .context("snapshotting before the plan-fix (fail closed)")?;
        let cmd = stages::findings_batch_fix_command(
            cfg,
            &plan_path,
            &steps,
            &briefs,
            Some(&spec),
            invariants.as_deref(),
        );
        run_fix(cfg, dirs, branch, title, "plan-fix", cmd)?;
        if let Some(snap) = snap {
            let change = crate::git::observed_change(&dirs.work_root, &snap)
                .context("verifying the plan-fix change (fail closed)")?;
            let leaked: Vec<String> = crate::git::changed_paths(&change)
                .into_iter()
                .filter(|p| *p != plan_rel)
                .map(|p| p.display().to_string())
                .collect();
            if !leaked.is_empty() {
                let _ = crate::undo::undo(dirs, slug, stages::DocKind::Plan.label(), &plan_path);
                anyhow::bail!(
                    "the plan-fix agent touched files outside plan.md ({}); \
                     plan reverted - inspect the tree before rerunning `ritual complete`",
                    leaked.join(", ")
                );
            }
        }
        let after = std::fs::read_to_string(&plan_path).unwrap_or_default();
        if let Some(item) = crate::complete::illegal_tick(&before, &after) {
            let _ = crate::undo::undo(dirs, slug, stages::DocKind::Plan.label(), &plan_path);
            println!(
                "  plan-fix tried to self-certify a deliverable (ticked \"{item}\"); \
                 plan reverted, gap stays open"
            );
        }
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
    stages::ensure_online(cfg)?;
    let req = RunRequest {
        agent: cmd.agent,
        argv: cmd.argv,
        env: cmd.env,
        stdin: cmd.stdin,
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

/// `ritual audit [--discover] [--lanes-file <p>]`: the optional whole-project
/// review. Blind lanes run in PARALLEL (spawned detached first, followed one
/// by one - wall clock tracks the slowest lane), then an adversarial
/// cross-vendor judge adjudicates every candidate and writes standard
/// findings (stage "audit"). Deliberately NOT a pipeline stage: state.json
/// and `ritual status` are untouched; findings triage via the normal tab.
pub fn audit(
    cfg: &Config,
    dirs: &RitualDirs,
    discover: bool,
    lanes_file: Option<&Path>,
) -> Result<()> {
    anyhow::ensure!(dirs.exists(), "no .ritual/ here; run `ritual init` first");
    stages::ensure_online(cfg)?;
    if let Some((spent, budget)) = budget_exceeded(cfg, dirs) {
        anyhow::bail!(
            "daily budget reached (${spent:.2}/${budget:.2}); raise budget_daily_usd to override"
        );
    }
    let branch = state::current_branch(&dirs.work_root).unwrap_or_else(|| "detached".to_string());
    // Resolve a relative --lanes-file against the PROJECT ROOT once: the
    // discovery agent's cwd is work_root, while this process may run from a
    // subdirectory - two different bases would write one file and read
    // another ("discovery finished but wrote no lanes", money spent).
    let lanes_path = lanes_file
        .map(|p| {
            if p.is_absolute() {
                p.to_path_buf()
            } else {
                dirs.work_root.join(p)
            }
        })
        .unwrap_or_else(|| dirs.audit_lanes_file());

    // One audit at a time: lanes are detached daemons, so Ctrl-C leaves them
    // running (and billing) - a rerun would silently double the spend.
    let live_audit: Vec<String> = runner::live_runs(dirs)
        .into_iter()
        .filter(|(_, s)| s.stage.starts_with("audit"))
        .map(|(id, _)| id)
        .collect();
    anyhow::ensure!(
        live_audit.is_empty(),
        "an audit is already running ({} live leg(s), e.g. `ritual attach {}`); \
         wait for it or kill the legs (`ritual ps`) before starting another",
        live_audit.len(),
        live_audit[0]
    );

    if discover {
        // Never silently destroy a hand-curated lanes file.
        if lanes_path.exists() {
            let backup = lanes_path.with_extension("md.bak");
            std::fs::copy(&lanes_path, &backup)
                .with_context(|| format!("backing up {}", lanes_path.display()))?;
            println!("existing lanes file backed up to {}", backup.display());
        }
        let cmd = stages::audit_discover_command(cfg, &lanes_path);
        let outcome = oneshot_leg(cfg, dirs, &branch, "audit", "audit-discover", cmd)?;
        anyhow::ensure!(
            outcome.meta.ok,
            "discovery run failed: {}",
            crate::history::decode_failure(&outcome.meta)
        );
        let text = std::fs::read_to_string(&lanes_path).unwrap_or_default();
        let lanes = crate::audit::parse_lanes(&text);
        anyhow::ensure!(
            !lanes.is_empty(),
            "discovery finished but wrote no lanes to {}",
            lanes_path.display()
        );
        println!(
            "discovered {} lane(s) in {}:",
            lanes.len(),
            lanes_path.display()
        );
        for l in &lanes {
            println!("  ## {}", l.name);
        }
        println!("review/edit the file, then run `ritual audit`.");
        return Ok(());
    }

    let text = std::fs::read_to_string(&lanes_path).map_err(|_| {
        anyhow::anyhow!(
            "no lanes file at {}; run `ritual audit --discover` first (or write `## <lane>` headings by hand)",
            lanes_path.display()
        )
    })?;
    let parsed = crate::audit::parse_lanes(&text);
    anyhow::ensure!(
        !parsed.is_empty(),
        "no lanes defined in {}; run `ritual audit --discover` or add `## <lane>` headings",
        lanes_path.display()
    );
    let sel = crate::audit::select_lanes(parsed, cfg.audit_max_lanes);
    if sel.truncated > 0 {
        println!(
            "warning: {} lane(s) beyond audit_max_lanes={} were skipped",
            sel.truncated, cfg.audit_max_lanes
        );
    }
    // The judge leg is cross-vendor by contract: fail BEFORE paying for lanes.
    if !cfg.offline && !stages::codex_ready(cfg) {
        anyhow::bail!(
            "codex is not authenticated. Run `codex login` first (the audit judge verifies findings via Codex)"
        );
    }

    let reports_dir = dirs
        .audit_dir()
        .join(chrono::Utc::now().format("%Y%m%dT%H%M%SZ").to_string());
    std::fs::create_dir_all(&reports_dir)?;
    let invariants = stages::meaningful_invariants(dirs);
    let names: Vec<String> = sel.lanes.iter().map(|l| l.name.clone()).collect();

    // Spawn EVERY lane daemon first - they execute concurrently - then follow
    // each to completion (order here is display-only).
    let mut legs: Vec<(String, std::path::PathBuf, String)> = Vec::new();
    for lane in &sel.lanes {
        let others: Vec<&str> = names
            .iter()
            .filter(|n| *n != &lane.name)
            .map(String::as_str)
            .collect();
        let slug = state::branch_slug(&lane.name);
        let report = reports_dir.join(format!("{slug}.md"));
        let cmd = stages::audit_lane_command(cfg, lane, &others, &report, invariants.as_deref());
        let stage_label = format!("audit-lane-{slug}");
        let req = RunRequest {
            agent: cmd.agent,
            argv: cmd.argv,
            env: cmd.env,
            stdin: cmd.stdin,
            stage: stage_label.clone(),
            feature: "audit".into(),
            branch: branch.clone(),
            redact: cfg.redaction,
            repro: None,
            cwd: dirs.work_root.clone(),
            wrapper: stages::wrapper_argv(cfg, cmd.mode),
        };
        let run_id = runner::new_run_id(&stage_label);
        runner::spawn_detached(dirs, &req, &run_id)?;
        legs.push((run_id, report, lane.name.clone()));
    }
    println!(
        "audit: {} lane(s) running in parallel (reports in {})",
        legs.len(),
        reports_dir.display()
    );

    // Collect in SELECTED LANE ORDER (deterministic judge input, whatever the
    // completion order was). A lane's report file is the sole success
    // criterion: a failed run that still left a report is used (warned).
    let mut used: Vec<(String, std::path::PathBuf)> = Vec::new();
    let mut failed = 0usize;
    for (run_id, report, name) in &legs {
        let run_ok = match follow_run(cfg, dirs, runner::AgentKind::Claude, run_id) {
            Ok(o) => o.meta.ok,
            Err(e) => {
                println!("warning: lane '{name}' run error: {e:#}");
                false
            }
        };
        let usable = std::fs::metadata(report)
            .map(|m| m.len() > 0)
            .unwrap_or(false);
        match (usable, run_ok) {
            (true, true) => used.push((name.clone(), report.clone())),
            (true, false) => {
                println!("warning: lane '{name}' run failed but left a report; using it");
                used.push((name.clone(), report.clone()));
            }
            (false, _) => {
                println!("warning: lane '{name}' produced no report; skipping it");
                failed += 1;
            }
        }
    }
    anyhow::ensure!(
        !used.is_empty(),
        "every lane failed to produce a report; nothing to judge"
    );

    let mut payload = String::new();
    for (name, report) in &used {
        // LOSSY read: one non-UTF8 byte in a report must not abort the audit
        // AFTER every lane's budget is already spent.
        let text = std::fs::read(report)
            .map(|b| String::from_utf8_lossy(&b).into_owned())
            .with_context(|| format!("reading lane report {}", report.display()))?;
        payload.push_str(&format!("\n\n===== LANE REPORT: {name} =====\n{text}"));
    }

    let findings_before = list_findings(&dirs.findings_dir());
    let mut cmd = stages::audit_judge_command(cfg, &dirs.findings_dir(), used.len(), payload);
    cmd.env.push((
        "RITUAL_FINDINGS_DIR".to_string(),
        dirs.findings_dir().display().to_string(),
    ));
    let outcome = oneshot_leg(cfg, dirs, &branch, "audit", "audit-judge", cmd)?;
    anyhow::ensure!(
        outcome.meta.ok,
        "audit judge failed: {}",
        crate::history::decode_failure(&outcome.meta)
    );
    // Only files matching the judge's naming contract: the findings dir is
    // shared (worktrees, concurrent runs), and stamping/counting a foreign
    // findings file that landed during the judge window would mis-scope it
    // to this branch and inflate the summary.
    let new_findings: Vec<String> = list_findings(&dirs.findings_dir())
        .into_iter()
        .filter(|f| !findings_before.contains(f))
        .filter(|f| f.ends_with("-audit.json"))
        .collect();
    // An empty findings LIST is valid; a missing findings FILE is a breach of
    // the judge contract - fail loudly, never a silent "all clean".
    anyhow::ensure!(
        !new_findings.is_empty(),
        "judge finished ok but wrote no findings file; inspect `ritual attach {}`",
        outcome.meta.run_id
    );
    crate::findings::stamp_branch(&dirs.findings_dir(), &new_findings, &branch);

    let (mut confirmed, mut unconfirmed) = (0usize, 0usize);
    for name in &new_findings {
        if let Ok(t) = std::fs::read_to_string(dirs.findings_dir().join(name))
            && let Ok(file) = serde_json::from_str::<crate::findings::FindingsFile>(&t)
        {
            for f in &file.findings {
                if crate::findings::verdict_confirmed(&f.verdict) {
                    confirmed += 1;
                } else {
                    unconfirmed += 1;
                }
            }
        }
    }
    println!(
        "audit done: {} lane(s) used, {failed} failed · {confirmed} confirmed + {unconfirmed} unconfirmed finding(s) → {}",
        used.len(),
        new_findings.join(", ")
    );
    println!("triage in the TUI findings tab (t = recommended triage, A/F = queue + fix).");
    Ok(())
}

/// Spawn one one-shot leg detached and follow it (audit discovery/judge,
/// architect). Non-pipeline: the run archives + bills normally but never
/// touches state.json.
fn oneshot_leg(
    cfg: &Config,
    dirs: &RitualDirs,
    branch: &str,
    feature: &str,
    stage_label: &str,
    cmd: stages::StageCommand,
) -> Result<runner::RunOutcome> {
    let req = RunRequest {
        agent: cmd.agent,
        argv: cmd.argv,
        env: cmd.env,
        stdin: cmd.stdin,
        stage: stage_label.into(),
        feature: feature.into(),
        branch: branch.into(),
        redact: cfg.redaction,
        repro: None,
        cwd: dirs.work_root.clone(),
        wrapper: stages::wrapper_argv(cfg, cmd.mode),
    };
    let run_id = runner::new_run_id(stage_label);
    runner::spawn_detached(dirs, &req, &run_id)?;
    follow_run(cfg, dirs, req.agent, &run_id)
}

/// `ritual architect`: survey the tree with one budgeted headless run and
/// install/refresh `.ritual/architecture.md` via the candidate protocol
/// ([`crate::architect::finalize`]). Non-pipeline like audit: never touches
/// state.json.
pub fn architect(cfg: &Config, dirs: &RitualDirs) -> Result<()> {
    anyhow::ensure!(dirs.exists(), "no .ritual/ here; run `ritual init` first");
    stages::ensure_online(cfg)?;
    if let Some((spent, budget)) = budget_exceeded(cfg, dirs) {
        anyhow::bail!(
            "daily budget reached (${spent:.2}/${budget:.2}); raise budget_daily_usd to override"
        );
    }
    // One architect at a time: the leg is a detached daemon, so Ctrl-C leaves
    // it running (and billing) - a rerun would race it for the same paths.
    let live: Vec<String> = runner::live_runs(dirs)
        .into_iter()
        .filter(|(_, s)| s.stage.starts_with("architect"))
        .map(|(id, _)| id)
        .collect();
    anyhow::ensure!(
        live.is_empty(),
        "an architect run is already running (`ritual attach {}`); \
         wait for it or kill it (`ritual ps`) before starting another",
        live[0]
    );

    let branch = state::current_branch(&dirs.work_root).unwrap_or_else(|| "detached".to_string());
    let candidate = dirs.architecture_candidate_file();
    // Debris from an earlier crashed run must never pass for THIS run's
    // output - finalize validates the candidate, so a stale one would lie.
    let _ = std::fs::remove_file(&candidate);

    let invariants = stages::meaningful_invariants(dirs);
    let cmd = stages::architect_command(cfg, &candidate, invariants.as_deref());
    let outcome = oneshot_leg(cfg, dirs, &branch, "architect", "architect", cmd)?;
    if !outcome.meta.ok {
        // finalize(false) cleans the candidate; the richer failure decode
        // (budget knob, denials) beats its generic message.
        let _ = crate::architect::finalize(dirs, false);
        anyhow::bail!(
            "architect run failed: {}",
            crate::history::decode_failure(&outcome.meta)
        );
    }
    let tracked = crate::architect::finalize(dirs, true)?;
    println!(
        "architecture map written to {}",
        dirs.architecture_file().display()
    );
    if crate::architect::backup_file(dirs).exists() {
        println!(
            "previous map backed up to {}",
            crate::architect::backup_file(dirs).display()
        );
    }
    if !tracked {
        println!("(not a git tree: staleness tracking disabled)");
    }
    if let Some(cost) = outcome.meta.total_cost_usd {
        println!("cost: ${cost:.2}");
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

/// Reload-merge-save: a headless run holds its `State` snapshot for minutes,
/// so the delta must be folded into a FRESH load - saving the stale snapshot
/// would clobber whatever the TUI (or another CLI command) wrote meanwhile.
/// Parity with the TUI's `set_stage` (commit 40fd595 fixed only that side).
/// On a load error the in-memory snapshot is kept (never lose our own delta).
#[allow(clippy::too_many_arguments)]
pub(crate) fn set_stage_persist(
    dirs: &RitualDirs,
    st: &mut State,
    branch: &str,
    stage: StageId,
    status: StageStatus,
    run_id: Option<String>,
    tree: Option<&Path>,
) -> Result<()> {
    if let Ok(fresh) = State::load(dirs) {
        *st = fresh;
    }
    set_stage(st, branch, stage, status, run_id, tree);
    st.save(dirs)
}

/// The single stage-status mutator (TUI and CLI both funnel here). `tree` is
/// the checkout the stage ran in: a TERMINAL status stamps its fingerprint
/// so guidance can flag Done-but-stale review stages; pass None where the
/// tree is unknown or has drifted since (the stamp is overwritten with None
/// then - a stale fingerprint must never lie).
pub(crate) fn set_stage(
    st: &mut State,
    branch: &str,
    stage: StageId,
    status: StageStatus,
    run_id: Option<String>,
    tree: Option<&Path>,
) {
    let feature = st.feature_for_branch_mut(branch);
    let entry = feature.stages.entry(stage).or_default();
    match status {
        StageStatus::Running => entry.started_at = Some(Utc::now()),
        _ => {
            entry.finished_at = Some(Utc::now());
            entry.fingerprint = tree.and_then(crate::provenance::tree_fingerprint);
        }
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
    set_stage(st, branch, StageId::Spec, new_status, None, None);
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
    stages::ensure_online(cfg)?;
    let slug = state::branch_slug(branch);
    let plan_mtime_before = mtime(&dirs.plan_file(&slug));

    set_stage(st, branch, stage, StageStatus::Running, None, None);
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
            if !status.success() {
                // A crashed/killed session wrote no reviewed red tests -
                // Done here would be a lie (the TUI marks the same exit
                // Failed; the two entry points must agree).
                println!("tests-red session exited with an error");
                StageStatus::Failed
            } else {
                if check_green(&dirs.work_root, cfg.check_timeout_secs) {
                    set_stage(
                        st,
                        branch,
                        StageId::Implement,
                        StageStatus::Done,
                        None,
                        Some(&dirs.work_root),
                    );
                    println!("check.sh green: tests-red and implement both done");
                } else {
                    println!("failing tests in place, ready to implement");
                }
                StageStatus::Done
            }
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
    set_stage(st, branch, stage, new_status, None, Some(&dirs.work_root));
    st.save(dirs)?;
    // Scriptability parity with the headless path: a crashed session is a
    // nonzero exit (NeedsAttention keeps 0 - the stage ran, work remains).
    anyhow::ensure!(
        new_status != StageStatus::Failed,
        "stage '{}' failed (session exited with an error)",
        stage.label()
    );
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
    stages::ensure_online(cfg)?;
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

    set_stage_persist(dirs, st, branch, stage, StageStatus::Running, None, None)?;

    let req = RunRequest {
        agent: cmd.agent,
        argv: cmd.argv,
        env: cmd.env,
        stdin: cmd.stdin,
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
    // Stamp the real branch onto the files this run produced so completeness
    // consumers can scope by branch (the skill's `branch` is untrusted). After
    // the new_status computation so it runs post-`finalize_coverage` (A3's tree
    // fingerprint, added at this same point, then reflects the post-tick tree).
    crate::findings::stamp_branch(&dirs.findings_dir(), &new_findings, branch);
    set_stage_persist(
        dirs,
        st,
        branch,
        stage,
        new_status,
        Some(outcome.meta.run_id.clone()),
        Some(&dirs.work_root),
    )?;

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
    stages::ensure_online(cfg)?;

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
        stdin: cmd.stdin,
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
        set_stage_persist(
            dirs,
            &mut st,
            &branch,
            stage_id,
            StageStatus::Done,
            Some(run_id),
            None,
        )?;
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
            None,
        );
        let done = st.features["main"].stage(StageId::PlanReview);
        assert_eq!(done.status, StageStatus::Done);
        assert!(done.finished_at.is_some());
        assert_eq!(done.runs, vec!["run-42".to_string()]);
    }

    #[test]
    fn set_stage_stamps_the_tree_fingerprint_on_terminal_status_only() {
        let tmp = tempfile::tempdir().unwrap();
        for args in [
            &["init", "-q", "-b", "main"][..],
            &[
                "-c",
                "user.email=t@t",
                "-c",
                "user.name=t",
                "commit",
                "-qm",
                "x",
                "--allow-empty",
            ][..],
        ] {
            std::process::Command::new("git")
                .args(args)
                .current_dir(tmp.path())
                .output()
                .unwrap();
        }
        let mut st = State::default();
        // Running never stamps.
        set_stage(
            &mut st,
            "main",
            StageId::DualReview,
            StageStatus::Running,
            None,
            Some(tmp.path()),
        );
        assert_eq!(
            st.features["main"].stage(StageId::DualReview).fingerprint,
            None
        );
        // Done in a git tree stamps "HEAD:digest".
        set_stage(
            &mut st,
            "main",
            StageId::DualReview,
            StageStatus::Done,
            None,
            Some(tmp.path()),
        );
        let fp = st.features["main"]
            .stage(StageId::DualReview)
            .fingerprint
            .expect("stamped on Done");
        assert!(fp.contains(':'), "{fp}");
        // A later terminal status with tree=None OVERWRITES to None (a stale
        // fingerprint must never lie).
        set_stage(
            &mut st,
            "main",
            StageId::DualReview,
            StageStatus::Failed,
            None,
            None,
        );
        assert_eq!(
            st.features["main"].stage(StageId::DualReview).fingerprint,
            None
        );
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
