//! TUI application state + event loop. All mutations flow through AppMsg;
//! drawing lives in dashboard.rs; terminal transitions live in term.rs.

use anyhow::{Context, Result};
use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers};
use futures::StreamExt;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

use crate::config::Config;
use crate::findings::LoadedFindings;
use crate::history::RunMeta;
use crate::keymap::{self, Action};
use crate::runner::events::AgentEvent;
use crate::runner::{self, RunOutcome, RunRequest};
use crate::stages::{self, Mode};
use crate::state::{self, PIPELINE, RitualDirs, StageId, StageStatus, State};
use crate::term::Term;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Live,
    Findings,
    History,
    Plan,
}

pub const TABS: &[(Tab, &str)] = &[
    (Tab::Live, "live"),
    (Tab::Findings, "findings"),
    (Tab::History, "history"),
    (Tab::Plan, "plan"),
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckState {
    Unknown,
    Running,
    Green,
    Red { tail: String },
}

pub enum AppMsg {
    Input(Event),
    Agent(Box<AgentEvent>),
    RunExited(Box<Result<RunOutcome>>),
    CheckDone { ok: bool, tail: String },
    AgentsStatus(Box<crate::agents_status::AgentsStatus>),
    FileChanged,
    Tick,
}

/// Deferred request to hand the terminal to a child process.
pub struct AttachedRequest {
    pub stage: Option<StageId>,
    pub argv: Vec<String>,
}

pub struct App {
    pub cfg: Config,
    pub dirs: RitualDirs,
    pub state: State,
    pub branch: String,
    pub slug: String,

    pub selected: usize,
    pub tab: Tab,
    pub stream: Vec<AgentEvent>,
    pub stream_scroll: Option<usize>, // None = follow tail
    pub findings: Vec<LoadedFindings>,
    pub selected_finding: usize,
    pub metas: Vec<RunMeta>,
    pub check: CheckState,
    pub agents: crate::agents_status::AgentsStatus,
    pub running: Option<StageId>,
    pub spinner: usize,
    pub show_help: bool,
    pub status_msg: Option<String>,
    pub confirm_quit: bool,
    pub quit: bool,
    pub palette: Option<PaletteState>,

    findings_before: Vec<String>,
    run_task: Option<JoinHandle<()>>,
    pending_attached: Option<AttachedRequest>,
}

/// Command palette state: typed filter + selection over matching entries.
#[derive(Debug, Clone, Default)]
pub struct PaletteState {
    pub input: String,
    pub selected: usize,
}

impl App {
    pub fn new(cfg: Config, dirs: RitualDirs) -> Result<Self> {
        let branch = state::current_branch(&dirs.project_root).unwrap_or_else(|| "detached".into());
        let slug = state::branch_slug(&branch);
        let mut st = State::load(&dirs)?;
        st.feature_for_branch_mut(&branch);
        let findings = crate::findings::load_all(&dirs.findings_dir()).unwrap_or_default();
        let metas = crate::history::load_all(&dirs.runs_dir()).unwrap_or_default();
        Ok(Self {
            cfg,
            dirs,
            state: st,
            branch,
            slug,
            selected: 0,
            tab: Tab::Live,
            stream: Vec::new(),
            stream_scroll: None,
            findings,
            selected_finding: 0,
            metas,
            check: CheckState::Unknown,
            agents: Default::default(),
            running: None,
            spinner: 0,
            show_help: false,
            status_msg: None,
            confirm_quit: false,
            quit: false,
            palette: None,
            findings_before: Vec::new(),
            run_task: None,
            pending_attached: None,
        })
    }

    /// Palette entries matching the current filter, in stable order.
    pub fn palette_filtered(&self) -> Vec<(String, Action)> {
        let filter = self
            .palette
            .as_ref()
            .map(|p| p.input.clone())
            .unwrap_or_default();
        keymap::palette_entries()
            .into_iter()
            .filter(|(label, _)| keymap::fuzzy_match(&filter, label))
            .collect()
    }

    pub fn selected_stage(&self) -> StageId {
        PIPELINE[self.selected.min(PIPELINE.len() - 1)]
    }

    /// Today's spend from loaded metas (status-bar budget segment).
    pub fn today_spend(&self) -> f64 {
        crate::history::today_summary(&self.metas).cost_usd
    }

    pub fn stage_status(&self, id: StageId) -> StageStatus {
        self.state
            .features
            .get(&self.slug)
            .map(|f| f.stage(id).status)
            .unwrap_or_default()
    }

    fn set_stage(&mut self, stage: StageId, status: StageStatus, run_id: Option<String>) {
        crate::run_cmd::set_stage(&mut self.state, &self.branch, stage, status, run_id);
        let _ = self.state.save(&self.dirs);
    }

    fn reload_artifacts(&mut self) {
        self.findings = crate::findings::load_all(&self.dirs.findings_dir()).unwrap_or_default();
        self.metas = crate::history::load_all(&self.dirs.runs_dir()).unwrap_or_default();
    }

    /// Handle one message. Side effects that need the terminal (attached
    /// children) are deferred via `pending_attached`.
    pub fn update(&mut self, msg: AppMsg, tx: &mpsc::Sender<AppMsg>) {
        match msg {
            AppMsg::Tick => {
                self.spinner = self.spinner.wrapping_add(1);
            }
            AppMsg::Input(ev) => self.on_input(ev, tx),
            AppMsg::Agent(ev) => {
                self.stream.push(*ev);
                if self.stream.len() > 5000 {
                    self.stream.drain(..1000);
                }
            }
            AppMsg::RunExited(outcome) => self.on_run_exited(*outcome),
            AppMsg::CheckDone { ok, tail } => {
                self.check = if ok {
                    CheckState::Green
                } else {
                    CheckState::Red { tail }
                };
            }
            AppMsg::AgentsStatus(status) => self.agents = *status,
            AppMsg::FileChanged => {
                // Auto-check only when idle: agent runs already get checked
                // by the PostToolUse hook, and parallel checks fight over
                // build locks.
                if self.running.is_none() && self.check != CheckState::Running {
                    self.run_check(tx, true);
                }
            }
        }
    }

    pub fn take_attached(&mut self) -> Option<AttachedRequest> {
        self.pending_attached.take()
    }

    fn on_input(&mut self, ev: Event, tx: &mpsc::Sender<AppMsg>) {
        let Event::Key(key) = ev else { return };
        if key.kind != KeyEventKind::Press {
            return;
        }
        if self.confirm_quit {
            match key.code {
                KeyCode::Char('y') | KeyCode::Char('q') => self.quit = true,
                _ => self.confirm_quit = false,
            }
            return;
        }
        if self.show_help {
            self.show_help = false;
            return;
        }
        if self.palette.is_some() {
            self.palette_input(key.code, tx);
            return;
        }
        // Ctrl-C always quits, even if rebound away.
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            self.quit = true;
            return;
        }
        if let Some(action) = self.cfg.keymap.resolve(key.code, key.modifiers) {
            self.dispatch(action, tx);
        }
    }

    /// Keys while the palette is open: type to filter, navigate, execute.
    fn palette_input(&mut self, code: KeyCode, tx: &mpsc::Sender<AppMsg>) {
        let matches = self.palette_filtered();
        let Some(p) = self.palette.as_mut() else {
            return;
        };
        match code {
            KeyCode::Esc => self.palette = None,
            KeyCode::Enter => {
                let action = matches.get(p.selected.min(matches.len().saturating_sub(1)));
                let action = action.map(|(_, a)| *a);
                self.palette = None;
                if let Some(a) = action {
                    self.dispatch(a, tx);
                }
            }
            KeyCode::Backspace => {
                p.input.pop();
                p.selected = 0;
            }
            KeyCode::Up => p.selected = p.selected.saturating_sub(1),
            KeyCode::Down => {
                if p.selected + 1 < matches.len() {
                    p.selected += 1;
                }
            }
            KeyCode::Char(c) => {
                p.input.push(c);
                p.selected = 0;
            }
            _ => {}
        }
    }

    fn dispatch(&mut self, action: Action, tx: &mpsc::Sender<AppMsg>) {
        match action {
            Action::Quit => {
                if self.running.is_some() {
                    self.confirm_quit = true;
                } else {
                    self.quit = true;
                }
            }
            Action::Help => self.show_help = true,
            Action::Palette => self.palette = Some(PaletteState::default()),
            Action::NextTab => self.next_tab(),
            Action::TabLive => self.tab = Tab::Live,
            Action::TabFindings => self.tab = Tab::Findings,
            Action::TabHistory => self.tab = Tab::History,
            Action::TabPlan => self.tab = Tab::Plan,
            Action::Down => self.nav(1),
            Action::Up => self.nav(-1),
            Action::ScrollTop => self.stream_scroll = Some(0),
            Action::Follow => self.stream_scroll = None,
            Action::Confirm => self.on_enter(tx),
            Action::Cancel => self.cancel_run(),
            Action::CheckFast => self.run_check(tx, true),
            Action::CheckFull => self.run_check(tx, false),
            Action::Refresh => self.refresh(tx),
            Action::OpenEditor => self.open_editor(),
            Action::RunStage(id) => {
                if let Some(idx) = PIPELINE.iter().position(|s| *s == id) {
                    self.selected = idx;
                }
                self.tab = Tab::Live;
                self.on_enter(tx);
            }
        }
    }

    fn next_tab(&mut self) {
        let idx = TABS.iter().position(|(t, _)| *t == self.tab).unwrap_or(0);
        self.tab = TABS[(idx + 1) % TABS.len()].0;
    }

    fn nav(&mut self, delta: i32) {
        match self.tab {
            Tab::Findings => {
                let len = crate::findings::aggregate(&self.findings).len();
                if len > 0 {
                    self.selected_finding =
                        (self.selected_finding as i32 + delta).rem_euclid(len as i32) as usize;
                }
            }
            Tab::Live => {
                // Manual scroll: leave follow mode.
                let cur = self.stream_scroll.unwrap_or(self.stream.len());
                let next = (cur as i32 + delta).max(0) as usize;
                self.stream_scroll = if next >= self.stream.len() {
                    None
                } else {
                    Some(next)
                };
            }
            _ => {
                self.selected =
                    (self.selected as i32 + delta).rem_euclid(PIPELINE.len() as i32) as usize;
            }
        }
    }

    fn on_enter(&mut self, tx: &mpsc::Sender<AppMsg>) {
        if self.tab == Tab::Findings {
            self.open_editor();
            return;
        }
        if self.running.is_some() {
            self.status_msg = Some("a run is already active — x to cancel".into());
            return;
        }
        let stage = self.selected_stage();
        let cmd = match stages::build(stage, &self.cfg, &self.dirs, &self.slug, None) {
            Ok(c) => c,
            Err(e) => {
                self.status_msg = Some(format!("{e:#}"));
                return;
            }
        };
        if cmd.needs_codex && self.agents.codex_cli_ok == Some(false) {
            self.status_msg = Some("codex not authenticated — run `codex login`".into());
            return;
        }
        match cmd.mode {
            Mode::Local => {
                let spec = self.dirs.spec_file(&self.slug);
                if !spec.exists() {
                    let _ = std::fs::create_dir_all(self.dirs.feature_dir(&self.slug));
                    let title = self
                        .state
                        .features
                        .get(&self.slug)
                        .map(|f| f.title.clone())
                        .unwrap_or_default();
                    let _ = std::fs::write(
                        &spec,
                        crate::scaffold::SPEC_TEMPLATE.replace("<title>", &title),
                    );
                }
                let editor = std::env::var("EDITOR").unwrap_or_else(|_| "vi".into());
                self.pending_attached = Some(AttachedRequest {
                    stage: Some(StageId::Spec),
                    argv: vec![editor, spec.display().to_string()],
                });
            }
            Mode::Interactive => {
                self.pending_attached = Some(AttachedRequest {
                    stage: Some(stage),
                    argv: cmd.argv,
                });
            }
            Mode::Headless => self.spawn_headless(stage, cmd, tx),
        }
    }

    fn spawn_headless(
        &mut self,
        stage: StageId,
        cmd: stages::StageCommand,
        tx: &mpsc::Sender<AppMsg>,
    ) {
        if let Some((spent, budget)) = crate::run_cmd::budget_exceeded(&self.cfg, &self.dirs) {
            self.status_msg = Some(format!(
                "daily budget reached (${spent:.2}/${budget:.2}) — `ritual run {} --force` to override",
                stage.label()
            ));
            return;
        }
        self.findings_before = list_dir(&self.dirs.findings_dir());
        self.stream.clear();
        self.stream_scroll = None;
        self.tab = Tab::Live;
        self.running = Some(stage);
        self.set_stage(stage, StageStatus::Running, None);

        let title = self
            .state
            .features
            .get(&self.slug)
            .map(|f| f.title.clone())
            .unwrap_or_default();
        let mut req = RunRequest {
            agent: cmd.agent,
            argv: cmd.argv,
            env: cmd.env,
            stage: stage.label().into(),
            feature: title,
            branch: self.branch.clone(),
            redact: self.cfg.redaction,
            repro: None,
        };
        let dirs = self.dirs.clone();
        let cfg = self.cfg.clone();
        let tx_events = tx.clone();
        let tx_done = tx.clone();
        self.run_task = Some(tokio::spawn(async move {
            // Provenance collection shells out (git, --version) — keep it off
            // the UI thread and off the async executor.
            let dirs_probe = dirs.clone();
            req.repro =
                tokio::task::spawn_blocking(move || crate::provenance::collect(&cfg, &dirs_probe))
                    .await
                    .ok();
            let (etx, mut erx) = mpsc::channel::<AgentEvent>(256);
            let forward = tokio::spawn(async move {
                while let Some(ev) = erx.recv().await {
                    if tx_events.send(AppMsg::Agent(Box::new(ev))).await.is_err() {
                        break;
                    }
                }
            });
            let outcome = runner::run_headless(&dirs, req, etx).await;
            let _ = forward.await;
            let _ = tx_done.send(AppMsg::RunExited(Box::new(outcome))).await;
        }));
    }

    fn on_run_exited(&mut self, outcome: Result<RunOutcome>) {
        let Some(stage) = self.running.take() else {
            return;
        };
        self.run_task = None;
        match outcome {
            Ok(out) => {
                let new_findings: Vec<String> = list_dir(&self.dirs.findings_dir())
                    .into_iter()
                    .filter(|f| !self.findings_before.contains(f))
                    .collect();
                let status = if !out.meta.ok {
                    StageStatus::Failed
                } else if new_findings.is_empty() {
                    StageStatus::NeedsAttention
                } else {
                    StageStatus::Done
                };
                crate::notify::notify(
                    self.cfg.notifications,
                    &format!(
                        "ritual: {} {}",
                        stage.label(),
                        match status {
                            StageStatus::Done => "done",
                            StageStatus::NeedsAttention => "needs attention",
                            _ => "failed",
                        }
                    ),
                    &format!(
                        "{} — {} new findings, ${:.2}",
                        self.branch,
                        new_findings.len(),
                        out.meta.total_cost_usd.unwrap_or(0.0)
                    ),
                );
                self.status_msg = Some(match status {
                    StageStatus::Done => format!(
                        "{} done — {} new findings file(s), ${:.3}",
                        stage.label(),
                        new_findings.len(),
                        out.meta.total_cost_usd.unwrap_or(0.0)
                    ),
                    StageStatus::NeedsAttention => format!(
                        "{} finished without findings — needs attention{}",
                        stage.label(),
                        out.meta
                            .session_id
                            .as_deref()
                            .map(|s| format!(" (claude --resume {s})"))
                            .unwrap_or_default()
                    ),
                    _ => format!(
                        "{} failed: {}",
                        stage.label(),
                        out.meta.error.as_deref().unwrap_or("see live tab")
                    ),
                });
                self.set_stage(stage, status, Some(out.meta.run_id.clone()));
            }
            Err(e) => {
                self.status_msg = Some(format!("{} failed: {e:#}", stage.label()));
                self.set_stage(stage, StageStatus::Failed, None);
            }
        }
        self.reload_artifacts();
    }

    /// Post-processing after an attached (interactive) child exits.
    pub fn after_attached(&mut self, stage: Option<StageId>, child_ok: bool) {
        let Some(stage) = stage else { return };
        let status = match stage {
            StageId::Spec => {
                let content =
                    std::fs::read_to_string(self.dirs.spec_file(&self.slug)).unwrap_or_default();
                let meaningful = content
                    .lines()
                    .any(|l| !l.trim().is_empty() && !l.trim_start().starts_with(['#', '<']));
                if meaningful {
                    StageStatus::Done
                } else {
                    StageStatus::Pending
                }
            }
            StageId::Plan => {
                if self.dirs.plan_file(&self.slug).exists() {
                    StageStatus::Done
                } else {
                    self.status_msg = Some(format!(
                        "plan.md not written — save it to {}",
                        self.dirs.plan_file(&self.slug).display()
                    ));
                    StageStatus::NeedsAttention
                }
            }
            _ => {
                if child_ok {
                    StageStatus::Done
                } else {
                    StageStatus::Failed
                }
            }
        };
        self.set_stage(stage, status, None);
        self.reload_artifacts();
    }

    fn cancel_run(&mut self) {
        if let Some(task) = self.run_task.take() {
            task.abort(); // kill_on_drop(true) takes the child down with it
            if let Some(stage) = self.running.take() {
                self.set_stage(stage, StageStatus::Failed, None);
                self.status_msg = Some(format!("{} cancelled", stage.label()));
            }
        }
    }

    fn run_check(&mut self, tx: &mpsc::Sender<AppMsg>, fast: bool) {
        if self.check == CheckState::Running {
            return;
        }
        if !self.dirs.project_root.join("check.sh").exists() {
            self.status_msg =
                Some("no check.sh in this project — `ritual init` creates one".into());
            return;
        }
        self.check = CheckState::Running;
        let root = self.dirs.project_root.clone();
        let tx = tx.clone();
        tokio::task::spawn_blocking(move || {
            let out = std::process::Command::new("./check.sh")
                .args(if fast { vec!["fast"] } else { vec![] })
                .current_dir(&root)
                .output();
            let (ok, tail) = match out {
                Ok(o) => {
                    let text = format!(
                        "{}{}",
                        String::from_utf8_lossy(&o.stdout),
                        String::from_utf8_lossy(&o.stderr)
                    );
                    let tail: String = text
                        .lines()
                        .rev()
                        .take(15)
                        .collect::<Vec<_>>()
                        .into_iter()
                        .rev()
                        .collect::<Vec<_>>()
                        .join("\n");
                    (o.status.success(), tail)
                }
                Err(e) => (false, e.to_string()),
            };
            let _ = tx.blocking_send(AppMsg::CheckDone { ok, tail });
        });
    }

    fn refresh(&mut self, tx: &mpsc::Sender<AppMsg>) {
        self.reload_artifacts();
        crate::agents_status::spawn_probe(&self.cfg, tx.clone());
        self.status_msg = Some("refreshed".into());
    }

    fn open_editor(&mut self) {
        let agg = crate::findings::aggregate(&self.findings);
        let Some((_, finding)) = agg.get(self.selected_finding) else {
            self.status_msg = Some("no finding selected".into());
            return;
        };
        let Some(file) = &finding.file else {
            self.status_msg = Some("finding has no file location".into());
            return;
        };
        let editor = std::env::var("EDITOR").unwrap_or_else(|_| "vi".into());
        let mut argv = vec![editor];
        if let Some(line) = finding.line {
            argv.push(format!("+{line}"));
        }
        argv.push(file.clone());
        self.pending_attached = Some(AttachedRequest { stage: None, argv });
    }
}

