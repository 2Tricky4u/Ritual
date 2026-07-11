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

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
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

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RunRequest {
    pub agent: AgentKind,
    pub argv: Vec<String>,
    pub env: Vec<(String, String)>,
    pub stage: String,
    pub feature: String,
    pub branch: String,
    /// Redact secrets before archiving/parsing (config `redaction`).
    pub redact: bool,
    /// Reproducibility bundle collected by the caller (provenance::collect).
    pub repro: Option<crate::provenance::ReproBundle>,
    /// Where the agent runs — the (work)tree being operated on.
    pub cwd: PathBuf,
}

/// Liveness sidecar written by the detached executor (`<run_id>.status`).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RunStatus {
    pub pid: u32,
    pub stage: String,
    pub branch: String,
}

/// Where a run stands, judged purely from the filesystem.
#[derive(Debug)]
pub enum RunState {
    Running(RunStatus),
    Finished(Box<RunMeta>),
    /// No live pid and no meta — the daemon died before finishing.
    Vanished,
}

pub fn new_run_id(stage: &str) -> String {
    format!("{}-{}", Utc::now().format("%Y%m%dT%H%M%SZ"), stage)
}

fn status_path(dirs: &RitualDirs, run_id: &str) -> PathBuf {
    dirs.runs_dir().join(format!("{run_id}.status"))
}

fn request_path(dirs: &RitualDirs, run_id: &str) -> PathBuf {
    dirs.runs_dir().join(format!("{run_id}.request.json"))
}

pub fn pid_alive(pid: u32) -> bool {
    nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid as i32), None).is_ok()
}

/// Judge a run from its sidecar files.
pub fn run_state(dirs: &RitualDirs, run_id: &str) -> RunState {
    let meta_path = dirs.runs_dir().join(format!("{run_id}.meta.json"));
    if let Ok(text) = std::fs::read_to_string(&meta_path)
        && let Ok(meta) = serde_json::from_str::<RunMeta>(&text)
    {
        return RunState::Finished(Box::new(meta));
    }
    if let Ok(text) = std::fs::read_to_string(status_path(dirs, run_id))
        && let Ok(status) = serde_json::from_str::<RunStatus>(&text)
        && pid_alive(status.pid)
    {
        return RunState::Running(status);
    }
    RunState::Vanished
}

