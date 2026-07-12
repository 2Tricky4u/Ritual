//! TUI application state + event loop. All mutations flow through AppMsg;
//! drawing lives in dashboard.rs; terminal transitions live in term.rs.

use anyhow::{Context, Result};
use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
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
    Guide,
}

pub const TABS: &[(Tab, &str)] = &[
    (Tab::Live, "live"),
    (Tab::Findings, "findings"),
    (Tab::History, "history"),
    (Tab::Plan, "plan"),
    (Tab::Guide, "guide"),
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
    /// A streamed event from a spec/plan chat edit (kept off the main stream).
    ChatAgent(Box<AgentEvent>),
    /// A chat edit finished.
    ChatExited(Box<Result<RunOutcome>>),
    CheckDone {
        ok: bool,
        tail: String,
    },
    AgentsStatus(Box<crate::agents_status::AgentsStatus>),
    FileChanged,
    Tick,
}

/// Deferred request to hand the terminal to a child process.
pub struct AttachedRequest {
    pub stage: Option<StageId>,
    pub argv: Vec<String>,
    pub cwd: std::path::PathBuf,
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
    /// Show findings already marked fixed/dismissed (toggled with `v`).
    pub show_resolved: bool,
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
    pub plan_scroll: usize,
    pub guide_scroll: usize,
    pub chat: Option<ChatState>,

    findings_before: Vec<String>,
    run_task: Option<JoinHandle<()>>,
    current_run_id: Option<String>,
    pending_attached: Option<AttachedRequest>,
    chat_task: Option<JoinHandle<()>>,
    current_chat_run_id: Option<String>,
    doc_before: String,
}

/// Command palette state: typed filter + selection over matching entries.
#[derive(Debug, Clone, Default)]
pub struct PaletteState {
    pub input: String,
    pub selected: usize,
}

/// One entry in the chat transcript.
#[derive(Debug, Clone)]
pub enum ChatTurn {
    User(String),
    Assistant(Vec<AgentEvent>),
    System(String),
}

/// A place a chat edit can be aimed: a whole document or one of its sections.
#[derive(Debug, Clone)]
pub struct ChatTarget {
    pub doc: stages::DocKind,
    /// `None` = the whole document; `Some(heading)` = one `##` section.
    pub section: Option<String>,
    /// Line range in the source file that the preview focuses on.
    pub range: std::ops::Range<usize>,
}

impl ChatTarget {
    fn scope(&self) -> stages::Scope {
        match &self.section {
            Some(name) => stages::Scope::Section(name.clone()),
            None => stages::Scope::Whole,
        }
    }
    /// Short label for the chat header, e.g. `spec · § Behavior` / `plan · whole`.
    pub fn label(&self) -> String {
        match &self.section {
            Some(name) => format!("{} · § {}", self.doc.label(), first_words(name, 20)),
            None => format!("{} · whole", self.doc.label()),
        }
    }
}

/// Interactive spec/plan chat: a transcript, a cursored input line, and the
/// set of documents/sections the edit can target. Mirrors `PaletteState` as
/// the app's second modal text-entry surface.
#[derive(Debug, Clone)]
pub struct ChatState {
    pub transcript: Vec<ChatTurn>,
    pub input: Vec<char>, // Vec<char> (not String) so the caret can sit mid-string
    pub cursor: usize,
    pub targets: Vec<ChatTarget>,
    pub target_idx: usize,
    pub scroll: usize,
    pub in_flight: bool,
    /// Messages typed while an edit was in flight, sent one at a time as
    /// each edit finishes (capped — this is a chat, not a job queue).
    pub pending: std::collections::VecDeque<String>,
}

/// Beyond this the user should wait — queued edits compound unpredictably.
const CHAT_QUEUE_CAP: usize = 3;

impl ChatState {
    pub fn target(&self) -> Option<&ChatTarget> {
        self.targets.get(self.target_idx)
    }
}

/// First `max` chars of a heading, ellipsized — keeps the chat header tidy.
fn first_words(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(max).collect::<String>())
    }
}