fn list_dir(dir: &std::path::Path) -> Vec<String> {
    std::fs::read_dir(dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default()
}

/// Input reader task: owns the crossterm EventStream. Must be stopped (and
/// awaited) before any terminal suspend — see term.rs contract.
pub struct InputTask {
    stop: oneshot::Sender<()>,
    handle: JoinHandle<()>,
}

impl InputTask {
    pub fn spawn(tx: mpsc::Sender<AppMsg>) -> Self {
        let (stop, mut stop_rx) = oneshot::channel();
        let handle = tokio::spawn(async move {
            let mut stream = crossterm::event::EventStream::new();
            loop {
                tokio::select! {
                    _ = &mut stop_rx => break,
                    ev = stream.next() => match ev {
                        Some(Ok(e)) => {
                            if tx.send(AppMsg::Input(e)).await.is_err() {
                                break;
                            }
                        }
                        _ => break,
                    }
                }
            }
        });
        Self { stop, handle }
    }

    pub async fn stop(self) {
        let _ = self.stop.send(());
        let _ = self.handle.await;
    }
}

/// The main TUI entry point.
pub async fn run(cfg: Config, dirs: RitualDirs) -> Result<()> {
    anyhow::ensure!(dirs.exists(), "no .ritual/ here — run `ritual init` first");
    let mut term = Term::enter()?;
    let (tx, mut rx) = mpsc::channel::<AppMsg>(512);

    let mut app = App::new(cfg, dirs).context("loading project state")?;
    crate::agents_status::spawn_probe(&app.cfg, tx.clone());
    let watcher = crate::watcher::spawn(app.dirs.project_root.clone(), tx.clone()).ok();

    // Spinner/refresh tick.
    let tick_tx = tx.clone();
    let tick = tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_millis(250));
        loop {
            interval.tick().await;
            if tick_tx.send(AppMsg::Tick).await.is_err() {
                break;
            }
        }
    });

    let mut input = Some(InputTask::spawn(tx.clone()));

    while !app.quit {
        term.terminal
            .draw(|f| crate::ui::dashboard::draw(f, &app))?;

        let Some(msg) = rx.recv().await else { break };
        app.update(msg, &tx);
        // Batch whatever else is queued before redrawing.
        while let Ok(msg) = rx.try_recv() {
            app.update(msg, &tx);
        }

        // The watcher stands down while any agent owns the project.
        if let Some(w) = &watcher {
            w.paused
                .store(app.running.is_some(), std::sync::atomic::Ordering::SeqCst);
        }

        if let Some(req) = app.take_attached() {
            if let Some(task) = input.take() {
                task.stop().await; // crossterm reader is global: MUST join first
            }
            if let Some(w) = &watcher {
                w.paused.store(true, std::sync::atomic::Ordering::SeqCst);
            }
            let cwd = app.dirs.project_root.clone();
            // std::process blocks; tell tokio so the worker thread is compensated.
            let ok = tokio::task::block_in_place(|| term.run_attached(&req.argv, &cwd))?;
            if let Some(w) = &watcher {
                w.paused.store(false, std::sync::atomic::Ordering::SeqCst);
            }
            app.after_attached(req.stage, ok);
            input = Some(InputTask::spawn(tx.clone()));
        }
    }

    tick.abort();
    if let Some(task) = input.take() {
        task.stop().await;
    }
    drop(term); // restores the terminal
    Ok(())
}
