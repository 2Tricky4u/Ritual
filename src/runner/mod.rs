pub mod claude;
pub mod codex;
pub mod events;

use std::path::PathBuf;
use std::process::Stdio;

use anyhow::{Context, Result};
use chrono::Utc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::sync::mpsc;

use crate::history::RunMeta;
use crate::runner::events::AgentEvent;
use crate::state::RitualDirs;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentKind {
    Claude,
    // Constructed once direct `codex exec` stages land (post-fixture capture).
    #[allow(dead_code)]
    Codex,
}

impl AgentKind {
    pub fn label(&self) -> &'static str {
        match self {
            AgentKind::Claude => "claude",
            AgentKind::Codex => "codex",
        }
    }

    fn parse(&self, line: &str) -> Vec<AgentEvent> {
        match self {
            AgentKind::Claude => claude::parse_line(line),
            AgentKind::Codex => codex::parse_line(line),
        }
    }
}

#[derive(Debug, Clone)]
pub struct RunRequest {
    pub agent: AgentKind,
    pub argv: Vec<String>,
    pub env: Vec<(String, String)>,
    pub stage: String,
    pub feature: String,
    pub branch: String,
}

#[derive(Debug)]
pub struct RunOutcome {
    pub meta: RunMeta,
    #[allow(dead_code)] // consumed by the TUI (M3)
    pub archive: PathBuf,
}

/// Spawn a headless agent run. Events arrive on the returned channel; the
/// raw stream is archived verbatim to `.ritual/runs/<run_id>.jsonl` BEFORE
/// parsing, and a `<run_id>.meta.json` summary is written when it exits.
pub async fn run_headless(
    dirs: &RitualDirs,
    req: RunRequest,
    tx: mpsc::Sender<AgentEvent>,
) -> Result<RunOutcome> {
    let run_id = format!("{}-{}", Utc::now().format("%Y%m%dT%H%M%SZ"), req.stage);
    let runs_dir = dirs.runs_dir();
    tokio::fs::create_dir_all(&runs_dir).await?;
    let archive_path = runs_dir.join(format!("{run_id}.jsonl"));
    let mut archive = tokio::fs::File::create(&archive_path)
        .await
        .with_context(|| format!("creating {}", archive_path.display()))?;

    let mut meta = RunMeta {
        run_id: run_id.clone(),
        stage: req.stage.clone(),
        feature: req.feature.clone(),
        branch: req.branch.clone(),
        agent: req.agent.label().into(),
        argv: req.argv.clone(),
        started_at: Some(Utc::now()),
        ..Default::default()
    };

    let (bin, args) = req.argv.split_first().context("empty argv for agent run")?;
    let mut cmd = Command::new(bin);
    cmd.args(args)
        .current_dir(&dirs.project_root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .process_group(0);
    for (k, v) in &req.env {
        cmd.env(k, v);
    }
    let mut child = cmd
        .spawn()
        .with_context(|| format!("spawning {bin} — is it installed and on PATH?"))?;

    let stdout = child.stdout.take().context("no stdout")?;
    let stderr = child.stderr.take().context("no stderr")?;

    // stderr -> events, concurrently with stdout.
    let tx_err = tx.clone();
    let stderr_task = tokio::spawn(async move {
        let mut lines = BufReader::new(stderr).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            if line.trim().is_empty() {
                continue;
            }
            let _ = tx_err.send(AgentEvent::Stderr { line }).await;
        }
    });

    let mut lines = BufReader::new(stdout).lines();
    while let Some(line) = lines.next_line().await? {
        // Archive verbatim first — schema drift must never lose data.
        archive.write_all(line.as_bytes()).await?;
        archive.write_all(b"\n").await?;
        for ev in req.agent.parse(&line) {
            harvest(&mut meta, &ev);
            let _ = tx.send(ev).await;
        }
    }
    archive.flush().await?;

    let status = child.wait().await?;
    let _ = stderr_task.await;

    meta.finished_at = Some(Utc::now());
    meta.exit_code = status.code();
    meta.ok = status.success() && meta.error.is_none() && meta.completed_ok();

    let meta_path = runs_dir.join(format!("{run_id}.meta.json"));
    tokio::fs::write(&meta_path, serde_json::to_string_pretty(&meta)?)
        .await
        .with_context(|| format!("writing {}", meta_path.display()))?;

    Ok(RunOutcome {
        meta,
        archive: archive_path,
    })
}

/// Pull meta-worthy facts out of the event stream as it flows.
fn harvest(meta: &mut RunMeta, ev: &AgentEvent) {
    match ev {
        AgentEvent::SessionStart {
            session_id, model, ..
        } => {
            if !session_id.is_empty() {
                meta.session_id = Some(session_id.clone());
            }
            if !model.is_empty() {
                meta.model = Some(model.clone());
            }
        }
        AgentEvent::RateLimit(info) => meta.rate_limit = Some(info.clone()),
        AgentEvent::Completed {
            ok,
            result_text,
            total_cost_usd,
            usage,
            num_turns,
            duration_ms,
            permission_denials,
        } => {
            meta.total_cost_usd = *total_cost_usd;
            meta.usage = usage.clone();
            meta.num_turns = *num_turns;
            meta.duration_ms = *duration_ms;
            meta.permission_denials = permission_denials.clone();
            if !ok {
                meta.error = Some(
                    result_text
                        .clone()
                        .unwrap_or_else(|| "agent reported failure".into()),
                );
            }
        }
        _ => {}
    }
}

impl RunMeta {
    /// True if we saw a Completed{ok:true}; a stream that never completed
    /// (killed, crashed) is not ok even when the exit code is 0.
    fn completed_ok(&self) -> bool {
        self.duration_ms.is_some() || self.usage.is_some() || self.total_cost_usd.is_some()
    }
}