/// All runs that are currently alive (for TUI resurrection).
pub fn live_runs(dirs: &RitualDirs) -> Vec<(String, RunStatus)> {
    let Ok(rd) = std::fs::read_dir(dirs.runs_dir()) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in rd.filter_map(|e| e.ok()) {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(run_id) = name.strip_suffix(".status") else {
            continue;
        };
        if let Ok(text) = std::fs::read_to_string(entry.path())
            && let Ok(status) = serde_json::from_str::<RunStatus>(&text)
            && pid_alive(status.pid)
        {
            out.push((run_id.to_string(), status));
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

/// Detach a run: persist the request, re-exec `ritual _spawn <run_id>` in its
/// own session. The daemon writes the same archive/meta files the inline
/// runner always did — callers follow along with [`tail_run`].
pub fn spawn_detached(dirs: &RitualDirs, req: &RunRequest, run_id: &str) -> Result<()> {
    std::fs::create_dir_all(dirs.runs_dir())?;
    std::fs::write(
        request_path(dirs, run_id),
        serde_json::to_string_pretty(req)?,
    )?;

    let exe = std::env::current_exe().context("resolving ritual binary")?;
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dirs.logs_dir().join("daemon.log"))
        .ok();
    let mut cmd = std::process::Command::new(exe);
    cmd.arg("_spawn")
        .arg(run_id)
        .current_dir(&dirs.project_root)
        .stdin(Stdio::null())
        .stdout(
            log.as_ref()
                .and_then(|f| f.try_clone().ok())
                .map(Stdio::from)
                .unwrap_or_else(Stdio::null),
        )
        .stderr(log.map(Stdio::from).unwrap_or_else(Stdio::null));
    // New session: the daemon survives the TUI, the terminal, and SIGHUP.
    unsafe {
        use std::os::unix::process::CommandExt;
        cmd.pre_exec(|| {
            nix::unistd::setsid()
                .map(|_| ())
                .map_err(|e| std::io::Error::other(e.to_string()))
        });
    }
    let mut child = cmd.spawn().context("spawning ritual _spawn daemon")?;
    // Reap the direct child so it never lingers as a zombie under the TUI.
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

/// Daemon side: load the persisted request and execute it. Events go nowhere
/// — the archive on disk IS the stream.
pub async fn daemon_main(dirs: &RitualDirs, run_id: &str) -> Result<()> {
    let req: RunRequest = serde_json::from_str(
        &std::fs::read_to_string(request_path(dirs, run_id))
            .with_context(|| format!("no request file for {run_id}"))?,
    )?;
    let (tx, mut rx) = mpsc::channel::<AgentEvent>(64);
    // Drain silently; senders in execute_run ignore failures anyway.
    tokio::spawn(async move { while rx.recv().await.is_some() {} });
    execute_run(dirs, req, run_id, tx).await.map(|_| ())
}

/// Follow a (possibly detached) run: replay the archive from the top, stream
/// new lines as they land, finish when the meta appears or the daemon dies.
pub async fn tail_run(
    dirs: &RitualDirs,
    agent: AgentKind,
    run_id: &str,
    tx: mpsc::Sender<AgentEvent>,
) -> Result<RunOutcome> {
    let archive_path = dirs.runs_dir().join(format!("{run_id}.jsonl"));
    let mut offset: usize = 0;
    let mut carry = String::new();
    // The daemon needs a beat to exec and write its .status sidecar; until
    // we've seen it alive once (or the startup window passes), a missing
    // sidecar means "still starting", not "vanished".
    const STARTUP_GRACE: std::time::Duration = std::time::Duration::from_secs(20);
    let started = std::time::Instant::now();
    let mut seen_running = false;
    loop {
        // Stream any new complete lines.
        if let Ok(bytes) = tokio::fs::read(&archive_path).await
            && bytes.len() > offset
        {
            let chunk = String::from_utf8_lossy(&bytes[offset..]).into_owned();
            offset = bytes.len();
            carry.push_str(&chunk);
            while let Some(nl) = carry.find('\n') {
                let line: String = carry.drain(..=nl).collect();
                for ev in agent.parse(line.trim_end()) {
                    let _ = tx.send(ev).await;
                }
            }
        }
        match run_state(dirs, run_id) {
            RunState::Finished(meta) => {
                return Ok(RunOutcome {
                    meta: *meta,
                    archive: archive_path,
                });
            }
            RunState::Running(_) => seen_running = true,
            RunState::Vanished => {
                if !seen_running && started.elapsed() < STARTUP_GRACE {
                    // Daemon still booting — keep polling.
                } else {
                    // Meta may lag the process death by a beat.
                    tokio::time::sleep(std::time::Duration::from_millis(400)).await;
                    if let RunState::Finished(meta) = run_state(dirs, run_id) {
                        return Ok(RunOutcome {
                            meta: *meta,
                            archive: archive_path,
                        });
                    }
                    anyhow::bail!("run {run_id} vanished (daemon died before writing meta)");
                }
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
}

/// SIGTERM the daemon's whole process group (daemon + agent).
pub fn kill_run(dirs: &RitualDirs, run_id: &str) -> bool {
    if let RunState::Running(status) = run_state(dirs, run_id) {
        let pgid = nix::unistd::Pid::from_raw(-(status.pid as i32));
        return nix::sys::signal::kill(pgid, nix::sys::signal::Signal::SIGTERM).is_ok();
    }
    false
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
pub async fn execute_run(
    dirs: &RitualDirs,
    req: RunRequest,
    run_id: &str,
    tx: mpsc::Sender<AgentEvent>,
) -> Result<RunOutcome> {
    let run_id = run_id.to_string();
    let runs_dir = dirs.runs_dir();
    tokio::fs::create_dir_all(&runs_dir).await?;

    // Liveness sidecar for tailers/resurrection; removed once meta lands.
    let status_file = status_path(dirs, &run_id);
    let _ = std::fs::write(
        &status_file,
        serde_json::to_string(&RunStatus {
            pid: std::process::id(),
            stage: req.stage.clone(),
            branch: req.branch.clone(),
        })?,
    );

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
        repro: req.repro.clone(),
        ..Default::default()
    };

    let (bin, args) = req.argv.split_first().context("empty argv for agent run")?;
    let mut cmd = Command::new(bin);
    cmd.args(args)
        .current_dir(&req.cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    for (k, v) in &req.env {
        cmd.env(k, v);
    }
    let mut child = cmd
        .spawn()
        .with_context(|| format!("spawning {bin} — is it installed and on PATH?"))?;

    let stdout = child.stdout.take().context("no stdout")?;
    let stderr = child.stderr.take().context("no stderr")?;

    // stderr -> events, concurrently with stdout (own redactor state).
    let tx_err = tx.clone();
    let redact_stderr = req.redact;
    let stderr_task = tokio::spawn(async move {
        let mut redactor = crate::redact::Redactor::new(redact_stderr);
        let mut lines = BufReader::new(stderr).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            if line.trim().is_empty() {
                continue;
            }
            let line = redactor.line(&line);
            let _ = tx_err.send(AgentEvent::Stderr { line }).await;
        }
    });

    // Redaction happens BEFORE the archive write: the file on disk must be
    // safe to commit/share. Parsing consumes the same redacted line, so the
    // UI can never display what the archive doesn't contain.
    let mut redactor = crate::redact::Redactor::new(req.redact);
    let mut lines = BufReader::new(stdout).lines();
    while let Some(line) = lines.next_line().await? {
        let line = redactor.line(&line);
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

    // Chain link: computed last, over the final archive + meta content.
    let archive_bytes = tokio::fs::read(&archive_path).await.unwrap_or_default();
    let prev = crate::provenance::last_link(&runs_dir);
    meta.chain = crate::provenance::compute_link(&prev, &archive_bytes, &meta).ok();

    let meta_path = runs_dir.join(format!("{run_id}.meta.json"));
    tokio::fs::write(&meta_path, serde_json::to_string_pretty(&meta)?)
        .await
        .with_context(|| format!("writing {}", meta_path.display()))?;
    let _ = std::fs::remove_file(&status_file);
    let _ = std::fs::remove_file(request_path(dirs, &run_id));

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

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression: tail_run must not declare a run vanished while the daemon
    /// is still booting (status sidecar appears late). Found live during the
    /// first real cross-model run.
    #[tokio::test(flavor = "multi_thread")]
    async fn tail_survives_slow_daemon_startup() {
        let tmp = tempfile::tempdir().unwrap();
        let dirs = RitualDirs::new(tmp.path());
        std::fs::create_dir_all(dirs.runs_dir()).unwrap();
        let run_id = "20260712T000000Z-slow";

        let runs = dirs.runs_dir();
        let rid = run_id.to_string();
        std::thread::spawn(move || {
            // Daemon "boots" slowly: sidecar + archive arrive after 800ms.
            std::thread::sleep(std::time::Duration::from_millis(800));
            std::fs::write(
                runs.join(format!("{rid}.status")),
                format!(
                    r#"{{"pid":{},"stage":"slow","branch":"main"}}"#,
                    std::process::id() // our own pid: definitely alive
                ),
            )
            .unwrap();
            std::fs::write(
                runs.join(format!("{rid}.jsonl")),
                "{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"duration_ms\":5,\"session_id\":\"s\"}\n",
            )
            .unwrap();
            std::thread::sleep(std::time::Duration::from_millis(400));
            let meta = crate::history::RunMeta {
                run_id: rid.clone(),
                stage: "slow".into(),
                ok: true,
                ..Default::default()
            };
            std::fs::write(
                runs.join(format!("{rid}.meta.json")),
                serde_json::to_string(&meta).unwrap(),
            )
            .unwrap();
            let _ = std::fs::remove_file(runs.join(format!("{rid}.status")));
        });

        let (tx, mut rx) = mpsc::channel(16);
        let drain = tokio::spawn(async move { while rx.recv().await.is_some() {} });
        let outcome = tail_run(&dirs, AgentKind::Claude, run_id, tx)
            .await
            .unwrap();
        drain.await.unwrap();
        assert!(outcome.meta.ok);
        assert_eq!(outcome.meta.stage, "slow");
    }
}
