//! `ritual run <stage>` — non-TUI stage execution with live styled output.

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
) -> Result<()> {
    let stage = StageId::parse(stage_str)
        .with_context(|| format!("unknown stage '{stage_str}' (spec, plan, plan-review, tests-red, implement, dual-review)"))?;
    anyhow::ensure!(dirs.exists(), "no .ritual/ here — run `ritual init` first");

    if let Some((spent, budget)) = budget_exceeded(cfg, dirs)
        && !force
    {
        anyhow::bail!(
            "daily budget reached: ${spent:.2} of ${budget:.2} spent today — rerun with --force to override"
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

    let cmd = stages::build(stage, cfg, dirs, &slug, arg)?;

    if cmd.needs_codex && !stages::codex_ready(cfg) {
        anyhow::bail!(
            "codex is not authenticated — run `codex login` first (stage '{}' talks to Codex via MCP)",
            stage.label()
        );
    }

    match cmd.mode {
        Mode::Local => run_spec_stage(dirs, &slug, &title, &mut st, &branch),
        Mode::Interactive => run_interactive(dirs, stage, cmd.argv, &mut st, &branch),
        Mode::Headless => run_headless(cfg, dirs, stage, cmd, &mut st, &branch, &title, ci),
    }
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
    let meaningful = content
        .lines()
        .any(|l| !l.trim().is_empty() && !l.trim_start().starts_with(['#', '<']));
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
            "still empty — left pending"
        },
        spec.display()
    );
    Ok(())
}

fn run_interactive(
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
                    "plan.md unchanged — marking needs-attention (save the plan to {})",
                    dirs.plan_file(&slug).display()
                );
                StageStatus::NeedsAttention
            }
        }
        StageId::TestsRed => {
            if check_green(&dirs.work_root) {
                // /tdd went all the way to green: tests-red AND implement done.
                set_stage(st, branch, StageId::Implement, StageStatus::Done, None);
                println!("check.sh green — tests-red and implement both done");
                StageStatus::Done
            } else {
                println!("check.sh red — failing tests in place, ready to implement");
                StageStatus::Done
            }
        }
        StageId::Implement => {
            if check_green(&dirs.work_root) {
                StageStatus::Done
            } else {
                println!("check.sh still red — implement stays needs-attention");
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
    };

    // Daemonize, then follow along. Ctrl-C here leaves the run alive.
    let run_id = runner::new_run_id(stage.label());
    runner::spawn_detached(dirs, &req, &run_id)?;
    println!("run {run_id} started (detached — survives this terminal)");

    let rt = tokio::runtime::Runtime::new()?;
    let outcome = rt.block_on(async {
        let (tx, mut rx) = mpsc::channel(256);
        let dirs2 = dirs.clone();
        let rid = run_id.clone();
        let handle =
            tokio::spawn(async move { runner::tail_run(&dirs2, req.agent, &rid, tx).await });
        while let Some(ev) = rx.recv().await {
            crate::output::render_event(cfg, &ev);
        }
        handle.await?
    })?;

    let new_findings: Vec<String> = list_findings(&dirs.findings_dir())
        .into_iter()
        .filter(|f| !findings_before.contains(f))
        .collect();

    let new_status = if !outcome.meta.ok {
        StageStatus::Failed
    } else if new_findings.is_empty() {
        // Review stages must leave a findings artifact; an ok run without one
        // means the skill under-delivered (asked a question, hit a wall...).
        println!("run finished ok but wrote no findings file — needs attention");
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
            anyhow::bail!("{} blocking finding(s) — see JUnit report", junit.failures);
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
            "{} — {} new findings, ${:.2}",
            branch,
            new_findings.len(),
            outcome.meta.total_cost_usd.unwrap_or(0.0)
        ),
    );
    if !outcome.meta.permission_denials.is_empty() {
        println!(
            "  ⚠ permission denials: {} — tune allowedTools or permission mode",
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

fn mtime(p: &Path) -> Option<std::time::SystemTime> {
    std::fs::metadata(p).and_then(|m| m.modified()).ok()
}

fn check_green(work_root: &Path) -> bool {
    let check = work_root.join("check.sh");
    if !check.exists() {
        return false;
    }
    let mut cmd = std::process::Command::new("./check.sh");
    cmd.current_dir(work_root)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    run_with_timeout(cmd, Config::default().check_timeout_secs)
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Spawn, poll, and kill on deadline — a hung check.sh (wedged build, dead
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