impl App {
    pub fn new(cfg: Config, dirs: RitualDirs) -> Result<Self> {
        let branch = state::current_branch(&dirs.work_root).unwrap_or_else(|| "detached".into());
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
            show_resolved: false,
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
            plan_scroll: 0,
            guide_scroll: 0,
            chat: None,
            findings_before: Vec::new(),
            run_task: None,
            current_run_id: None,
            pending_attached: None,
            chat_task: None,
            current_chat_run_id: None,
            doc_before: String::new(),
        })
    }

    /// True while a spec/plan chat edit is running.
    pub fn chat_running(&self) -> bool {
        self.chat.as_ref().is_some_and(|c| c.in_flight)
    }

    /// Palette entries matching the current filter, in stable order.
    pub fn palette_filtered(&self) -> Vec<(String, Action)> {
        let filter = self
            .palette
            .as_ref()
            .map(|p| p.input.clone())
            .unwrap_or_default();
        let mut entries = keymap::palette_entries();
        for (i, (name, _)) in self.cfg.commands.iter().enumerate() {
            entries.push((format!("cmd: {name}"), Action::Custom(i)));
        }
        entries
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

    /// True when any stage of the feature needs a human.
    pub fn feature_needs_you(&self, slug: &str) -> bool {
        self.state
            .features
            .get(slug)
            .map(|f| {
                f.stages
                    .values()
                    .any(|s| matches!(s.status, StageStatus::NeedsAttention | StageStatus::Failed))
            })
            .unwrap_or(false)
    }

    /// All features, needs-you first, then most recently updated.
    pub fn feature_order(&self) -> Vec<String> {
        let mut slugs: Vec<&String> = self.state.features.keys().collect();
        slugs.sort_by_key(|slug| {
            let needs = self.feature_needs_you(slug);
            let updated = self
                .state
                .features
                .get(*slug)
                .map(|f| f.updated_at)
                .unwrap_or_default();
            (std::cmp::Reverse(needs), std::cmp::Reverse(updated))
        });
        slugs.into_iter().cloned().collect()
    }

    /// Cycle the viewed feature; run cwd resolution happens at run time.
    fn select_feature(&mut self, delta: i32) {
        let order = self.feature_order();
        if order.len() < 2 {
            return;
        }
        let idx = order.iter().position(|s| *s == self.slug).unwrap_or(0);
        let next = (idx as i32 + delta).rem_euclid(order.len() as i32) as usize;
        self.slug = order[next].clone();
        self.plan_scroll = 0;
        if let Some(f) = self.state.features.get(&self.slug) {
            self.branch = f.branch.clone();
        }
        self.status_msg = Some(format!("viewing feature: {}", self.slug));
    }

    /// Where a run for the currently selected feature must execute: the
    /// current checkout if branches match, else that branch's worktree.
    fn run_cwd(&self) -> Option<std::path::PathBuf> {
        let checked_out = state::current_branch(&self.dirs.work_root);
        if checked_out.as_deref() == Some(self.branch.as_str()) || self.branch == "detached" {
            return Some(self.dirs.work_root.clone());
        }
        state::worktrees(&self.dirs.work_root)
            .into_iter()
            .find(|(b, _)| *b == self.branch)
            .map(|(_, p)| p)
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
            AppMsg::ChatAgent(ev) => {
                if let Some(chat) = self.chat.as_mut()
                    && let Some(ChatTurn::Assistant(evs)) = chat.transcript.last_mut()
                {
                    evs.push(*ev);
                    if evs.len() > 2000 {
                        evs.drain(..500);
                    }
                    chat.scroll = 0; // follow the tail while streaming
                }
            }
            AppMsg::ChatExited(outcome) => self.on_chat_exited(*outcome, tx),
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
                // build locks. A chat edit is also an agent run.
                if self.running.is_none()
                    && !self.chat_running()
                    && self.check != CheckState::Running
                {
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
        if self.chat.is_some() {
            self.chat_input(key, tx);
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
            Action::TabGuide => self.tab = Tab::Guide,
            Action::Down => self.nav(1),
            Action::Up => self.nav(-1),
            Action::ScrollTop => match self.tab {
                Tab::Plan => self.plan_scroll = 0,
                Tab::Guide => self.guide_scroll = 0,
                _ => self.stream_scroll = Some(0),
            },
            Action::Follow => self.stream_scroll = None,
            Action::Confirm => self.on_enter(tx),
            Action::Cancel => self.cancel_run(),
            Action::CheckFast => self.run_check(tx, true),
            Action::CheckFull => self.run_check(tx, false),
            Action::Refresh => self.refresh(tx),
            Action::OpenEditor => self.open_editor(),
            Action::FeatureNext => self.select_feature(1),
            Action::FeaturePrev => self.select_feature(-1),
            Action::Takeover => self.takeover(),
            Action::NvimOpen => self.nvim_open(),
            Action::NvimQuickfix => self.nvim_quickfix(),
            Action::SpecChat => self.open_chat(),
            Action::FindingFix => self.finding_set_action("fixed"),
            Action::FindingDismiss => self.finding_set_action("dismissed"),
            Action::ToggleResolved => {
                if self.tab == Tab::Findings {
                    self.show_resolved = !self.show_resolved;
                    self.clamp_selected_finding();
                    self.status_msg = Some(if self.show_resolved {
                        "showing resolved findings".into()
                    } else {
                        "hiding resolved findings".into()
                    });
                } else {
                    self.status_msg = Some("v toggles resolved on the findings tab (2)".into());
                }
            }
            Action::Custom(i) => self.run_custom(i, tx),
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
                let len = crate::findings::aggregate(&self.findings, self.show_resolved).len();
                if len > 0 {
                    self.selected_finding =
                        (self.selected_finding as i32 + delta).rem_euclid(len as i32) as usize;
                }
            }
            Tab::Live if self.stream.is_empty() => {
                // Greeter is showing (nothing to scroll): j/k moves the
                // sidebar pipeline highlight, so the greeter's "enter = run
                // selected stage" hint is actually navigable from the dash.
                self.selected =
                    (self.selected as i32 + delta).rem_euclid(PIPELINE.len() as i32) as usize;
            }
            Tab::Live => {
                // Live stream present: manual scroll leaves follow mode.
                let cur = self.stream_scroll.unwrap_or(self.stream.len());
                let next = (cur as i32 + delta).max(0) as usize;
                self.stream_scroll = if next >= self.stream.len() {
                    None
                } else {
                    Some(next)
                };
            }
            Tab::Plan => {
                self.plan_scroll = (self.plan_scroll as i32 + delta).max(0) as usize;
            }
            Tab::Guide => {
                self.guide_scroll = (self.guide_scroll as i32 + delta).max(0) as usize;
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
        if self.running.is_some() || self.chat_running() {
            self.status_msg = Some("a run is already active — x to cancel".into());
            return;
        }
        let Some(run_cwd) = self.run_cwd() else {
            self.status_msg = Some(format!(
                "branch '{}' has no checkout — `ritual new --worktree {}` or switch to it",
                self.branch, self.branch
            ));
            return;
        };
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
                    cwd: run_cwd,
                });
            }
            Mode::Interactive => {
                self.pending_attached = Some(AttachedRequest {
                    stage: Some(stage),
                    argv: cmd.argv,
                    cwd: run_cwd,
                });
            }
            Mode::Headless => self.spawn_headless(stage, cmd, run_cwd, tx),
        }
    }

    /// `a`: reattach interactively to the selected stage's last recorded
    /// session (`claude --resume <session_id>`).
    fn takeover(&mut self) {
        let stage = self.selected_stage();
        let Some(run_id) = self
            .state
            .features
            .get(&self.slug)
            .map(|f| f.stage(stage))
            .and_then(|s| s.runs.last().cloned())
        else {
            self.status_msg = Some(format!("no recorded runs for {}", stage.label()));
            return;
        };
        let Some(sid) = self
            .metas
            .iter()
            .find(|m| m.run_id == run_id)
            .and_then(|m| m.session_id.clone())
        else {
            self.status_msg = Some(format!("run {run_id} recorded no session id"));
            return;
        };
        let Some(cwd) = self.run_cwd() else {
            self.status_msg = Some(format!("branch '{}' has no checkout", self.branch));
            return;
        };
        let mut argv = self.cfg.claude_cmd.clone();
        argv.push("--resume".into());
        argv.push(sid);
        self.pending_attached = Some(AttachedRequest {
            stage: None,
            argv,
            cwd,
        });
    }

    fn spawn_headless(
        &mut self,
        stage: StageId,
        cmd: stages::StageCommand,
        run_cwd: std::path::PathBuf,
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
            cwd: run_cwd,
        };
        let dirs = self.dirs.clone();
        let cfg = self.cfg.clone();
        let run_id = runner::new_run_id(stage.label());
        self.current_run_id = Some(run_id.clone());
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
            // Detach, then follow the archive: the run survives the TUI.
            let agent = req.agent;
            if let Err(e) = runner::spawn_detached(&dirs, &req, &run_id) {
                let _ = tx_done.send(AppMsg::RunExited(Box::new(Err(e)))).await;
                return;
            }
            let (etx, mut erx) = mpsc::channel::<AgentEvent>(256);
            let forward = tokio::spawn(async move {
                while let Some(ev) = erx.recv().await {
                    if tx_events.send(AppMsg::Agent(Box::new(ev))).await.is_err() {
                        break;
                    }
                }
            });
            let outcome = runner::tail_run(&dirs, agent, &run_id, etx).await;
            let _ = forward.await;
            let _ = tx_done.send(AppMsg::RunExited(Box::new(outcome))).await;
        }));
    }

    // -- spec/plan chat -----------------------------------------------------

    /// Open the interactive chat over this feature's spec (and plan, if it
    /// exists). Ensures spec.md exists so there is always something to edit.
    fn open_chat(&mut self) {
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
        let targets = self.build_chat_targets();
        self.chat = Some(ChatState {
            transcript: Vec::new(),
            input: Vec::new(),
            cursor: 0,
            targets,
            target_idx: 0,
            scroll: 0,
            in_flight: false,
            pending: Default::default(),
        });
    }

    /// The editable targets: spec (whole + each `##` section), then plan the
    /// same way if plan.md exists.
    fn build_chat_targets(&self) -> Vec<ChatTarget> {
        let mut targets = Vec::new();
        for (doc, path) in [
            (stages::DocKind::Spec, self.dirs.spec_file(&self.slug)),
            (stages::DocKind::Plan, self.dirs.plan_file(&self.slug)),
        ] {
            if doc == stages::DocKind::Plan && !path.exists() {
                continue; // only refine a plan that already exists
            }
            let text = std::fs::read_to_string(&path).unwrap_or_default();
            let n = text.lines().count().max(1);
            targets.push(ChatTarget {
                doc,
                section: None,
                range: 0..n,
            });
            for (name, range) in crate::spec::sections(&text) {
                targets.push(ChatTarget {
                    doc,
                    section: Some(name),
                    range,
                });
            }
        }
        targets
    }

    /// Keys while the chat panel is open. Unlike the palette this maintains a
    /// real cursor (mid-string insert/delete) over a `Vec<char>` input.
    fn chat_input(&mut self, key: KeyEvent, tx: &mpsc::Sender<AppMsg>) {
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            self.quit = true;
            return;
        }
        if key.code == KeyCode::Char('x') && key.modifiers.contains(KeyModifiers::CONTROL) {
            self.chat_cancel();
            return;
        }
        if key.code == KeyCode::Char('z') && key.modifiers.contains(KeyModifiers::CONTROL) {
            self.chat_undo();
            return;
        }
        // Alt+Enter inserts a newline; plain Enter submits (handled first
        // because submitting needs `&mut self` to spawn). While an edit is
        // in flight, Enter queues instead (drained as edits finish).
        if key.code == KeyCode::Enter {
            if key.modifiers.contains(KeyModifiers::ALT) {
                if let Some(chat) = self.chat.as_mut() {
                    chat.input.insert(chat.cursor, '\n');
                    chat.cursor += 1;
                }
            } else if self.chat.as_ref().is_some_and(|c| c.in_flight) {
                if let Some(chat) = self.chat.as_mut() {
                    let msg: String = chat.input.iter().collect::<String>().trim().to_string();
                    if msg.is_empty() {
                        return;
                    }
                    if chat.pending.len() >= CHAT_QUEUE_CAP {
                        chat.transcript.push(ChatTurn::System(
                            "queue full — wait for the current edit".into(),
                        ));
                        return;
                    }
                    chat.input.clear();
                    chat.cursor = 0;
                    chat.pending.push_back(msg);
                    chat.transcript.push(ChatTurn::System(format!(
                        "queued ({} waiting)",
                        chat.pending.len()
                    )));
                    chat.scroll = 0;
                }
            } else if let Some(msg) = self.chat_take_submit() {
                self.spawn_doc_chat(msg, tx);
            }
            return;
        }
        let Some(chat) = self.chat.as_mut() else {
            return;
        };
        match key.code {
            KeyCode::Esc => self.chat = None,
            KeyCode::Backspace => {
                if chat.cursor > 0 {
                    chat.input.remove(chat.cursor - 1);
                    chat.cursor -= 1;
                }
            }
            KeyCode::Delete => {
                if chat.cursor < chat.input.len() {
                    chat.input.remove(chat.cursor);
                }
            }
            KeyCode::Left => chat.cursor = chat.cursor.saturating_sub(1),
            KeyCode::Right => chat.cursor = (chat.cursor + 1).min(chat.input.len()),
            KeyCode::Home => chat.cursor = 0,
            KeyCode::End => chat.cursor = chat.input.len(),
            KeyCode::Tab => {
                if !chat.targets.is_empty() {
                    chat.target_idx = (chat.target_idx + 1) % chat.targets.len();
                }
            }
            KeyCode::BackTab => {
                if !chat.targets.is_empty() {
                    chat.target_idx =
                        (chat.target_idx + chat.targets.len() - 1) % chat.targets.len();
                }
            }
            // scroll = lines up from the bottom (0 = follow tail).
            KeyCode::Up => chat.scroll = chat.scroll.saturating_add(1),
            KeyCode::Down => chat.scroll = chat.scroll.saturating_sub(1),
            // Only PLAIN characters type (shift is part of the char itself);
            // ctrl/alt chords must never insert letters.
            KeyCode::Char(c) if key.modifiers.difference(KeyModifiers::SHIFT).is_empty() => {
                chat.input.insert(chat.cursor, c);
                chat.cursor += 1;
            }
            _ => {}
        }
    }

    /// Ctrl+X: kill an in-flight chat edit. The aborted tail task means
    /// on_chat_exited never fires for this run — reset state here.
    fn chat_cancel(&mut self) {
        let in_flight = self.chat.as_ref().is_some_and(|c| c.in_flight);
        if !in_flight {
            if let Some(chat) = self.chat.as_mut() {
                chat.transcript
                    .push(ChatTurn::System("nothing in flight to cancel".into()));
            }
            return;
        }
        if let Some(rid) = self.current_chat_run_id.take() {
            runner::kill_run(&self.dirs, &rid);
        }
        if let Some(task) = self.chat_task.take() {
            task.abort();
        }
        if let Some(chat) = self.chat.as_mut() {
            chat.in_flight = false;
            let dropped = chat.pending.len();
            chat.pending.clear();
            chat.transcript.push(ChatTurn::System(format!(
                "edit cancelled{} — Ctrl+Z restores the pre-edit document",
                if dropped > 0 {
                    format!(" ({dropped} queued message(s) dropped)")
                } else {
                    String::new()
                }
            )));
            chat.scroll = 0;
        }
    }

    /// Ctrl+Z: swap the current target's document with its pre-edit snapshot
    /// (press again to redo). The snapshot file is written on every chat
    /// edit, so undo survives TUI restarts and covers CLI chats too.
    fn chat_undo(&mut self) {
        if self.chat.as_ref().is_some_and(|c| c.in_flight) {
            if let Some(chat) = self.chat.as_mut() {
                chat.transcript.push(ChatTurn::System(
                    "cannot undo while an edit is in flight — Ctrl+X to cancel first".into(),
                ));
            }
            return;
        }
        let Some(target) = self.chat.as_ref().and_then(|c| c.target()).cloned() else {
            return;
        };
        let doc_path = match target.doc {
            stages::DocKind::Spec => self.dirs.spec_file(&self.slug),
            stages::DocKind::Plan => self.dirs.plan_file(&self.slug),
        };
        let undo = self.dirs.undo_file(&self.slug, target.doc.label());
        let note = if !undo.exists() {
            format!("nothing to undo for {}", target.doc.label())
        } else {
            let doc_now = std::fs::read_to_string(&doc_path).unwrap_or_default();
            let snapshot = std::fs::read_to_string(&undo).unwrap_or_default();
            let swap =
                std::fs::write(&doc_path, &snapshot).and_then(|_| std::fs::write(&undo, &doc_now));
            match swap {
                Ok(()) => "undid last edit — Ctrl+Z again to redo".into(),
                Err(e) => format!("undo failed: {e}"),
            }
        };
        // Refresh targets against the (possibly) restored content.
        let targets = self.build_chat_targets();
        if let Some(chat) = self.chat.as_mut() {
            chat.transcript.push(ChatTurn::System(note));
            chat.target_idx = chat.target_idx.min(targets.len().saturating_sub(1));
            chat.targets = targets;
            chat.scroll = 0;
        }
    }

    /// Consume the input as a submitted message: records the user turn, clears
    /// the input, and returns the text to send — or None if empty or a run is
    /// already in flight. Split out from spawning so it is unit-testable.
    fn chat_take_submit(&mut self) -> Option<String> {
        let chat = self.chat.as_mut()?;
        if chat.in_flight {
            return None;
        }
        let msg: String = chat.input.iter().collect::<String>().trim().to_string();
        if msg.is_empty() {
            return None;
        }
        chat.input.clear();
        chat.cursor = 0;
        chat.transcript.push(ChatTurn::User(msg.clone()));
        chat.scroll = 0;
        Some(msg)
    }

    /// The last few turns before the current one, as plain context for the
    /// prompt (so "make it 3 not 5" resolves against the prior exchange).
    fn recent_context(&self) -> String {
        let Some(chat) = self.chat.as_ref() else {
            return String::new();
        };
        let mut lines = Vec::new();
        for turn in chat.transcript.iter().rev().skip(1).take(6) {
            match turn {
                ChatTurn::User(t) => lines.push(format!("you: {t}")),
                ChatTurn::Assistant(evs) => {
                    let text: String = evs
                        .iter()
                        .filter_map(|e| match e {
                            AgentEvent::Text { text } => Some(text.as_str()),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join(" ");
                    if !text.trim().is_empty() {
                        lines.push(format!("assistant: {}", first_words(text.trim(), 200)));
                    }
                }
                ChatTurn::System(_) => {}
            }
        }
        lines.reverse();
        lines.join("\n")
    }

    /// Spawn one chat edit: a detached `/spec` run whose events stream into the
    /// transcript. Never touches `self.running`/`run_task` — the pipeline is
    /// independent of the chat.
    fn spawn_doc_chat(&mut self, message: String, tx: &mpsc::Sender<AppMsg>) {
        if let Some((spent, budget)) = crate::run_cmd::budget_exceeded(&self.cfg, &self.dirs) {
            if let Some(chat) = self.chat.as_mut() {
                chat.transcript.push(ChatTurn::System(format!(
                    "daily budget reached (${spent:.2}/${budget:.2}) — `ritual chat … --force` to override"
                )));
            }
            return;
        }
        let Some(run_cwd) = self.run_cwd() else {
            if let Some(chat) = self.chat.as_mut() {
                chat.transcript.push(ChatTurn::System(format!(
                    "branch '{}' has no checkout",
                    self.branch
                )));
            }
            return;
        };
        let Some(target) = self.chat.as_ref().and_then(|c| c.target()).cloned() else {
            return;
        };
        let doc_path = match target.doc {
            stages::DocKind::Spec => self.dirs.spec_file(&self.slug),
            stages::DocKind::Plan => self.dirs.plan_file(&self.slug),
        };
        let context = self.recent_context();
        let cmd = stages::doc_chat_command(
            &self.cfg,
            &doc_path,
            target.doc,
            &target.scope(),
            &message,
            &context,
        );
        self.doc_before = std::fs::read_to_string(&doc_path).unwrap_or_default();
        // Persist the pre-edit snapshot: the Ctrl+Z undo source (survives
        // restarts; single-level — each edit replaces it).
        let _ = std::fs::create_dir_all(self.dirs.feature_dir(&self.slug));
        let _ = std::fs::write(
            self.dirs.undo_file(&self.slug, target.doc.label()),
            &self.doc_before,
        );
        if let Some(chat) = self.chat.as_mut() {
            chat.transcript.push(ChatTurn::Assistant(Vec::new()));
            chat.in_flight = true;
            chat.scroll = 0;
        }

        let title = self
            .state
            .features
            .get(&self.slug)
            .map(|f| f.title.clone())
            .unwrap_or_default();
        let stage_label = format!("{}-chat", target.doc.label());
        let req = RunRequest {
            agent: cmd.agent,
            argv: cmd.argv,
            env: cmd.env,
            stage: stage_label.clone(),
            feature: title,
            branch: self.branch.clone(),
            redact: self.cfg.redaction,
            repro: None, // chat edits are frequent + small — skip provenance
            cwd: run_cwd,
        };
        let dirs = self.dirs.clone();
        let run_id = runner::new_run_id(&stage_label);
        self.current_chat_run_id = Some(run_id.clone());
        let tx_events = tx.clone();
        let tx_done = tx.clone();
        self.chat_task = Some(tokio::spawn(async move {
            let agent = req.agent;
            if let Err(e) = runner::spawn_detached(&dirs, &req, &run_id) {
                let _ = tx_done.send(AppMsg::ChatExited(Box::new(Err(e)))).await;
                return;
            }
            let (etx, mut erx) = mpsc::channel::<AgentEvent>(256);
            let forward = tokio::spawn(async move {
                while let Some(ev) = erx.recv().await {
                    if tx_events
                        .send(AppMsg::ChatAgent(Box::new(ev)))
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
            });
            let outcome = runner::tail_run(&dirs, agent, &run_id, etx).await;
            let _ = forward.await;
            let _ = tx_done.send(AppMsg::ChatExited(Box::new(outcome))).await;
        }));
    }

    /// A chat edit finished: mark the target stage done iff the document
    /// meaningfully changed, refresh section targets, and note the cost.
    fn on_chat_exited(&mut self, outcome: Result<RunOutcome>, tx: &mpsc::Sender<AppMsg>) {
        self.chat_task = None;
        let run_id = self.current_chat_run_id.take();
        if let Some(chat) = self.chat.as_mut() {
            chat.in_flight = false;
        }
        let outcome = match outcome {
            Ok(o) => o,
            Err(e) => {
                if let Some(chat) = self.chat.as_mut() {
                    chat.transcript
                        .push(ChatTurn::System(format!("chat failed: {e:#}")));
                }
                return;
            }
        };
        let target_doc = self.chat.as_ref().and_then(|c| c.target()).map(|t| t.doc);
        let (doc_path, stage_id) = match target_doc {
            Some(stages::DocKind::Plan) => (self.dirs.plan_file(&self.slug), StageId::Plan),
            _ => (self.dirs.spec_file(&self.slug), StageId::Spec),
        };
        let content = std::fs::read_to_string(&doc_path).unwrap_or_default();
        let changed = content != self.doc_before && crate::spec::has_meaningful_content(&content);
        let cost = outcome.meta.total_cost_usd.unwrap_or(0.0);
        let note = if outcome.meta.ok && changed {
            self.set_stage(stage_id, StageStatus::Done, run_id);
            format!("✓ {} updated · ${cost:.3}", stage_id.label())
        } else if outcome.meta.ok {
            format!("no change · ${cost:.3}")
        } else {
            "chat edit failed — see the transcript above".to_string()
        };
        // Refresh targets against the new content (a section may have appeared
        // or vanished); clamp the selection.
        let targets = self.build_chat_targets();
        if let Some(chat) = self.chat.as_mut() {
            chat.transcript.push(ChatTurn::System(note));
            chat.target_idx = chat.target_idx.min(targets.len().saturating_sub(1));
            chat.targets = targets;
            chat.scroll = 0;
        }
        self.reload_artifacts();
        // Send the next queued message, if any (one at a time). Note: each
        // send replaces the undo snapshot — undo covers the LAST edit.
        if let Some(msg) = self.chat.as_mut().and_then(|c| c.pending.pop_front()) {
            if let Some(chat) = self.chat.as_mut() {
                chat.transcript.push(ChatTurn::User(msg.clone()));
            }
            self.spawn_doc_chat(msg, tx);
        }
    }

    /// Run a user-defined [commands] template ({{branch}}, {{run_id}},
    /// {{finding.file}}, {{finding.line}}); output lands in the live stream.
    fn run_custom(&mut self, idx: usize, tx: &mpsc::Sender<AppMsg>) {
        let Some((name, template)) = self.cfg.commands.get(idx).cloned() else {
            return;
        };
        let agg = crate::findings::aggregate(&self.findings, self.show_resolved);
        let finding = agg.get(self.selected_finding).map(|af| af.finding.clone());
        let rendered = template
            .replace("{{branch}}", &self.branch)
            .replace("{{run_id}}", self.current_run_id.as_deref().unwrap_or(""))
            .replace(
                "{{finding.file}}",
                finding
                    .as_ref()
                    .and_then(|f| f.file.as_deref())
                    .unwrap_or(""),
            )
            .replace(
                "{{finding.line}}",
                &finding
                    .as_ref()
                    .and_then(|f| f.line)
                    .map(|l| l.to_string())
                    .unwrap_or_default(),
            );
        self.status_msg = Some(format!("cmd {name}: running…"));
        self.tab = Tab::Live;
        let cwd = self
            .run_cwd()
            .unwrap_or_else(|| self.dirs.work_root.clone());
        let tx = tx.clone();
        tokio::task::spawn_blocking(move || {
            let out = std::process::Command::new("sh")
                .arg("-c")
                .arg(&rendered)
                .current_dir(&cwd)
                .output();
            let text = match out {
                Ok(o) => format!(
                    "$ {rendered}\n{}{}",
                    String::from_utf8_lossy(&o.stdout),
                    String::from_utf8_lossy(&o.stderr)
                ),
                Err(e) => format!("$ {rendered}\nfailed: {e}"),
            };
            for line in text.lines().take(80) {
                let _ = tx.blocking_send(AppMsg::Agent(Box::new(AgentEvent::Text {
                    text: line.to_string(),
                })));
            }
        });
    }

    /// Stages stuck in Running whose run actually finished (launcher died
    /// mid-tail) get finalized from the on-disk meta; runs that vanished
    /// entirely become needs-attention.
    pub fn reconcile_stale_runs(&mut self) {
        let mut fixes: Vec<(String, StageId, StageStatus)> = Vec::new();
        for (_, feature) in self.state.features.iter() {
            for (stage_id, sstate) in feature.stages.iter() {
                if sstate.status != StageStatus::Running {
                    continue;
                }
                let Some(run_id) = sstate.runs.last() else {
                    // Interactive stage interrupted before any run recorded.
                    fixes.push((
                        feature.branch.clone(),
                        *stage_id,
                        StageStatus::NeedsAttention,
                    ));
                    continue;
                };
                match runner::run_state(&self.dirs, run_id) {
                    runner::RunState::Running(_) => {} // resurrection reattaches
                    runner::RunState::Finished(meta) => {
                        let status = if meta.ok {
                            StageStatus::NeedsAttention // finished unwatched: human confirms
                        } else {
                            StageStatus::Failed
                        };
                        fixes.push((feature.branch.clone(), *stage_id, status));
                    }
                    runner::RunState::Vanished => {
                        fixes.push((feature.branch.clone(), *stage_id, StageStatus::Failed));
                    }
                }
            }
        }
        for (branch, stage, status) in fixes {
            crate::run_cmd::set_stage(&mut self.state, &branch, stage, status, None);
        }
        let _ = self.state.save(&self.dirs);
    }

    /// Reattach to a still-running detached run (crash/reboot resurrection).
    pub fn resume_run(
        &mut self,
        run_id: String,
        status: runner::RunStatus,
        tx: &mpsc::Sender<AppMsg>,
    ) {
        let Some(stage) = StageId::parse(&status.stage) else {
            return;
        };
        self.branch = status.branch.clone();
        self.slug = state::branch_slug(&status.branch);
        self.running = Some(stage);
        self.current_run_id = Some(run_id.clone());
        self.tab = Tab::Live;
        self.status_msg = Some(format!(
            "reattached to running {} ({run_id})",
            stage.label()
        ));
        let dirs = self.dirs.clone();
        // The agent is in the persisted request (RunStatus doesn't carry it).
        let agent = runner::load_request(&self.dirs, &run_id)
            .map(|r| r.agent)
            .unwrap_or(runner::AgentKind::Claude);
        let tx_events = tx.clone();
        let tx_done = tx.clone();
        self.run_task = Some(tokio::spawn(async move {
            let (etx, mut erx) = mpsc::channel::<AgentEvent>(256);
            let forward = tokio::spawn(async move {
                while let Some(ev) = erx.recv().await {
                    if tx_events.send(AppMsg::Agent(Box::new(ev))).await.is_err() {
                        break;
                    }
                }
            });
            let outcome = runner::tail_run(&dirs, agent, &run_id, etx).await;
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
                if crate::spec::has_meaningful_content(&content) {
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
        let Some(run_id) = self.current_run_id.take() else {
            self.status_msg = Some("no active run".into());
            return;
        };
        // The run is a detached process group — kill it there, then stop
        // the local tail.
        let killed = runner::kill_run(&self.dirs, &run_id);
        if let Some(task) = self.run_task.take() {
            task.abort();
        }
        if let Some(stage) = self.running.take() {
            self.set_stage(stage, StageStatus::Failed, None);
            self.status_msg = Some(format!(
                "{} cancelled{}",
                stage.label(),
                if killed { "" } else { " (daemon already gone)" }
            ));
        }
    }

    fn run_check(&mut self, tx: &mpsc::Sender<AppMsg>, fast: bool) {
        if self.check == CheckState::Running {
            return;
        }
        if !self.dirs.work_root.join("check.sh").exists() {
            self.status_msg =
                Some("no check.sh in this project — `ritual init` creates one".into());
            return;
        }
        self.check = CheckState::Running;
        let root = self.dirs.work_root.clone();
        let timeout = self.cfg.check_timeout_secs;
        let tx = tx.clone();
        tokio::task::spawn_blocking(move || {
            // Output goes to a temp file: no pipe-fill deadlock, and the
            // deadline (hung build / dead HIL board) can kill the child.
            let (ok, tail) = match tempfile::NamedTempFile::new() {
                Ok(log) => {
                    let mut cmd = std::process::Command::new("./check.sh");
                    if fast {
                        cmd.arg("fast");
                    }
                    cmd.current_dir(&root)
                        .stdout(
                            log.reopen()
                                .map(std::process::Stdio::from)
                                .unwrap_or_else(|_| std::process::Stdio::null()),
                        )
                        .stderr(
                            log.reopen()
                                .map(std::process::Stdio::from)
                                .unwrap_or_else(|_| std::process::Stdio::null()),
                        );
                    let status = crate::run_cmd::run_with_timeout(cmd, timeout);
                    let text = std::fs::read_to_string(log.path()).unwrap_or_default();
                    let tail: String = text
                        .lines()
                        .rev()
                        .take(15)
                        .collect::<Vec<_>>()
                        .into_iter()
                        .rev()
                        .collect::<Vec<_>>()
                        .join("\n");
                    match status {
                        Some(s) => (s.success(), tail),
                        None => (
                            false,
                            format!("check.sh timed out after {timeout}s\n{tail}"),
                        ),
                    }
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

    /// The finding under the cursor, if any (Findings tab or last selection).
    fn selected_finding_ref(&self) -> Option<crate::findings::Finding> {
        crate::findings::aggregate(&self.findings, self.show_resolved)
            .get(self.selected_finding)
            .map(|af| af.finding.clone())
    }

    /// `f`/`d` on the findings tab: mark the selected finding fixed/dismissed
    /// (toggling back to pending), writing through to the source JSON.
    fn finding_set_action(&mut self, action: &str) {
        if self.tab != Tab::Findings {
            self.status_msg = Some(format!("{action} works on the findings tab (2)",));
            return;
        }
        let agg = crate::findings::aggregate(&self.findings, self.show_resolved);
        let Some(af) = agg.get(self.selected_finding) else {
            self.status_msg = Some("no finding selected".into());
            return;
        };
        let (file_idx, pos, title) = (af.file_idx, af.pos, af.finding.title.clone());
        match crate::findings::set_action(&mut self.findings, file_idx, pos, action) {
            Ok(()) => {
                let now = &self.findings[file_idx].file.findings[pos].action;
                self.status_msg = Some(format!("{title}: {now}"));
            }
            Err(e) => self.status_msg = Some(format!("could not update finding: {e:#}")),
        }
        self.clamp_selected_finding();
    }

    /// Keep the selection valid after resolving/toggling changes the list.
    fn clamp_selected_finding(&mut self) {
        let len = crate::findings::aggregate(&self.findings, self.show_resolved).len();
        self.selected_finding = self.selected_finding.min(len.saturating_sub(1));
    }

    /// `o`: open the selected finding in a RUNNING nvim (no TUI suspend).
    /// Falls back to the attached $EDITOR flow when no server is found.
    fn nvim_open(&mut self) {
        let Some(finding) = self.selected_finding_ref() else {
            self.status_msg = Some("no finding selected".into());
            return;
        };
        let Some(file) = finding.file.clone() else {
            self.status_msg = Some("finding has no file location".into());
            return;
        };
        let server = self
            .agents
            .nvim
            .clone()
            .or_else(|| crate::nvim::discover(self.cfg.nvim_server.as_deref()));
        let Some(server) = server else {
            self.status_msg = Some("no running nvim found — falling back to $EDITOR".into());
            self.open_editor();
            return;
        };
        let cwd = self
            .run_cwd()
            .unwrap_or_else(|| self.dirs.work_root.clone());
        let path = cwd.join(&file);
        match crate::nvim::open_at(&server, &path, finding.line) {
            Ok(()) => {
                self.status_msg = Some(format!(
                    " nvim: {}{}",
                    file,
                    finding.line.map(|l| format!(":{l}")).unwrap_or_default()
                ));
            }
            Err(e) => self.status_msg = Some(format!("nvim: {e:#}")),
        }
    }

    /// `Q`: push every located finding into the remote nvim quickfix list.
    fn nvim_quickfix(&mut self) {
        let server = self
            .agents
            .nvim
            .clone()
            .or_else(|| crate::nvim::discover(self.cfg.nvim_server.as_deref()));
        let Some(server) = server else {
            self.status_msg = Some("no running nvim found (start nvim or set nvim_server)".into());
            return;
        };
        let cwd = self
            .run_cwd()
            .unwrap_or_else(|| self.dirs.work_root.clone());
        let entries: Vec<crate::nvim::QfEntry> =
            crate::findings::aggregate(&self.findings, self.show_resolved)
                .into_iter()
                .map(|af| af.finding)
                .filter_map(|f| {
                    let file = f.file.as_ref()?;
                    Some(crate::nvim::QfEntry {
                        file: cwd.join(file).display().to_string(),
                        line: f.line.unwrap_or(1),
                        text: format!(
                            "{}{}: {} [{}]",
                            f.severity.label(),
                            if f.cross_confirmed() { " ◆both" } else { "" },
                            f.title,
                            f.verdict
                        ),
                    })
                })
                .collect();
        match crate::nvim::send_quickfix(&server, &entries, "ritual findings") {
            Ok(n) => self.status_msg = Some(format!(" {n} finding(s) → nvim quickfix")),
            Err(e) => self.status_msg = Some(format!("nvim: {e:#}")),
        }
    }

    fn open_editor(&mut self) {
        let agg = crate::findings::aggregate(&self.findings, self.show_resolved);
        let Some(finding) = agg.get(self.selected_finding).map(|af| &af.finding) else {
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
        let cwd = self
            .run_cwd()
            .unwrap_or_else(|| self.dirs.work_root.clone());
        self.pending_attached = Some(AttachedRequest {
            stage: None,
            argv,
            cwd,
        });
    }
}

/// The newest live run the TUI can resume. Chat runs ("spec-chat" etc.) have
/// stages that don't parse to a StageId and stay daemon-only — a newer live
/// chat run must never shadow an older pipeline run (follow chat runs with
/// `ritual attach` instead).
fn newest_resumable_run(dirs: &RitualDirs) -> Option<(String, runner::RunStatus)> {
    runner::live_runs(dirs)
        .into_iter()
        .rev()
        .find(|(_, s)| StageId::parse(&s.stage).is_some())
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

    // Finalize stages whose runs completed while nobody was watching, then
    // reattach to any run that is still alive.
    app.reconcile_stale_runs();
    if let Some((run_id, status)) = newest_resumable_run(&app.dirs) {
        app.resume_run(run_id, status, &tx);
    }
    let watcher = crate::watcher::spawn(app.dirs.work_root.clone(), tx.clone()).ok();

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
            w.paused.store(
                app.running.is_some() || app.chat_running(),
                std::sync::atomic::Ordering::SeqCst,
            );
        }

        if let Some(req) = app.take_attached() {
            if let Some(task) = input.take() {
                task.stop().await; // crossterm reader is global: MUST join first
            }
            if let Some(w) = &watcher {
                w.paused.store(true, std::sync::atomic::Ordering::SeqCst);
            }
            // std::process blocks; tell tokio so the worker thread is compensated.
            let ok = tokio::task::block_in_place(|| term.run_attached(&req.argv, &req.cwd))?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::Feature;

    /// A throwaway App backed by a temp `.ritual/`. The single seeded feature
    /// is "detached" (no git in the tempdir).
    fn test_app() -> (
        tempfile::TempDir,
        App,
        mpsc::Sender<AppMsg>,
        mpsc::Receiver<AppMsg>,
    ) {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join(".ritual")).unwrap();
        let dirs = RitualDirs::new(tmp.path());
        let app = App::new(Config::default(), dirs).unwrap();
        // Keep the receiver alive so pure-state dispatches that never send
        // still hold a valid channel.
        let (tx, rx) = mpsc::channel(64);
        (tmp, app, tx, rx)
    }

    fn send(app: &mut App, tx: &mpsc::Sender<AppMsg>, code: KeyCode) {
        app.chat_input(KeyEvent::new(code, KeyModifiers::NONE), tx);
    }

    #[test]
    fn resurrection_skips_live_chat_runs_for_older_pipeline_runs() {
        let tmp = tempfile::tempdir().unwrap();
        let runs = tmp.path().join(".ritual/runs");
        std::fs::create_dir_all(&runs).unwrap();
        let pid = std::process::id(); // our own pid: definitely alive
        // Older pipeline run + newer chat run, both "live".
        std::fs::write(
            runs.join("20260712T000001Z-1-1-plan-review.status"),
            format!(r#"{{"pid":{pid},"stage":"plan-review","branch":"main"}}"#),
        )
        .unwrap();
        std::fs::write(
            runs.join("20260712T000002Z-1-2-spec-chat.status"),
            format!(r#"{{"pid":{pid},"stage":"spec-chat","branch":"main"}}"#),
        )
        .unwrap();
        let dirs = RitualDirs::new(tmp.path());
        let (run_id, status) =
            newest_resumable_run(&dirs).expect("pipeline run should be resumable");
        assert!(run_id.ends_with("plan-review"), "picked {run_id}");
        assert_eq!(status.stage, "plan-review");

        // Only chat runs live -> nothing to resume (they stay daemon-only).
        std::fs::remove_file(runs.join("20260712T000001Z-1-1-plan-review.status")).unwrap();
        assert!(newest_resumable_run(&dirs).is_none());
    }

    #[test]
    fn finding_lifecycle_marks_and_clamps() {
        let (_t, mut app, tx, _rx) = test_app();
        std::fs::create_dir_all(app.dirs.findings_dir()).unwrap();
        std::fs::write(
            app.dirs
                .findings_dir()
                .join("20260712T000000Z-dual-review.json"),
            r#"{"stage":"dual-review","findings":[
                {"title":"a","severity":"critical","verdict":"confirmed"},
                {"title":"b","severity":"major","verdict":"confirmed"}]}"#,
        )
        .unwrap();
        app.findings = crate::findings::load_all(&app.dirs.findings_dir()).unwrap();
        app.tab = Tab::Findings;
        app.selected_finding = 1; // "b" (major sorts after critical)

        app.dispatch(Action::FindingDismiss, &tx);
        // b is dismissed and hidden; selection clamped to the one visible.
        let agg = crate::findings::aggregate(&app.findings, false);
        assert_eq!(agg.len(), 1);
        assert_eq!(app.selected_finding, 0);
        // The write went through to disk.
        let text = std::fs::read_to_string(
            app.dirs
                .findings_dir()
                .join("20260712T000000Z-dual-review.json"),
        )
        .unwrap();
        assert!(text.contains("dismissed"));

        // v shows resolved again.
        app.dispatch(Action::ToggleResolved, &tx);
        assert!(app.show_resolved);
        assert_eq!(
            crate::findings::aggregate(&app.findings, app.show_resolved).len(),
            2
        );

        // f on the wrong tab is a hint, not a mutation.
        app.tab = Tab::Live;
        app.dispatch(Action::FindingFix, &tx);
        assert!(
            app.status_msg
                .as_deref()
                .unwrap_or("")
                .contains("findings tab")
        );
    }

    #[test]
    fn chat_input_edits_with_a_real_cursor() {
        let (_t, mut app, tx, _rx) = test_app();
        app.open_chat();
        for c in "helo".chars() {
            send(&mut app, &tx, KeyCode::Char(c));
        }
        // Insert the missing 'l' mid-string: Left then type.
        send(&mut app, &tx, KeyCode::Left);
        send(&mut app, &tx, KeyCode::Char('l'));
        assert_eq!(
            app.chat.as_ref().unwrap().input.iter().collect::<String>(),
            "hello"
        );
        assert_eq!(app.chat.as_ref().unwrap().cursor, 4);

        send(&mut app, &tx, KeyCode::End);
        send(&mut app, &tx, KeyCode::Backspace); // drop trailing 'o'
        send(&mut app, &tx, KeyCode::Home);
        send(&mut app, &tx, KeyCode::Delete); // drop leading 'h'
        assert_eq!(
            app.chat.as_ref().unwrap().input.iter().collect::<String>(),
            "ell"
        );

        // Bounds never panic.
        send(&mut app, &tx, KeyCode::Left); // already at 0
        send(&mut app, &tx, KeyCode::Backspace); // cursor 0, no-op
        assert_eq!(app.chat.as_ref().unwrap().cursor, 0);
        assert_eq!(
            app.chat.as_ref().unwrap().input.iter().collect::<String>(),
            "ell"
        );
    }

    #[test]
    fn chat_ctrl_chords_do_not_type_letters() {
        let (_t, mut app, tx, _rx) = test_app();
        app.open_chat();
        // Ctrl+Z on a fresh chat: no snapshot -> "nothing to undo" note, and
        // crucially no literal 'z' lands in the input.
        app.chat_input(
            KeyEvent::new(KeyCode::Char('z'), KeyModifiers::CONTROL),
            &tx,
        );
        assert!(app.chat.as_ref().unwrap().input.is_empty());
        assert!(matches!(
            app.chat.as_ref().unwrap().transcript.last(),
            Some(ChatTurn::System(n)) if n.contains("nothing to undo")
        ));
        // Alt+q: swallowed, not typed.
        app.chat_input(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::ALT), &tx);
        assert!(app.chat.as_ref().unwrap().input.is_empty());
        // Shift-produced uppercase still types.
        app.chat_input(KeyEvent::new(KeyCode::Char('H'), KeyModifiers::SHIFT), &tx);
        assert_eq!(
            app.chat.as_ref().unwrap().input.iter().collect::<String>(),
            "H"
        );
    }

    #[test]
    fn chat_alt_enter_inserts_newline_and_enter_still_submits() {
        let (_t, mut app, tx, _rx) = test_app();
        app.open_chat();
        for c in "ab".chars() {
            app.chat_input(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE), &tx);
        }
        app.chat_input(KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT), &tx);
        for c in "cd".chars() {
            app.chat_input(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE), &tx);
        }
        let chat = app.chat.as_ref().unwrap();
        assert_eq!(chat.input.iter().collect::<String>(), "ab\ncd");
        assert_eq!(chat.cursor, 5);
        // chat_take_submit keeps the newline in the message.
        let msg = app.chat_take_submit().unwrap();
        assert_eq!(msg, "ab\ncd");
    }

    #[test]
    fn chat_undo_swaps_doc_and_snapshot() {
        let (_t, mut app, tx, _rx) = test_app();
        app.open_chat();
        let spec = app.dirs.spec_file(&app.slug);
        // Simulate an edit cycle: snapshot the old content, then "Claude"
        // writes new content (this is what spawn_doc_chat does pre-run).
        std::fs::write(&spec, "OLD SPEC\n").unwrap();
        std::fs::write(app.dirs.undo_file(&app.slug, "spec"), "OLD SPEC\n").unwrap();
        std::fs::write(&spec, "NEW SPEC\n").unwrap();

        let ctrl_z = KeyEvent::new(KeyCode::Char('z'), KeyModifiers::CONTROL);
        app.chat_input(ctrl_z, &tx);
        assert_eq!(std::fs::read_to_string(&spec).unwrap(), "OLD SPEC\n");
        // Swap = redo on second press.
        app.chat_input(ctrl_z, &tx);
        assert_eq!(std::fs::read_to_string(&spec).unwrap(), "NEW SPEC\n");
        // Blocked while in flight.
        app.chat.as_mut().unwrap().in_flight = true;
        app.chat_input(ctrl_z, &tx);
        assert_eq!(std::fs::read_to_string(&spec).unwrap(), "NEW SPEC\n");
        assert!(matches!(
            app.chat.as_ref().unwrap().transcript.last(),
            Some(ChatTurn::System(n)) if n.contains("in flight")
        ));
    }

    #[test]
    fn chat_enter_while_in_flight_queues_with_cap() {
        let (_t, mut app, tx, _rx) = test_app();
        app.open_chat();
        app.chat.as_mut().unwrap().in_flight = true;
        let type_and_enter = |app: &mut App, tx: &mpsc::Sender<AppMsg>, s: &str| {
            for c in s.chars() {
                app.chat_input(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE), tx);
            }
            app.chat_input(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), tx);
        };
        for i in 1..=3 {
            type_and_enter(&mut app, &tx, &format!("msg{i}"));
            assert_eq!(app.chat.as_ref().unwrap().pending.len(), i);
            assert!(app.chat.as_ref().unwrap().input.is_empty());
        }
        // Fourth message: queue full, input retained so nothing is lost.
        type_and_enter(&mut app, &tx, "overflow");
        let chat = app.chat.as_ref().unwrap();
        assert_eq!(chat.pending.len(), 3);
        assert!(matches!(
            chat.transcript.last(),
            Some(ChatTurn::System(n)) if n.contains("queue full")
        ));
        assert_eq!(chat.input.iter().collect::<String>(), "overflow");

        // Ctrl+X drops the queue.
        app.current_chat_run_id = Some("nope".into());
        app.chat_input(
            KeyEvent::new(KeyCode::Char('x'), KeyModifiers::CONTROL),
            &tx,
        );
        let chat = app.chat.as_ref().unwrap();
        assert!(chat.pending.is_empty());
        assert!(matches!(
            chat.transcript.last(),
            Some(ChatTurn::System(n)) if n.contains("3 queued")
        ));
    }

    #[test]
    fn chat_cancel_resets_in_flight() {
        let (_t, mut app, tx, _rx) = test_app();
        app.open_chat();
        // Nothing in flight: informational note only.
        app.chat_input(
            KeyEvent::new(KeyCode::Char('x'), KeyModifiers::CONTROL),
            &tx,
        );
        assert!(matches!(
            app.chat.as_ref().unwrap().transcript.last(),
            Some(ChatTurn::System(n)) if n.contains("nothing in flight")
        ));
        // In flight (no real daemon — kill_run on a missing id is a no-op).
        app.chat.as_mut().unwrap().in_flight = true;
        app.current_chat_run_id = Some("20260712T000000Z-0-0-spec-chat".into());
        app.chat_input(
            KeyEvent::new(KeyCode::Char('x'), KeyModifiers::CONTROL),
            &tx,
        );
        let chat = app.chat.as_ref().unwrap();
        assert!(!chat.in_flight, "cancel must clear in_flight");
        assert!(app.current_chat_run_id.is_none());
        assert!(matches!(
            chat.transcript.last(),
            Some(ChatTurn::System(n)) if n.contains("cancelled")
        ));
    }

    #[test]
    fn chat_input_is_utf8_safe() {
        let (_t, mut app, tx, _rx) = test_app();
        app.open_chat();
        for c in "café".chars() {
            send(&mut app, &tx, KeyCode::Char(c));
        }
        send(&mut app, &tx, KeyCode::Backspace); // removes 'é', not a byte
        assert_eq!(
            app.chat.as_ref().unwrap().input.iter().collect::<String>(),
            "caf"
        );
    }

    #[test]
    fn chat_submit_records_clears_and_guards() {
        let (_t, mut app, _tx, _rx) = test_app();
        app.open_chat();
        assert!(
            app.chat_take_submit().is_none(),
            "empty input submits nothing"
        );

        {
            let chat = app.chat.as_mut().unwrap();
            chat.input = "add retry".chars().collect();
            chat.cursor = chat.input.len();
        }
        assert_eq!(app.chat_take_submit().as_deref(), Some("add retry"));
        let chat = app.chat.as_ref().unwrap();
        assert!(chat.input.is_empty() && chat.cursor == 0);
        assert!(matches!(chat.transcript.last(), Some(ChatTurn::User(m)) if m == "add retry"));

        // A run in flight blocks new submits.
        app.chat.as_mut().unwrap().in_flight = true;
        app.chat.as_mut().unwrap().input = "again".chars().collect();
        assert!(app.chat_take_submit().is_none());
    }

    #[test]
    fn chat_tab_cycles_targets_and_esc_closes() {
        let (_t, mut app, tx, _rx) = test_app();
        app.open_chat();
        let n = app.chat.as_ref().unwrap().targets.len();
        assert!(n >= 5, "spec whole + 4 sections expected, got {n}");
        send(&mut app, &tx, KeyCode::Tab);
        assert_eq!(app.chat.as_ref().unwrap().target_idx, 1);
        send(&mut app, &tx, KeyCode::BackTab);
        assert_eq!(app.chat.as_ref().unwrap().target_idx, 0);
        send(&mut app, &tx, KeyCode::BackTab); // wraps to the last target
        assert_eq!(app.chat.as_ref().unwrap().target_idx, n - 1);
        send(&mut app, &tx, KeyCode::Esc);
        assert!(app.chat.is_none(), "esc closes the chat");
    }

    #[test]
    fn open_chat_seeds_spec_and_targets() {
        let (_t, mut app, _tx, _rx) = test_app();
        app.open_chat();
        // spec.md was created from the template.
        assert!(app.dirs.spec_file(&app.slug).exists());
        let chat = app.chat.as_ref().unwrap();
        // First target is the whole spec; a Behavior section target exists.
        assert!(matches!(chat.targets[0].doc, stages::DocKind::Spec));
        assert!(chat.targets[0].section.is_none());
        assert!(
            chat.targets
                .iter()
                .any(|t| t.section.as_deref() == Some("Behavior (the contract — WHAT, not HOW)"))
        );
    }

    #[test]
    fn next_tab_cycles_through_all_five_including_guide() {
        let (_t, mut app, _tx, _rx) = test_app();
        let seen: Vec<Tab> = (0..6)
            .map(|_| {
                let cur = app.tab;
                app.next_tab();
                cur
            })
            .collect();
        assert_eq!(
            seen,
            vec![
                Tab::Live,
                Tab::Findings,
                Tab::History,
                Tab::Plan,
                Tab::Guide,
                Tab::Live, // wrapped
            ]
        );
    }

    #[test]
    fn dispatch_tab_guide_selects_guide_tab() {
        let (_t, mut app, tx, _rx) = test_app();
        app.dispatch(Action::TabGuide, &tx);
        assert_eq!(app.tab, Tab::Guide);
    }

    #[test]
    fn nav_scrolls_the_tab_specific_buffer() {
        let (_t, mut app, tx, _rx) = test_app();

        app.tab = Tab::Plan;
        app.dispatch(Action::Down, &tx);
        app.dispatch(Action::Down, &tx);
        assert_eq!(app.plan_scroll, 2);
        assert_eq!(
            app.guide_scroll, 0,
            "guide buffer must not move on plan tab"
        );

        app.tab = Tab::Guide;
        app.dispatch(Action::Down, &tx);
        assert_eq!(app.guide_scroll, 1);
        assert_eq!(app.plan_scroll, 2, "plan buffer must not move on guide tab");

        // Up never underflows past zero.
        app.tab = Tab::Plan;
        app.plan_scroll = 0;
        app.dispatch(Action::Up, &tx);
        assert_eq!(app.plan_scroll, 0);
    }

    #[test]
    fn scroll_top_resets_the_active_tab_only() {
        let (_t, mut app, tx, _rx) = test_app();
        app.plan_scroll = 9;
        app.guide_scroll = 9;

        app.tab = Tab::Plan;
        app.dispatch(Action::ScrollTop, &tx);
        assert_eq!(app.plan_scroll, 0);
        assert_eq!(app.guide_scroll, 9);

        app.tab = Tab::Guide;
        app.dispatch(Action::ScrollTop, &tx);
        assert_eq!(app.guide_scroll, 0);
    }

    #[test]
    fn greeter_nav_moves_pipeline_selection_when_stream_empty() {
        let (_t, mut app, tx, _rx) = test_app();
        app.tab = Tab::Live;
        assert!(app.stream.is_empty());
        app.selected = 0;
        app.dispatch(Action::Down, &tx);
        assert_eq!(
            app.selected, 1,
            "greeter j/k should move the stage highlight"
        );
        app.dispatch(Action::Up, &tx);
        assert_eq!(app.selected, 0);
        app.dispatch(Action::Up, &tx); // wraps to the last stage
        assert_eq!(app.selected, PIPELINE.len() - 1);

        // With a live stream, j/k scrolls it instead of moving the selection.
        app.stream.push(AgentEvent::Text {
            text: "line".into(),
        });
        app.selected = 2;
        app.dispatch(Action::Up, &tx);
        assert_eq!(
            app.selected, 2,
            "selection frozen while a stream is present"
        );
    }

    #[test]
    fn nav_wraps_pipeline_selection_on_history_tab() {
        let (_t, mut app, tx, _rx) = test_app();
        app.tab = Tab::History; // the tab that drives sidebar selection
        app.selected = 0;
        app.dispatch(Action::Up, &tx); // wrap backwards to the last stage
        assert_eq!(app.selected, PIPELINE.len() - 1);
        app.dispatch(Action::Down, &tx); // wrap forward to the first
        assert_eq!(app.selected, 0);
    }

    #[test]
    fn selected_stage_clamps_out_of_range_index() {
        let (_t, mut app, _tx, _rx) = test_app();
        app.selected = 999;
        assert_eq!(app.selected_stage(), PIPELINE[PIPELINE.len() - 1]);
    }

    #[test]
    fn feature_order_lists_needs_you_features_first() {
        let (_t, mut app, _tx, _rx) = test_app();
        // Two extra features; "beta" has a failed stage (needs you).
        let mut alpha = Feature::new("alpha", "Alpha");
        alpha.updated_at = chrono::Utc::now();
        let mut beta = Feature::new("beta", "Beta");
        beta.stages.get_mut(&StageId::Implement).unwrap().status = StageStatus::Failed;
        app.state.features.insert("alpha".into(), alpha);
        app.state.features.insert("beta".into(), beta);

        assert!(app.feature_needs_you("beta"));
        assert!(!app.feature_needs_you("alpha"));
        let order = app.feature_order();
        assert_eq!(order.first().map(String::as_str), Some("beta"));
    }

    #[test]
    fn select_feature_cycles_branch_and_slug() {
        let (_t, mut app, tx, _rx) = test_app();
        app.state
            .features
            .insert("other".into(), Feature::new("other", "Other"));
        let start = app.slug.clone();
        app.dispatch(Action::FeatureNext, &tx);
        assert_ne!(app.slug, start, "slug should move to the other feature");
        // The viewed branch tracks the feature.
        assert_eq!(app.branch, app.state.features[&app.slug].branch);
    }

    #[test]
    fn palette_filter_matches_fuzzy_and_surfaces_custom_commands() {
        let (_t, mut app, _tx, _rx) = test_app();
        app.cfg.commands = vec![("deploy preview".into(), "echo hi".into())];

        app.palette = Some(PaletteState {
            input: "guide".into(),
            selected: 0,
        });
        let labels: Vec<String> = app.palette_filtered().into_iter().map(|(l, _)| l).collect();
        assert!(labels.iter().any(|l| l.contains("guide tab")));

        // Custom command is fuzzy-reachable and dispatches Action::Custom.
        app.palette = Some(PaletteState {
            input: "cmddeploy".into(),
            selected: 0,
        });
        let matches = app.palette_filtered();
        assert!(
            matches
                .iter()
                .any(|(l, a)| l == "cmd: deploy preview" && matches!(a, Action::Custom(0)))
        );
    }

    #[test]
    fn palette_input_types_navigates_and_executes() {
        let (_t, mut app, tx, _rx) = test_app();
        app.palette = Some(PaletteState::default());

        // Typing filters and resets the cursor.
        for c in "go to guide".chars() {
            app.palette_input(KeyCode::Char(c), &tx);
        }
        assert_eq!(app.palette.as_ref().unwrap().input, "go to guide");
        assert_eq!(app.palette.as_ref().unwrap().selected, 0);

        // Enter executes the top match and closes the palette.
        app.palette_input(KeyCode::Enter, &tx);
        assert!(app.palette.is_none());
        assert_eq!(app.tab, Tab::Guide);
    }

    #[test]
    fn palette_input_esc_closes_without_acting() {
        let (_t, mut app, tx, _rx) = test_app();
        let before = app.tab;
        app.palette = Some(PaletteState::default());
        app.palette_input(KeyCode::Esc, &tx);
        assert!(app.palette.is_none());
        assert_eq!(app.tab, before);
    }

    #[test]
    fn palette_down_is_bounded_by_match_count() {
        let (_t, mut app, tx, _rx) = test_app();
        // A filter that matches exactly one entry: Down must not advance past it.
        app.palette = Some(PaletteState {
            input: "quitritual".into(),
            selected: 0,
        });
        let n = app.palette_filtered().len();
        assert_eq!(n, 1, "expected a single match for the test");
        app.palette_input(KeyCode::Down, &tx);
        assert_eq!(app.palette.as_ref().unwrap().selected, 0);
    }

    #[test]
    fn after_attached_spec_marks_done_only_with_real_content() {
        let (_t, mut app, _tx, _rx) = test_app();
        let spec = app.dirs.spec_file(&app.slug);
        std::fs::create_dir_all(spec.parent().unwrap()).unwrap();

        // Only comments/blank lines -> stays pending.
        std::fs::write(&spec, "# title\n<!-- note -->\n\n").unwrap();
        app.after_attached(Some(StageId::Spec), true);
        assert_eq!(app.stage_status(StageId::Spec), StageStatus::Pending);

        // Real content -> done.
        std::fs::write(&spec, "# title\n\nImplement the widget.\n").unwrap();
        app.after_attached(Some(StageId::Spec), true);
        assert_eq!(app.stage_status(StageId::Spec), StageStatus::Done);
    }

    #[test]
    fn after_attached_plan_requires_a_written_file() {
        let (_t, mut app, _tx, _rx) = test_app();
        // No plan.md yet -> needs attention.
        app.after_attached(Some(StageId::Plan), true);
        assert_eq!(app.stage_status(StageId::Plan), StageStatus::NeedsAttention);

        // Write the plan -> done.
        let plan = app.dirs.plan_file(&app.slug);
        std::fs::create_dir_all(plan.parent().unwrap()).unwrap();
        std::fs::write(&plan, "# Plan\n").unwrap();
        app.after_attached(Some(StageId::Plan), true);
        assert_eq!(app.stage_status(StageId::Plan), StageStatus::Done);
    }

    #[test]
    fn after_attached_review_stage_follows_child_exit() {
        let (_t, mut app, _tx, _rx) = test_app();
        app.after_attached(Some(StageId::DualReview), true);
        assert_eq!(app.stage_status(StageId::DualReview), StageStatus::Done);
        app.after_attached(Some(StageId::DualReview), false);
        assert_eq!(app.stage_status(StageId::DualReview), StageStatus::Failed);
    }
}
