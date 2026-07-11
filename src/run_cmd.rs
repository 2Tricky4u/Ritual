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

pub fn execute(cfg: &Config, dirs: &RitualDirs, stage_str: &str, arg: Option<&str>) -> Result<()> {
    let stage = StageId::parse(stage_str)
        .with_context(|| format!("unknown stage '{stage_str}' (spec, plan, plan-review, tests-red, implement, dual-review)"))?;
    anyhow::ensure!(dirs.exists(), "no .ritual/ here — run `ritual init` first");

    let branch =
        state::current_branch(&dirs.project_root).unwrap_or_else(|| "detached".to_string());
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
        Mode::Headless => run_headless(cfg, dirs, stage, cmd, &mut st, &branch, &slug, &title),
    }
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
        .current_dir(&dirs.project_root)
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
            if check_green(&dirs.project_root) {
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
            if check_green(&dirs.project_root) {
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
    _slug: &str,
    title: &str,
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
    };

    let rt = tokio::runtime::Runtime::new()?;
    let outcome = rt.block_on(async {
        let (tx, mut rx) = mpsc::channel(256);
        let dirs2 = dirs.clone();
        let handle = tokio::spawn(async move { runner::run_headless(&dirs2, req, tx).await });
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

    crate::output::render_run_summary(cfg, &outcome.meta, &new_findings);
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

fn check_green(project_root: &Path) -> bool {
    let check = project_root.join("check.sh");
    if !check.exists() {
        return false;
    }
    std::process::Command::new("./check.sh")
        .current_dir(project_root)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
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
