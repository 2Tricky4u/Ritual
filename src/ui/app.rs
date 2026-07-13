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
    /// A claude plan fix finished; the second field is the run's final
    /// assistant text (the ANSWERS block source), captured off the tail.
    FixExited(Box<Result<RunOutcome>>, Option<String>),
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

/// Everything `on_fix_exited` needs to gate + write back a claude plan fix.
/// The findings file is tracked by PATH, not index: `reload_artifacts`
/// invalidates indices, and the fix run can't touch findings JSON (its tool
/// lock only allows the plan file), so path+pos stay stable.
#[derive(Debug)]
struct FixCtx {
    findings_path: std::path::PathBuf,
    pos: usize,
    title: String,
    slug: String,
    /// Branch the findings file came from (meta records, notifications).
    branch: String,
    plan_path: std::path::PathBuf,
    /// `None` = the step couldn't be located: whole-doc scope, gate skipped.
    section: Option<String>,
    range: std::ops::Range<usize>,
}

/// The last applied fix, so `u` can revert it (and reopen its finding).
struct LastFix {
    slug: String,
    plan_path: std::path::PathBuf,
    findings_path: std::path::PathBuf,
    pos: usize,
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
    /// Detail overlay over the selected finding (Enter on the findings tab).
    /// Stateless: it renders whatever the cursor points at.
    pub finding_detail: bool,
    /// Show findings already marked fixed/dismissed (toggled with `v`).
    pub show_resolved: bool,
    /// `/` filter over the findings/history lists (empty = inactive).
    pub filter: String,
    /// True while typing the filter (keys feed it instead of navigating).
    pub filter_editing: bool,
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
    /// One-shot model override consumed by the next stage build (retry-with).
    pending_model_override: Option<String>,
    fix_task: Option<JoinHandle<()>>,
    current_fix_run_id: Option<String>,
    /// Plan content snapshot from just before the fix run (scope gate + revert).
    fix_doc_before: String,
    /// The in-flight claude plan fix, if any (one at a time).
    fix_ctx: Option<FixCtx>,
    /// The last APPLIED fix, revertable with `u` until a newer doc edit lands.
    last_fix: Option<LastFix>,
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
    /// The document doesn't exist yet; the first message drafts it.
    pub missing: bool,
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
            None if self.missing => format!("{} (draft from spec)", self.doc.label()),
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
    /// each edit finishes (capped, since this is a chat, not a job queue).
    pub pending: std::collections::VecDeque<String>,
}

/// Beyond this the user should wait: queued edits compound unpredictably.
const CHAT_QUEUE_CAP: usize = 3;

impl ChatState {
    pub fn target(&self) -> Option<&ChatTarget> {
        self.targets.get(self.target_idx)
    }
}

/// First `max` chars of a heading, ellipsized to keep the chat header tidy.
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
            finding_detail: false,
            show_resolved: false,
            filter: String::new(),
            filter_editing: false,
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
            pending_model_override: None,
            fix_task: None,
            current_fix_run_id: None,
            fix_doc_before: String::new(),
            fix_ctx: None,
            last_fix: None,
        })
    }

    /// True while a claude plan fix (`F`) is running.
    pub fn fix_running(&self) -> bool {
        self.fix_ctx.is_some()
    }

    /// Statusline / overlay label for the in-flight fix, e.g. `fix §Steps`.
    pub fn fix_label(&self) -> Option<String> {
        self.fix_ctx.as_ref().map(|c| match &c.section {
            Some(name) => format!("fix §{}", first_words(name, 18)),
            None => "fix plan".into(),
        })
    }

    /// True once a fix has been applied and `u` would revert it.
    pub fn fix_revertable(&self) -> bool {
        self.last_fix.is_some()
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
        // Retry-with-model: offered only where it can act, on a failed (or
        // needs-attention) headless stage, with [retry] models configured.
        if let Some(feature) = self.state.features.get(&self.slug) {
            for id in [StageId::PlanReview, StageId::DualReview] {
                if matches!(
                    feature.stage(id).status,
                    StageStatus::Failed | StageStatus::NeedsAttention
                ) {
                    for (i, m) in self.cfg.retry_models.iter().enumerate() {
                        entries.push((
                            format!("retry {} with {m}", id.label()),
                            Action::RetryStage(id, i),
                        ));
                    }
                }
            }
        }
        entries
            .into_iter()
            .filter(|(label, _)| keymap::fuzzy_match(&filter, label))
            .collect()
    }

    pub fn selected_stage(&self) -> StageId {
        PIPELINE[self.selected.min(PIPELINE.len() - 1)]
    }

    /// How many runs this stage has recorded (×N marker once retried).
    pub fn stage_attempts(&self, id: StageId) -> usize {
        self.state
            .features
            .get(&self.slug)
            .map(|f| f.stage(id).runs.len())
            .unwrap_or(0)
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
            AppMsg::FixExited(outcome, result_text) => {
                self.on_fix_exited(*outcome, result_text, tx)
            }
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
                    && !self.fix_running()
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
        // Bracketed paste: route the whole blob into the active text surface
        // so a multi-line paste inserts literal newlines instead of arriving
        // as Enter presses that would submit the chat message mid-paste.
        if let Event::Paste(text) = ev {
            self.on_paste(&text);
            return;
        }
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
        if self.finding_detail {
            self.detail_input(key, tx);
            return;
        }
        if self.chat.is_some() {
            self.chat_input(key, tx);
            return;
        }
        if self.filter_editing {
            self.filter_input(key.code);
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

    /// The `/` filter is meaningful only on the findings and history lists.
    fn tab_is_filterable(&self) -> bool {
        matches!(self.tab, Tab::Findings | Tab::History)
    }

    /// A filter is showing (bar visible) when it has text or is being typed.
    pub fn filter_active(&self) -> bool {
        self.tab_is_filterable() && (self.filter_editing || !self.filter.is_empty())
    }

    /// Enter filter-typing mode (keeps existing text so it can be refined).
    fn start_filter(&mut self) {
        if self.tab_is_filterable() {
            self.filter_editing = true;
        }
    }

    /// Clear the filter and stop editing (on tab switch and on Esc).
    fn clear_filter(&mut self) {
        self.filter.clear();
        self.filter_editing = false;
    }

    /// Keys while typing the `/` filter: Enter keeps it and returns to nav,
    /// Esc clears it, the rest edits the needle and re-clamps the selection.
    fn filter_input(&mut self, code: KeyCode) {
        match code {
            KeyCode::Esc => self.clear_filter(),
            KeyCode::Enter => self.filter_editing = false,
            KeyCode::Backspace => {
                self.filter.pop();
                self.clamp_selected_finding();
            }
            KeyCode::Char(c) => {
                self.filter.push(c);
                self.clamp_selected_finding();
            }
            _ => {}
        }
    }

    /// Case-insensitive substring test used by both filtered lists.
    fn filter_hit(needle: &str, haystacks: &[&str]) -> bool {
        if needle.is_empty() {
            return true;
        }
        let n = needle.to_ascii_lowercase();
        haystacks
            .iter()
            .any(|h| h.to_ascii_lowercase().contains(&n))
    }

    /// The findings the findings tab shows: aggregated, then narrowed by the
    /// `/` filter. Every consumer (nav clamp, f/d actions, editor jump, the
    /// renderer) goes through here so indices stay consistent.
    pub fn visible_findings(&self) -> Vec<crate::findings::AggregatedFinding> {
        let mut agg = crate::findings::aggregate(&self.findings, self.show_resolved);
        if !self.filter.is_empty() {
            agg.retain(|af| {
                let f = &af.finding;
                let loc = f.location();
                Self::filter_hit(&self.filter, &[&f.title, &loc, &f.scenario, &f.verdict])
            });
        }
        agg
    }

    /// The run metas the history tab shows, narrowed by the `/` filter.
    pub fn visible_metas(&self) -> Vec<&RunMeta> {
        self.metas
            .iter()
            .filter(|m| {
                self.filter.is_empty()
                    || Self::filter_hit(
                        &self.filter,
                        &[
                            &m.stage,
                            &m.agent,
                            &m.run_id,
                            m.model.as_deref().unwrap_or(""),
                        ],
                    )
            })
            .collect()
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
        let prev_tab = self.tab;
        match action {
            Action::Quit => {
                if self.running.is_some() || self.fix_running() {
                    // Quitting mid-fix would also skip the scope gate: the
                    // detached run finishes ungated (plan-fix never resumes).
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
            Action::SpecChat => self.open_chat(tx),
            Action::Filter => self.start_filter(),
            Action::FindingFix => self.finding_set_action("fixed"),
            Action::FindingDismiss => self.finding_set_action("dismissed"),
            Action::FindingClaudeFix => self.spawn_finding_fix(tx),
            Action::DocUndo => self.doc_undo(),
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
            Action::RetryStage(id, model_idx) => {
                self.pending_model_override = self.cfg.retry_models.get(model_idx).cloned();
                if let Some(idx) = PIPELINE.iter().position(|s| *s == id) {
                    self.selected = idx;
                }
                self.tab = Tab::Live;
                self.on_enter(tx);
            }
        }
        // A filter is scoped to its list; leaving the tab drops it so it
        // can't silently empty the next tab's view.
        if self.tab != prev_tab {
            self.clear_filter();
        }
    }

    fn next_tab(&mut self) {
        let idx = TABS.iter().position(|(t, _)| *t == self.tab).unwrap_or(0);
        self.tab = TABS[(idx + 1) % TABS.len()].0;
    }

    fn nav(&mut self, delta: i32) {
        match self.tab {
            Tab::Findings => {
                let len = self.visible_findings().len();
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
            // Enter opens the detail overlay ($EDITOR stays on `e`).
            if self.selected_finding_af().is_some() {
                self.finding_detail = true;
            } else {
                self.status_msg = Some("no finding selected".into());
            }
            return;
        }
        if self.running.is_some() || self.chat_running() {
            self.status_msg = Some("a run is already active; press x to cancel".into());
            return;
        }
        let Some(run_cwd) = self.run_cwd() else {
            self.status_msg = Some(format!(
                "branch '{}' has no checkout; run `ritual new --worktree {}` or switch to it",
                self.branch, self.branch
            ));
            return;
        };
        let stage = self.selected_stage();
        let model_override = self.pending_model_override.take();
        let cmd = match stages::build(
            stage,
            &self.cfg,
            &self.dirs,
            &self.slug,
            None,
            model_override.as_deref(),
        ) {
            Ok(c) => c,
            Err(e) => {
                self.status_msg = Some(format!("{e:#}"));
                return;
            }
        };
        if cmd.needs_codex && self.agents.codex_cli_ok == Some(false) {
            self.status_msg = Some("codex not authenticated; run `codex login`".into());
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
                "daily budget reached (${spent:.2}/${budget:.2}); run `ritual run {} --force` to override",
                stage.label()
            ));
            return;
        }
        // Pre-review gates run BEFORE the findings snapshot so their artifacts
        // don't count as the agent run's own output.
        if stage == StageId::DualReview {
            let _ = crate::lessons::refresh(&self.dirs);
            if let Some(msg) = crate::secrets::preflight(&self.cfg, &self.dirs) {
                self.status_msg = Some(msg);
            }
            // Cloud review can take minutes, so run it off the event loop; its
            // findings file lands via the .ritual watcher when done.
            if self.cfg.coderabbit_enabled {
                let (cfg, dirs) = (self.cfg.clone(), self.dirs.clone());
                std::thread::spawn(move || {
                    let _ = crate::coderabbit::preflight(&cfg, &dirs);
                });
            }
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
            wrapper: stages::wrapper_argv(&self.cfg, cmd.mode),
        };
        let dirs = self.dirs.clone();
        let cfg = self.cfg.clone();
        let run_id = runner::new_run_id(stage.label());
        self.current_run_id = Some(run_id.clone());
        let tx_events = tx.clone();
        let tx_done = tx.clone();
        self.run_task = Some(tokio::spawn(async move {
            // Provenance collection shells out (git, --version), so keep it off
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
    fn open_chat(&mut self, tx: &mpsc::Sender<AppMsg>) {
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
        let mut chat = ChatState {
            transcript: Vec::new(),
            input: Vec::new(),
            cursor: 0,
            targets,
            target_idx: 0,
            scroll: 0,
            in_flight: false,
            pending: Default::default(),
        };
        // Reattach: a chat edit daemonized before the TUI died is still live,
        // so rebuild the view around it instead of orphaning it (the archive
        // replay repaints the assistant turn; completion lands normally).
        if self.chat_task.is_none()
            && let Some((run_id, status)) = runner::live_runs(&self.dirs)
                .into_iter()
                .rev()
                .find(|(_, s)| s.stage.ends_with("-chat") && s.branch == self.branch)
        {
            let doc = if status.stage.starts_with("plan") {
                stages::DocKind::Plan
            } else {
                stages::DocKind::Spec
            };
            if let Some(i) = chat
                .targets
                .iter()
                .position(|t| t.doc == doc && t.section.is_none())
            {
                chat.target_idx = i;
            }
            let doc_path = match doc {
                stages::DocKind::Plan => self.dirs.plan_file(&self.slug),
                stages::DocKind::Spec => self.dirs.spec_file(&self.slug),
            };
            self.doc_before = std::fs::read_to_string(&doc_path).unwrap_or_default();
            chat.in_flight = true;
            chat.transcript.push(ChatTurn::System(format!(
                "reattached to in-flight {} ({run_id})",
                status.stage
            )));
            let agent = runner::load_request(&self.dirs, &run_id)
                .map(|r| r.agent)
                .unwrap_or(runner::AgentKind::Claude);
            self.attach_chat_tail(run_id, agent, tx);
        }
        self.chat = Some(chat);
    }

    /// The editable targets: spec (whole + each `##` section), then plan the
    /// same way if plan.md exists.
    fn build_chat_targets(&self) -> Vec<ChatTarget> {
        let mut targets = Vec::new();
        for (doc, path) in [
            (stages::DocKind::Spec, self.dirs.spec_file(&self.slug)),
            (stages::DocKind::Plan, self.dirs.plan_file(&self.slug)),
        ] {
            let missing = !path.exists();
            let text = std::fs::read_to_string(&path).unwrap_or_default();
            let n = text.lines().count().max(1);
            // A missing plan is still a target: the first message DRAFTS it
            // from the spec (whole-doc only; sections appear once it exists).
            targets.push(ChatTarget {
                doc,
                section: None,
                range: 0..n,
                missing,
            });
            for (name, range) in crate::spec::sections(&text) {
                targets.push(ChatTarget {
                    doc,
                    section: Some(name),
                    range,
                    missing: false,
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
        if key.code == KeyCode::Char('z') && key.modifiers.contains(KeyModifiers::ALT) {
            self.chat_undo_redo(false);
            return;
        }
        if key.code == KeyCode::Char('z') && key.modifiers.contains(KeyModifiers::CONTROL) {
            self.chat_undo_redo(true);
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
                            "queue full; wait for the current edit".into(),
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

    /// A bracketed-paste blob lands in whichever text surface is focused:
    /// the chat input (newlines kept literal, no mid-paste submit) or the
    /// palette filter (flattened to one line, since a filter is single-line). Any
    /// other context ignores it.
    fn on_paste(&mut self, text: &str) {
        if let Some(chat) = self.chat.as_mut() {
            for c in text.chars() {
                chat.input.insert(chat.cursor, c);
                chat.cursor += 1;
            }
            chat.scroll = 0;
        } else if let Some(p) = self.palette.as_mut() {
            p.input.extend(text.chars().filter(|c| !c.is_control()));
            p.selected = 0;
        }
    }

    /// Ctrl+X: kill an in-flight chat edit. The aborted tail task means
    /// on_chat_exited never fires for this run, so reset state here.
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
                "edit cancelled{}; Ctrl+Z restores the pre-edit document",
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
    /// Ctrl+Z walks the snapshot stack back one edit; Alt+Z walks forward
    /// again. Persisted stacks (cap 10) that survive TUI restarts.
    fn chat_undo_redo(&mut self, back: bool) {
        if self.chat.as_ref().is_some_and(|c| c.in_flight) || self.fix_running() {
            if let Some(chat) = self.chat.as_mut() {
                chat.transcript.push(ChatTurn::System(
                    "cannot undo while an edit is in flight; Ctrl+X to cancel first".into(),
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
        let label = target.doc.label();
        let result = if back {
            crate::undo::undo(&self.dirs, &self.slug, label, &doc_path)
        } else {
            crate::undo::redo(&self.dirs, &self.slug, label, &doc_path)
        };
        let note = match (back, result) {
            (true, Ok(true)) => {
                let left = crate::undo::depth(&self.dirs, &self.slug, label);
                format!("undid last edit ({left} more); Alt+Z to redo")
            }
            (false, Ok(true)) => "redid edit".to_string(),
            (true, Ok(false)) => format!("nothing to undo for {label}"),
            (false, Ok(false)) => format!("nothing to redo for {label}"),
            (_, Err(e)) => format!("undo failed: {e:#}"),
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
    /// the input, and returns the text to send, or None if empty or a run is
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
    /// transcript. Never touches `self.running`/`run_task`; the pipeline is
    /// independent of the chat.
    fn spawn_doc_chat(&mut self, message: String, tx: &mpsc::Sender<AppMsg>) {
        if let Some((spent, budget)) = crate::run_cmd::budget_exceeded(&self.cfg, &self.dirs) {
            if let Some(chat) = self.chat.as_mut() {
                chat.transcript.push(ChatTurn::System(format!(
                    "daily budget reached (${spent:.2}/${budget:.2}); run `ritual chat … --force` to override"
                )));
            }
            return;
        }
        // A plan fix owns the doc + undo stack: hold the message until it
        // lands (on_fix_exited drains the queue).
        if self.fix_running() {
            if let Some(chat) = self.chat.as_mut() {
                if chat.pending.len() < CHAT_QUEUE_CAP {
                    chat.pending.push_back(message);
                    chat.transcript.push(ChatTurn::System(
                        "a plan fix is running; message queued until it finishes".into(),
                    ));
                } else {
                    chat.transcript.push(ChatTurn::System(format!(
                        "a plan fix is running and {CHAT_QUEUE_CAP} edits are already queued; try again shortly"
                    )));
                }
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
        // Plan targets carry the spec path so a missing plan drafts from it.
        let spec_path = (target.doc == stages::DocKind::Plan
            && self.dirs.spec_file(&self.slug).exists())
        .then(|| self.dirs.spec_file(&self.slug));
        let invariants = stages::meaningful_invariants(&self.dirs);
        let cmd = stages::doc_chat_command(
            &self.cfg,
            &doc_path,
            target.doc,
            &target.scope(),
            &message,
            &context,
            spec_path.as_deref(),
            invariants.as_deref(),
        );
        self.doc_before = std::fs::read_to_string(&doc_path).unwrap_or_default();
        // Persist the pre-edit snapshot onto the undo stack (Ctrl+Z source,
        // survives restarts; a new edit invalidates the redo branch).
        let _ = std::fs::create_dir_all(self.dirs.feature_dir(&self.slug));
        let _ = crate::undo::push(&self.dirs, &self.slug, target.doc.label(), &self.doc_before);
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
            repro: None, // chat edits are frequent + small, so skip provenance
            cwd: run_cwd,
            wrapper: stages::wrapper_argv(&self.cfg, cmd.mode),
        };
        let run_id = runner::new_run_id(&stage_label);
        if let Err(e) = runner::spawn_detached(&self.dirs, &req, &run_id) {
            if let Some(chat) = self.chat.as_mut() {
                chat.in_flight = false;
                chat.transcript
                    .push(ChatTurn::System(format!("chat failed to start: {e:#}")));
            }
            return;
        }
        // A chat edit now sits above any applied fix on the shared undo
        // stack: `u` must not revert the wrong snapshot.
        self.last_fix = None;
        self.attach_chat_tail(run_id, req.agent, tx);
    }

    /// Follow a chat run (just spawned OR reattached after a TUI restart):
    /// `tail_run` replays the archive from byte 0 and then follows, so the
    /// assistant turn rebuilds itself through the normal ChatAgent path and
    /// completion lands as ChatExited either way.
    fn attach_chat_tail(
        &mut self,
        run_id: String,
        agent: runner::AgentKind,
        tx: &mpsc::Sender<AppMsg>,
    ) {
        let dirs = self.dirs.clone();
        self.current_chat_run_id = Some(run_id.clone());
        let tx_events = tx.clone();
        let tx_done = tx.clone();
        self.chat_task = Some(tokio::spawn(async move {
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
            "chat edit failed; see the transcript above".to_string()
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
        // send replaces the undo snapshot, so undo covers the LAST edit.
        // Held while a plan fix runs; on_fix_exited drains it instead.
        if !self.fix_running()
            && let Some(msg) = self.chat.as_mut().and_then(|c| c.pending.pop_front())
        {
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
        let agg = self.visible_findings();
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
                        "{}: {} new findings, ${:.2}",
                        self.branch,
                        new_findings.len(),
                        out.meta.total_cost_usd.unwrap_or(0.0)
                    ),
                );
                self.status_msg = Some(match status {
                    StageStatus::Done => format!(
                        "{} done: {} new findings file(s), ${:.3}",
                        stage.label(),
                        new_findings.len(),
                        out.meta.total_cost_usd.unwrap_or(0.0)
                    ),
                    StageStatus::NeedsAttention => format!(
                        "{} finished without findings, needs attention{}",
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
                        "plan.md not written; save it to {}",
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
        // The run is a detached process group, so kill it there, then stop
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
            self.status_msg = Some("no check.sh in this project; `ritual init` creates one".into());
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

    /// The aggregated finding under the cursor, if any.
    pub fn selected_finding_af(&self) -> Option<crate::findings::AggregatedFinding> {
        self.visible_findings()
            .into_iter()
            .nth(self.selected_finding)
    }

    /// Keys while the finding detail overlay is open: a whitelist of the
    /// findings-tab actions, resolved through the keymap so rebinds hold.
    /// The overlay is stateless (renders the cursor's finding), so j/k just
    /// move the cursor underneath it.
    fn detail_input(&mut self, key: KeyEvent, tx: &mpsc::Sender<AppMsg>) {
        if matches!(key.code, KeyCode::Esc | KeyCode::Enter) {
            self.finding_detail = false;
            return;
        }
        match self.cfg.keymap.resolve(key.code, key.modifiers) {
            Some(Action::Down) => self.nav(1),
            Some(Action::Up) => self.nav(-1),
            Some(Action::FindingFix) => {
                self.finding_set_action("fixed");
                self.finding_detail = false;
            }
            Some(Action::FindingDismiss) => {
                self.finding_set_action("dismissed");
                self.finding_detail = false;
            }
            // Stays open: the footer shows the in-flight spinner, and
            // on_fix_exited closes it with the verdict.
            Some(Action::FindingClaudeFix) => self.spawn_finding_fix(tx),
            Some(Action::DocUndo) => self.doc_undo(),
            Some(Action::NvimOpen) => self.nvim_open(), // stays open (remote jump)
            Some(Action::OpenEditor) => {
                self.finding_detail = false;
                self.open_editor(); // suspends the TUI into $EDITOR
            }
            Some(Action::Help) => self.show_help = true, // paints on top
            Some(Action::Quit) => self.finding_detail = false, // q closes a modal
            _ => {}
        }
    }

    /// The feature slug a finding belongs to: its findings-file branch,
    /// falling back to the feature currently in view. Undo push/pop and the
    /// plan path must both come from HERE so they always agree.
    fn finding_slug(&self, file_idx: usize) -> String {
        self.findings
            .get(file_idx)
            .map(|lf| lf.file.branch.as_str())
            .filter(|b| !b.is_empty())
            .map(state::branch_slug)
            .unwrap_or_else(|| self.slug.clone())
    }

    /// The plan document a plan-review finding (from findings-file `file_idx`)
    /// refers to.
    fn plan_path_for(&self, file_idx: usize) -> std::path::PathBuf {
        self.dirs.plan_file(&self.finding_slug(file_idx))
    }

    /// What "open" should target for a finding. Code findings use their own
    /// `file:line`; plan-review findings have no file but a `plan_step`, so they
    /// target the feature's plan document at that step's line (best-effort).
    /// Returns (absolute path, line, short label) or a status message to show.
    fn finding_open_target(
        &self,
        af: &crate::findings::AggregatedFinding,
    ) -> Result<(std::path::PathBuf, Option<u32>, String), &'static str> {
        let f = &af.finding;
        if let Some(file) = &f.file {
            let cwd = self
                .run_cwd()
                .unwrap_or_else(|| self.dirs.work_root.clone());
            return Ok((cwd.join(file), f.line, file.clone()));
        }
        if let Some(step) = &f.plan_step {
            let plan = self.plan_path_for(af.file_idx);
            let Ok(text) = std::fs::read_to_string(&plan) else {
                return Err(
                    "plan-review finding, but no plan.md on disk; run the plan stage first",
                );
            };
            return Ok((plan, locate_plan_step(&text, step), "plan.md".into()));
        }
        Err("finding has no file location")
    }

    /// `f`/`d` on the findings tab: mark the selected finding fixed/dismissed
    /// (toggling back to pending), writing through to the source JSON.
    fn finding_set_action(&mut self, action: &str) {
        if self.tab != Tab::Findings {
            self.status_msg = Some(format!("{action} works on the findings tab (2)",));
            return;
        }
        let agg = self.visible_findings();
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
        let len = self.visible_findings().len();
        self.selected_finding = self.selected_finding.min(len.saturating_sub(1));
    }

    /// `o`: open the selected finding in a RUNNING nvim (no TUI suspend).
    /// Falls back to the attached $EDITOR flow when no server is found.
    fn nvim_open(&mut self) {
        let Some(af) = self.selected_finding_af() else {
            self.status_msg = Some("no finding selected".into());
            return;
        };
        let (path, line, label) = match self.finding_open_target(&af) {
            Ok(t) => t,
            Err(msg) => {
                self.status_msg = Some(msg.into());
                return;
            }
        };
        let server = self
            .agents
            .nvim
            .clone()
            .or_else(|| crate::nvim::discover(self.cfg.nvim_server.as_deref()));
        let Some(server) = server else {
            self.status_msg = Some("no running nvim found; falling back to $EDITOR".into());
            self.open_editor();
            return;
        };
        match crate::nvim::open_at(&server, &path, line) {
            Ok(()) => {
                self.status_msg = Some(format!(
                    " nvim: {}{}",
                    label,
                    line.map(|l| format!(":{l}")).unwrap_or_default()
                ));
            }
            Err(e) => self.status_msg = Some(format!("nvim: {e:#}")),
        }
    }

    /// `Q`: push every locatable finding into the remote nvim quickfix list.
    /// Code findings anchor at their file:line; plan-review findings anchor in
    /// the plan document at the referenced step.
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
                .filter_map(|af| {
                    let f = &af.finding;
                    let (file, line) = if let Some(rel) = &f.file {
                        (cwd.join(rel).display().to_string(), f.line.unwrap_or(1))
                    } else if let Some(step) = &f.plan_step {
                        let plan = self.plan_path_for(af.file_idx);
                        let line = std::fs::read_to_string(&plan)
                            .ok()
                            .and_then(|t| locate_plan_step(&t, step))
                            .unwrap_or(1);
                        (plan.display().to_string(), line)
                    } else {
                        return None;
                    };
                    Some(crate::nvim::QfEntry {
                        file,
                        line,
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
        let Some(af) = self.selected_finding_af() else {
            self.status_msg = Some("no finding selected".into());
            return;
        };
        let (path, line, _label) = match self.finding_open_target(&af) {
            Ok(t) => t,
            Err(msg) => {
                self.status_msg = Some(msg.into());
                return;
            }
        };
        let editor = std::env::var("EDITOR").unwrap_or_else(|_| "vi".into());
        let mut argv = vec![editor];
        if let Some(line) = line {
            argv.push(format!("+{line}"));
        }
        argv.push(path.display().to_string());
        let cwd = self
            .run_cwd()
            .unwrap_or_else(|| self.dirs.work_root.clone());
        self.pending_attached = Some(AttachedRequest {
            stage: None,
            argv,
            cwd,
        });
    }

    /// Everything that must hold before a claude plan fix may spawn, plus the
    /// built command and write-back context. Pure of side effects except the
    /// undo push + `fix_doc_before` snapshot (the last two steps, after every
    /// guard has passed).
    fn prepare_finding_fix(&mut self) -> Result<(stages::StageCommand, FixCtx), String> {
        let Some(af) = self.selected_finding_af() else {
            return Err("no finding selected".into());
        };
        let f = &af.finding;
        if f.file.is_some() {
            return Err("claude fix targets plan findings only (v1); use o/e for code".into());
        }
        let Some(step) = f.plan_step.clone() else {
            return Err("finding has no plan step to fix".into());
        };
        if self.fix_running() {
            return Err("a plan fix is already running".into());
        }
        if self.chat_running() {
            return Err("a chat edit is in flight; wait for it to finish".into());
        }
        if let Some((spent, budget)) = crate::run_cmd::budget_exceeded(&self.cfg, &self.dirs) {
            return Err(format!(
                "daily budget reached (${spent:.2}/${budget:.2}); raise budget_daily_usd to override"
            ));
        }
        let slug = self.finding_slug(af.file_idx);
        let plan_path = self.dirs.plan_file(&slug);
        let Ok(text) = std::fs::read_to_string(&plan_path) else {
            return Err(
                "plan-review finding, but no plan.md on disk; run the plan stage first".into(),
            );
        };
        // Step -> line -> containing `##` section. Unlocatable steps fall back
        // to whole-doc scope: the gate is skipped, only undo protects.
        let (section, range) = match locate_plan_step(&text, &step).and_then(|line| {
            crate::spec::sections(&text)
                .into_iter()
                .find(|(_, r)| r.contains(&((line - 1) as usize)))
        }) {
            Some((name, range)) => (Some(name), range),
            None => (None, 0..text.lines().count()),
        };
        let scope = match &section {
            Some(name) => stages::Scope::Section(name.clone()),
            None => stages::Scope::Whole,
        };
        let brief = stages::FindingBrief {
            title: &f.title,
            severity: f.severity.label(),
            scenario: &f.scenario,
            plan_step: &step,
            snippet: f.snippet.as_deref(),
        };
        let spec_path = self.dirs.spec_file(&slug);
        let spec_path = spec_path.exists().then_some(spec_path);
        let invariants = stages::meaningful_invariants(&self.dirs);
        let cmd = stages::finding_fix_command(
            &self.cfg,
            &plan_path,
            &scope,
            &brief,
            spec_path.as_deref(),
            invariants.as_deref(),
        );
        let branch = self
            .findings
            .get(af.file_idx)
            .map(|lf| lf.file.branch.clone())
            .filter(|b| !b.is_empty())
            .unwrap_or_else(|| self.branch.clone());
        // Guards all passed: snapshot for the gate and persist the undo point.
        let _ = std::fs::create_dir_all(self.dirs.feature_dir(&slug));
        let _ = crate::undo::push(&self.dirs, &slug, stages::DocKind::Plan.label(), &text);
        self.fix_doc_before = text;
        let ctx = FixCtx {
            findings_path: self.findings[af.file_idx].path.clone(),
            pos: af.pos,
            title: f.title.clone(),
            slug,
            branch,
            plan_path,
            section,
            range,
        };
        Ok((cmd, ctx))
    }

    /// `F`: spawn ONE headless claude run that fixes the selected plan-review
    /// finding inside its plan section (apply + gate + auto-mark; `u` reverts).
    fn spawn_finding_fix(&mut self, tx: &mpsc::Sender<AppMsg>) {
        if self.tab != Tab::Findings {
            self.status_msg = Some("F fixes findings on the findings tab (2)".into());
            return;
        }
        let Some(run_cwd) = self.run_cwd() else {
            self.status_msg = Some(format!("branch '{}' has no checkout", self.branch));
            return;
        };
        let (cmd, ctx) = match self.prepare_finding_fix() {
            Ok(v) => v,
            Err(msg) => {
                self.status_msg = Some(msg);
                return;
            }
        };
        let title = self
            .state
            .features
            .get(&self.slug)
            .map(|f| f.title.clone())
            .unwrap_or_default();
        let req = RunRequest {
            agent: cmd.agent,
            argv: cmd.argv,
            env: cmd.env,
            stage: "plan-fix".into(),
            feature: title,
            branch: ctx.branch.clone(),
            redact: self.cfg.redaction,
            repro: None, // small scoped edit; skip provenance like doc-chat
            cwd: run_cwd,
            wrapper: stages::wrapper_argv(&self.cfg, cmd.mode),
        };
        let run_id = runner::new_run_id("plan-fix");
        if let Err(e) = runner::spawn_detached(&self.dirs, &req, &run_id) {
            self.status_msg = Some(format!("plan-fix failed to start: {e:#}"));
            return;
        }
        let label = match &ctx.section {
            Some(n) => format!("§{n}"),
            None => "the whole plan (step not located — undo only)".into(),
        };
        self.status_msg = Some(format!("fixing {label} via claude…"));
        self.last_fix = None; // the new run owns the top of the undo stack
        self.fix_ctx = Some(ctx);
        self.attach_fix_tail(run_id, req.agent, tx);
    }

    /// Follow a plan-fix run to completion. Events are not streamed to the
    /// UI, but the tail watches for the final `Completed` event and carries
    /// its result text home — that is where the ANSWERS block lives. The
    /// sender drops when tail_run returns, ending the watcher loop; the
    /// archive (incl. the result line) is fully flushed before that.
    fn attach_fix_tail(
        &mut self,
        run_id: String,
        agent: runner::AgentKind,
        tx: &mpsc::Sender<AppMsg>,
    ) {
        let dirs = self.dirs.clone();
        self.current_fix_run_id = Some(run_id.clone());
        let tx_done = tx.clone();
        self.fix_task = Some(tokio::spawn(async move {
            let (etx, mut erx) = mpsc::channel::<AgentEvent>(256);
            let watch = tokio::spawn(async move {
                let mut last: Option<String> = None;
                while let Some(ev) = erx.recv().await {
                    if let AgentEvent::Completed { result_text, .. } = ev {
                        last = result_text; // last Completed wins
                    }
                }
                last
            });
            let outcome = runner::tail_run(&dirs, agent, &run_id, etx).await;
            let result_text = watch.await.unwrap_or(None);
            let _ = tx_done
                .send(AppMsg::FixExited(Box::new(outcome), result_text))
                .await;
        }));
    }

    /// A plan fix finished: enforce the scope gate, auto-mark or revert.
    fn on_fix_exited(
        &mut self,
        outcome: Result<RunOutcome>,
        _result_text: Option<String>,
        tx: &mpsc::Sender<AppMsg>,
    ) {
        self.fix_task = None;
        self.current_fix_run_id = None;
        let Some(ctx) = self.fix_ctx.take() else {
            return;
        };
        self.finding_detail = false;
        let plan_label = stages::DocKind::Plan.label();
        let content = std::fs::read_to_string(&ctx.plan_path).unwrap_or_default();
        let changed = content != self.fix_doc_before;
        let label = match &ctx.section {
            Some(n) => format!("§{n}"),
            None => "plan".into(),
        };
        match outcome {
            Err(e) if changed => {
                let _ = crate::undo::undo(&self.dirs, &ctx.slug, plan_label, &ctx.plan_path);
                self.reload_artifacts();
                self.status_msg = Some(format!("plan-fix failed mid-edit; reverted ({e:#})"));
            }
            Err(e) => self.status_msg = Some(format!("plan-fix failed: {e:#}")),
            Ok(o) if !o.meta.ok => {
                if changed {
                    let _ = crate::undo::undo(&self.dirs, &ctx.slug, plan_label, &ctx.plan_path);
                    self.reload_artifacts();
                    self.status_msg = Some("plan-fix failed mid-edit; reverted".into());
                } else {
                    self.status_msg = Some(format!(
                        "plan-fix failed{}",
                        o.meta
                            .error
                            .as_deref()
                            .map(|e| format!(": {e}"))
                            .unwrap_or_default()
                    ));
                }
            }
            Ok(o) if !changed => {
                let cost = o.meta.total_cost_usd.unwrap_or(0.0);
                self.status_msg = Some(format!(
                    "plan-fix declined — no edit made (may need a broader change) · ${cost:.3}"
                ));
            }
            Ok(o) => {
                let cost = o.meta.total_cost_usd.unwrap_or(0.0);
                // Whole-doc scope (section None) has an all-covering range, so
                // the gate degenerates to always-confined there by design.
                match crate::spec::edits_confined(&self.fix_doc_before, &content, &ctx.range) {
                    None => {
                        let _ =
                            crate::undo::undo(&self.dirs, &ctx.slug, plan_label, &ctx.plan_path);
                        self.reload_artifacts();
                        self.status_msg = Some(format!(
                            "reverted: fix leaked outside {label}; finding stays pending"
                        ));
                        crate::notify::notify(
                            self.cfg.notifications,
                            "ritual: plan-fix reverted",
                            &format!("{}: {}", ctx.branch, ctx.title),
                        );
                    }
                    Some((added, removed)) => {
                        self.reload_artifacts();
                        // Re-find the findings file by PATH: reload shifted the
                        // indices, but the fix run can't have rewritten findings
                        // JSON (its tool lock only allows the plan file).
                        if let Some(i) = self
                            .findings
                            .iter()
                            .position(|lf| lf.path == ctx.findings_path)
                            && self.findings[i]
                                .file
                                .findings
                                .get(ctx.pos)
                                .is_some_and(|f| f.action != "fixed")
                        {
                            // set_action toggles same-value back to pending, so
                            // only apply when it isn't already fixed.
                            let _ = crate::findings::set_action(
                                &mut self.findings,
                                i,
                                ctx.pos,
                                "fixed",
                            );
                        }
                        self.clamp_selected_finding();
                        self.status_msg = Some(format!(
                            "✓ {label} rewritten (+{added}/−{removed}) · ${cost:.3} · u reverts"
                        ));
                        crate::notify::notify(
                            self.cfg.notifications,
                            "ritual: plan-fix done",
                            &format!("{}: {}", ctx.branch, ctx.title),
                        );
                        self.last_fix = Some(LastFix {
                            slug: ctx.slug.clone(),
                            plan_path: ctx.plan_path.clone(),
                            findings_path: ctx.findings_path.clone(),
                            pos: ctx.pos,
                        });
                    }
                }
            }
        }
        // A chat message may have queued while the fix held the doc.
        if !self.chat_running()
            && let Some(msg) = self.chat.as_mut().and_then(|c| c.pending.pop_front())
        {
            if let Some(chat) = self.chat.as_mut() {
                chat.transcript.push(ChatTurn::User(msg.clone()));
            }
            self.spawn_doc_chat(msg, tx);
        }
    }

    /// `u`: revert the last APPLIED claude plan fix and reopen its finding.
    fn doc_undo(&mut self) {
        if self.fix_running() || self.chat_running() {
            self.status_msg = Some("busy: wait for the running edit to finish".into());
            return;
        }
        let Some(lf) = self.last_fix.take() else {
            self.status_msg = Some("no plan fix to revert (chat has Ctrl+Z)".into());
            return;
        };
        match crate::undo::undo(
            &self.dirs,
            &lf.slug,
            stages::DocKind::Plan.label(),
            &lf.plan_path,
        ) {
            Ok(true) => {
                self.reload_artifacts();
                if let Some(i) = self
                    .findings
                    .iter()
                    .position(|x| x.path == lf.findings_path)
                    && self.findings[i]
                        .file
                        .findings
                        .get(lf.pos)
                        .is_some_and(|f| f.action == "fixed")
                {
                    // Same-value set_action toggles fixed -> pending.
                    let _ = crate::findings::set_action(&mut self.findings, i, lf.pos, "fixed");
                }
                self.status_msg = Some("plan fix reverted; finding pending again".into());
            }
            Ok(false) => self.status_msg = Some("nothing to undo".into()),
            Err(e) => self.status_msg = Some(format!("undo failed: {e:#}")),
        }
    }
}

/// Best-effort 1-based line in `plan` for a plan-review finding's free-text
/// `step`. Tries, in order: the whole step text, a leading headline like
/// "Step 2", and the ordered-list item ("2." / "2)") that plans number their
/// steps with. None when nothing matches — the caller opens the plan at the top.
fn locate_plan_step(plan: &str, step: &str) -> Option<u32> {
    let needle = step.trim().to_lowercase();
    if needle.is_empty() {
        return None;
    }
    // 1) Whole step text appears verbatim on a line.
    if let Some(n) = line_where(plan, |l| l.contains(&needle)) {
        return Some(n);
    }
    // 2) The step's headline: the text before the first '(' or '/'.
    let headline = needle.split(['(', '/']).next().unwrap_or("").trim();
    if headline.len() >= 3
        && headline != needle
        && let Some(n) = line_where(plan, |l| l.contains(headline))
    {
        return Some(n);
    }
    // 3) "Step 2" / "Steps 2-4" -> the "2." or "2)" ordered-list item that
    //    plan-mode plans number their steps with.
    if let Some(num) = step_number(&needle) {
        let dot = format!("{num}.");
        let paren = format!("{num})");
        if let Some(n) = line_where(plan, |l| {
            let t = l.trim_start();
            t.starts_with(&dot) || t.starts_with(&paren)
        }) {
            return Some(n);
        }
    }
    None
}

/// First 1-based line whose lowercased text satisfies `pred`.
fn line_where<F: Fn(&str) -> bool>(plan: &str, pred: F) -> Option<u32> {
    plan.lines()
        .position(|l| pred(&l.to_lowercase()))
        .map(|i| i as u32 + 1)
}

/// The step number in a plan_step: digits right after "step", else the first
/// digit run anywhere ("Step 2 (x)" -> 2, "Steps 2-4" -> 2).
fn step_number(needle: &str) -> Option<u32> {
    let tail = needle
        .find("step")
        .map(|i| &needle[i + 4..])
        .unwrap_or(needle);
    tail.trim_start_matches(|c: char| !c.is_ascii_digit())
        .chars()
        .take_while(char::is_ascii_digit)
        .collect::<String>()
        .parse()
        .ok()
}

/// The newest live run the TUI can resume. Chat runs ("spec-chat" etc.) have
/// stages that don't parse to a StageId and stay daemon-only; a newer live
/// chat run must never shadow an older pipeline run (follow chat runs with
/// `ritual attach` instead).
/// With worktree parallelism a second live run can't attach into this TUI,
/// but it must not be invisible either (chat runs count too: `ps` sees them).
fn other_live_runs_notice(dirs: &RitualDirs, resumed: Option<&str>) -> Option<String> {
    let others = runner::live_runs(dirs)
        .into_iter()
        .filter(|(id, _)| Some(id.as_str()) != resumed)
        .count();
    (others > 0).then(|| format!("{others} other live run(s): `ritual ps` / `ritual attach <id>`"))
}

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
/// awaited) before any terminal suspend; see term.rs contract.
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
    anyhow::ensure!(dirs.exists(), "no .ritual/ here; run `ritual init` first");
    let mut term = Term::enter()?;
    let (tx, mut rx) = mpsc::channel::<AppMsg>(512);

    let mut app = App::new(cfg, dirs).context("loading project state")?;
    crate::agents_status::spawn_probe(&app.cfg, tx.clone());

    // Finalize stages whose runs completed while nobody was watching, then
    // reattach to any run that is still alive.
    app.reconcile_stale_runs();
    let resumed = newest_resumable_run(&app.dirs);
    let resumed_id = resumed.as_ref().map(|(id, _)| id.clone());
    if let Some((run_id, status)) = resumed {
        app.resume_run(run_id, status, &tx);
    }
    if let Some(msg) = other_live_runs_notice(&app.dirs, resumed_id.as_deref()) {
        app.status_msg = Some(msg);
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
                app.running.is_some() || app.chat_running() || app.fix_running(),
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
    fn locate_plan_step_prefers_whole_then_headline_then_none() {
        let plan = "# Plan\n\n### Step 1 — scaffold\ndo thing\n\n### Step 2 — delete\nmore\n";
        // Whole-text substring match.
        assert_eq!(locate_plan_step(plan, "do thing"), Some(4));
        // Headline fallback: "Step 2" extracted from "Step 2 (delete via x)".
        assert_eq!(locate_plan_step(plan, "Step 2 (delete via x)"), Some(6));
        // Nothing matches, empty/whitespace.
        assert_eq!(locate_plan_step(plan, "nonexistent"), None);
        assert_eq!(locate_plan_step(plan, "   "), None);
    }

    #[test]
    fn locate_plan_step_maps_numbered_list_steps() {
        // The real plan-mode convention: a "## Steps" ordered list, not "Step N".
        let plan =
            "# Plan\n\n## Steps\n1. first thing\n2. enumerate by filename\n3. classify groups\n";
        // lines: 4 = "1. first", 5 = "2. enumerate", 6 = "3. classify".
        assert_eq!(locate_plan_step(plan, "Step 2 (enumerate)"), Some(5));
        assert_eq!(locate_plan_step(plan, "Steps 2-4 (range)"), Some(5));
        assert_eq!(locate_plan_step(plan, "Risks section / Step 3"), Some(6));
        // A step number with no matching list item -> None (open at the top).
        assert_eq!(locate_plan_step(plan, "Step 9"), None);
    }

    /// Seed one on-disk findings file and land on the findings tab.
    fn seed_findings(app: &mut App, json: &str) {
        std::fs::create_dir_all(app.dirs.findings_dir()).unwrap();
        std::fs::write(
            app.dirs
                .findings_dir()
                .join("20260713T000000Z-plan-review.json"),
            json,
        )
        .unwrap();
        app.reload_artifacts();
        app.tab = Tab::Findings;
    }

    #[test]
    fn enter_opens_detail_and_esc_closes() {
        let (_t, mut app, tx, _rx) = test_app();
        seed_findings(
            &mut app,
            r#"{"stage":"plan-review","findings":[
                {"id":1,"title":"boom","plan_step":"Step 2","severity":"major","verdict":"confirmed"}]}"#,
        );
        app.dispatch(Action::Confirm, &tx);
        assert!(app.finding_detail, "enter opens the overlay");
        app.on_input(
            Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
            &tx,
        );
        assert!(!app.finding_detail, "esc closes it");
        // q also closes the modal instead of quitting.
        app.finding_detail = true;
        app.on_input(
            Event::Key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE)),
            &tx,
        );
        assert!(!app.finding_detail);
        assert!(!app.quit);
    }

    #[test]
    fn detail_f_marks_fixed_and_closes() {
        let (_t, mut app, tx, _rx) = test_app();
        seed_findings(
            &mut app,
            r#"{"stage":"plan-review","findings":[
                {"id":1,"title":"boom","plan_step":"Step 2","severity":"major","verdict":"confirmed"}]}"#,
        );
        app.dispatch(Action::Confirm, &tx);
        assert!(app.finding_detail);
        app.on_input(
            Event::Key(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::NONE)),
            &tx,
        );
        assert!(
            !app.finding_detail,
            "acting on the finding closes the overlay"
        );
        let json = std::fs::read_to_string(
            app.dirs
                .findings_dir()
                .join("20260713T000000Z-plan-review.json"),
        )
        .unwrap();
        assert!(
            json.contains(r#""action": "fixed""#),
            "write-through: {json}"
        );
    }

    #[test]
    fn enter_with_no_findings_shows_status_not_overlay() {
        let (_t, mut app, tx, _rx) = test_app();
        app.tab = Tab::Findings;
        app.dispatch(Action::Confirm, &tx);
        assert!(!app.finding_detail);
        assert_eq!(app.status_msg.as_deref(), Some("no finding selected"));
    }

    /// Plan with two sections; "Step 2" maps into "## Steps" (lines 2..6).
    const FIX_PLAN: &str = "# Plan\n\n## Steps\n1. first\n2. second\n\n## Risks\nr1\n";

    /// Seed a plan + one plan-review finding pointing at Step 2; findings tab.
    fn seed_fixable(app: &mut App) {
        let plan = app.dirs.plan_file(&app.slug);
        std::fs::create_dir_all(plan.parent().unwrap()).unwrap();
        std::fs::write(&plan, FIX_PLAN).unwrap();
        seed_findings(
            app,
            r#"{"stage":"plan-review","findings":[
                {"id":1,"title":"step 2 is wrong","plan_step":"Step 2 (x)","severity":"major",
                 "scenario":"boom","verdict":"confirmed"}]}"#,
        );
    }

    fn fix_outcome(ok: bool, cost: f64) -> Result<RunOutcome> {
        Ok(RunOutcome {
            meta: RunMeta {
                ok,
                total_cost_usd: Some(cost),
                ..Default::default()
            },
            archive: std::path::PathBuf::new(),
        })
    }

    #[test]
    fn prepare_fix_refuses_code_findings_busy_states_and_missing_step() {
        let (_t, mut app, _tx, _rx) = test_app();
        seed_findings(
            &mut app,
            r#"{"stage":"dual-review","findings":[
                {"id":1,"title":"code bug","file":"src/a.rs","line":3,"severity":"major","verdict":"confirmed"},
                {"id":2,"title":"no location","severity":"minor","verdict":"confirmed"}]}"#,
        );
        // Code finding -> manual flow only.
        let err = app.prepare_finding_fix().unwrap_err();
        assert!(err.contains("plan findings only"), "{err}");
        // No plan_step at all.
        app.selected_finding = 1;
        let err = app.prepare_finding_fix().unwrap_err();
        assert!(err.contains("no plan step"), "{err}");

        // Busy: a fix already running.
        let (_t2, mut app2, _tx2, _rx2) = test_app();
        seed_fixable(&mut app2);
        let (_cmd, ctx) = app2.prepare_finding_fix().unwrap();
        app2.fix_ctx = Some(ctx);
        let err = app2.prepare_finding_fix().unwrap_err();
        assert!(err.contains("already running"), "{err}");
    }

    #[test]
    fn prepare_fix_resolves_section_and_pushes_undo() {
        let (_t, mut app, _tx, _rx) = test_app();
        seed_fixable(&mut app);
        let (cmd, ctx) = app.prepare_finding_fix().unwrap();
        assert_eq!(ctx.section.as_deref(), Some("Steps"));
        assert_eq!(ctx.range, 2..6);
        assert_eq!(ctx.pos, 0);
        assert_eq!(app.fix_doc_before, FIX_PLAN);
        assert_eq!(crate::undo::depth(&app.dirs, &ctx.slug, "plan"), 1);
        let prompt = cmd.argv.iter().find(|a| a.starts_with("/spec")).unwrap();
        assert!(prompt.contains(r#"SCOPE: section "Steps""#));
        assert!(prompt.contains("title: step 2 is wrong"));
        let i = cmd
            .argv
            .iter()
            .position(|a| a == "--max-budget-usd")
            .unwrap();
        assert_eq!(cmd.argv[i + 1], "1");
    }

    #[test]
    fn prepare_fix_falls_back_to_whole_when_step_unlocatable() {
        let (_t, mut app, _tx, _rx) = test_app();
        seed_fixable(&mut app);
        // Point the finding at a step that exists nowhere in the plan.
        let path = app
            .dirs
            .findings_dir()
            .join("20260713T000000Z-plan-review.json");
        std::fs::write(
            &path,
            r#"{"stage":"plan-review","findings":[
                {"id":1,"title":"lost","plan_step":"Step 99 (nowhere)","severity":"major","verdict":"confirmed"}]}"#,
        )
        .unwrap();
        app.reload_artifacts();
        let (cmd, ctx) = app.prepare_finding_fix().unwrap();
        assert_eq!(ctx.section, None);
        assert_eq!(ctx.range, 0..FIX_PLAN.lines().count());
        let prompt = cmd.argv.iter().find(|a| a.starts_with("/spec")).unwrap();
        assert!(prompt.contains("SCOPE: whole"));
    }

    #[test]
    fn on_fix_exited_confined_marks_fixed_and_notes_counts() {
        let (_t, mut app, tx, _rx) = test_app();
        seed_fixable(&mut app);
        app.finding_detail = true;
        let (_cmd, ctx) = app.prepare_finding_fix().unwrap();
        let plan_path = ctx.plan_path.clone();
        app.fix_ctx = Some(ctx);
        // The "agent" edits inside the section.
        std::fs::write(
            &plan_path,
            FIX_PLAN.replace("2. second", "2. second, fixed"),
        )
        .unwrap();
        app.on_fix_exited(fix_outcome(true, 0.05), None, &tx);
        let msg = app.status_msg.clone().unwrap_or_default();
        assert!(msg.contains("✓ §Steps rewritten (+1/−1)"), "{msg}");
        assert!(msg.contains("u reverts"), "{msg}");
        assert!(!app.finding_detail, "overlay closes with the verdict");
        assert!(app.fix_revertable());
        assert!(!app.fix_running());
        // Write-through: the finding is fixed on disk.
        let json = std::fs::read_to_string(
            app.dirs
                .findings_dir()
                .join("20260713T000000Z-plan-review.json"),
        )
        .unwrap();
        assert!(json.contains(r#""action": "fixed""#), "{json}");
    }

    #[test]
    fn on_fix_exited_leak_reverts_and_keeps_pending() {
        let (_t, mut app, tx, _rx) = test_app();
        seed_fixable(&mut app);
        let (_cmd, ctx) = app.prepare_finding_fix().unwrap();
        let plan_path = ctx.plan_path.clone();
        app.fix_ctx = Some(ctx);
        // The "agent" edits OUTSIDE the section (Risks body).
        std::fs::write(&plan_path, FIX_PLAN.replace("r1", "r1 sneaky")).unwrap();
        app.on_fix_exited(fix_outcome(true, 0.05), None, &tx);
        let msg = app.status_msg.clone().unwrap_or_default();
        assert!(msg.contains("leaked outside §Steps"), "{msg}");
        // Mechanically reverted; finding still pending; nothing revertable.
        assert_eq!(std::fs::read_to_string(&plan_path).unwrap(), FIX_PLAN);
        assert!(!app.fix_revertable());
        let json = std::fs::read_to_string(
            app.dirs
                .findings_dir()
                .join("20260713T000000Z-plan-review.json"),
        )
        .unwrap();
        assert!(!json.contains(r#""action": "fixed""#), "{json}");
    }

    #[test]
    fn on_fix_exited_unchanged_is_declined_and_failure_reverts() {
        let (_t, mut app, tx, _rx) = test_app();
        seed_fixable(&mut app);
        let (_cmd, ctx) = app.prepare_finding_fix().unwrap();
        let plan_path = ctx.plan_path.clone();
        app.fix_ctx = Some(ctx);
        // No edit at all -> declined, finding stays pending.
        app.on_fix_exited(fix_outcome(true, 0.02), None, &tx);
        let msg = app.status_msg.clone().unwrap_or_default();
        assert!(msg.contains("declined"), "{msg}");
        assert!(!app.fix_revertable());

        // A failed run that DID edit gets rolled back.
        let (_cmd, ctx) = app.prepare_finding_fix().unwrap();
        app.fix_ctx = Some(ctx);
        std::fs::write(&plan_path, "half-written garbage\n").unwrap();
        app.on_fix_exited(fix_outcome(false, 0.01), None, &tx);
        let msg = app.status_msg.clone().unwrap_or_default();
        assert!(msg.contains("reverted"), "{msg}");
        assert_eq!(std::fs::read_to_string(&plan_path).unwrap(), FIX_PLAN);
    }

    #[test]
    fn doc_undo_restores_plan_and_reopens_finding() {
        let (_t, mut app, tx, _rx) = test_app();
        seed_fixable(&mut app);
        let (_cmd, ctx) = app.prepare_finding_fix().unwrap();
        let plan_path = ctx.plan_path.clone();
        app.fix_ctx = Some(ctx);
        std::fs::write(&plan_path, FIX_PLAN.replace("2. second", "2. improved")).unwrap();
        app.on_fix_exited(fix_outcome(true, 0.05), None, &tx);
        assert!(app.fix_revertable());

        app.doc_undo();
        assert_eq!(std::fs::read_to_string(&plan_path).unwrap(), FIX_PLAN);
        assert!(!app.fix_revertable());
        let json = std::fs::read_to_string(
            app.dirs
                .findings_dir()
                .join("20260713T000000Z-plan-review.json"),
        )
        .unwrap();
        assert!(json.contains(r#""action": "pending""#), "{json}");
        // Nothing left to revert.
        app.doc_undo();
        assert!(
            app.status_msg
                .as_deref()
                .is_some_and(|m| m.contains("no plan fix to revert"))
        );
    }

    #[test]
    fn chat_send_and_ctrl_z_are_held_while_fix_runs() {
        let (_t, mut app, tx, _rx) = test_app();
        seed_fixable(&mut app);
        let (_cmd, ctx) = app.prepare_finding_fix().unwrap();
        app.fix_ctx = Some(ctx);
        // Open a chat and try to send: the message queues instead of spawning.
        app.chat = Some(ChatState {
            transcript: Vec::new(),
            input: Vec::new(),
            cursor: 0,
            targets: app.build_chat_targets(),
            target_idx: 0,
            scroll: 0,
            in_flight: false,
            pending: std::collections::VecDeque::new(),
        });
        app.spawn_doc_chat("tighten step 1".into(), &tx);
        let chat = app.chat.as_ref().unwrap();
        assert!(
            !chat.in_flight,
            "no run spawned while the fix holds the doc"
        );
        assert_eq!(chat.pending.len(), 1);
        assert!(
            chat.transcript
                .iter()
                .any(|t| matches!(t, ChatTurn::System(m) if m.contains("plan fix is running")))
        );
        // Ctrl+Z is refused too.
        app.chat_undo_redo(true);
        assert!(app.chat.as_ref().unwrap().transcript.iter().any(
            |t| matches!(t, ChatTurn::System(m) if m.contains("cannot undo while an edit is in flight"))
        ));
    }

    #[test]
    fn finding_open_target_routes_plan_findings_to_the_plan_doc() {
        let (_tmp, mut app, _tx, _rx) = test_app();
        // A plan.md for the in-view feature, with the step on line 5.
        let plan = app.dirs.plan_file(&app.slug);
        std::fs::create_dir_all(plan.parent().unwrap()).unwrap();
        std::fs::write(
            &plan,
            "# Plan\n\n## Phases\n\n### Step 2 — delete via load_all\nbody\n",
        )
        .unwrap();

        // Branch left empty -> plan_path_for falls back to the in-view slug.
        let file: crate::findings::FindingsFile = serde_json::from_str(
            r#"{"stage":"plan-review","branch":"",
                "findings":[
                  {"title":"boom","plan_step":"Step 2 (delete via load_all)","verdict":"confirmed"},
                  {"title":"bug","file":"src/a.rs","line":7,"verdict":"confirmed"},
                  {"title":"no loc","verdict":"confirmed"}
                ]}"#,
        )
        .unwrap();
        app.findings = vec![crate::findings::LoadedFindings {
            path: app.dirs.findings_dir().join("x-plan-review.json"),
            file,
        }];

        let ags = app.visible_findings();
        let by = |t: &str| ags.iter().find(|a| a.finding.title == t).unwrap().clone();

        // Plan-review finding -> the plan doc, at the located step line.
        let (p, line, label) = app.finding_open_target(&by("boom")).unwrap();
        assert_eq!(p, plan);
        assert_eq!(label, "plan.md");
        assert_eq!(line, Some(5));

        // Code finding -> its own file:line, unchanged.
        let (p, line, label) = app.finding_open_target(&by("bug")).unwrap();
        assert!(p.ends_with("src/a.rs"));
        assert_eq!(line, Some(7));
        assert_eq!(label, "src/a.rs");

        // Neither file nor plan_step -> still an error (the old message).
        assert!(app.finding_open_target(&by("no loc")).is_err());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn open_chat_reattaches_to_a_live_chat_run() {
        let (_t, mut app, tx, _rx) = test_app();
        let runs = app.dirs.runs_dir();
        std::fs::create_dir_all(&runs).unwrap();
        let pid = std::process::id(); // our own pid: definitely alive
        std::fs::write(
            runs.join("20260712T000009Z-9-9-spec-chat.status"),
            format!(
                r#"{{"pid":{pid},"stage":"spec-chat","branch":"{}"}}"#,
                app.branch
            ),
        )
        .unwrap();
        std::fs::write(runs.join("20260712T000009Z-9-9-spec-chat.jsonl"), "").unwrap();

        app.open_chat(&tx);
        {
            let chat = app.chat.as_ref().unwrap();
            assert!(chat.in_flight, "reattached chat is in flight");
            assert!(
                matches!(&chat.transcript[0], ChatTurn::System(s) if s.contains("reattached")),
                "transcript starts with the reattach note"
            );
        }
        assert!(
            app.current_chat_run_id
                .as_deref()
                .unwrap()
                .ends_with("spec-chat")
        );
        assert!(app.chat_task.is_some(), "tail task follows the daemon");
        app.chat_task.take().unwrap().abort();
        app.current_chat_run_id = None;

        // No live chat run -> a plain fresh chat.
        std::fs::remove_file(runs.join("20260712T000009Z-9-9-spec-chat.status")).unwrap();
        app.open_chat(&tx);
        assert!(!app.chat.as_ref().unwrap().in_flight);
        assert!(app.chat.as_ref().unwrap().transcript.is_empty());
    }

    #[test]
    fn other_live_runs_surface_a_notice() {
        let tmp = tempfile::tempdir().unwrap();
        let runs = tmp.path().join(".ritual/runs");
        std::fs::create_dir_all(&runs).unwrap();
        let dirs = RitualDirs::new(tmp.path());
        assert!(other_live_runs_notice(&dirs, None).is_none());

        let pid = std::process::id();
        for name in [
            "20260712T000001Z-1-1-plan-review",
            "20260712T000002Z-1-2-dual-review",
        ] {
            std::fs::write(
                runs.join(format!("{name}.status")),
                format!(r#"{{"pid":{pid},"stage":"x","branch":"other"}}"#),
            )
            .unwrap();
        }
        // The resumed run is excluded; the other one is announced.
        let msg = other_live_runs_notice(&dirs, Some("20260712T000002Z-1-2-dual-review")).unwrap();
        assert!(msg.contains("1 other live run(s)"), "{msg}");
        assert!(msg.contains("ritual ps"));
        // Nothing resumed -> both count.
        assert!(
            other_live_runs_notice(&dirs, None)
                .unwrap()
                .contains("2 other")
        );
    }

    #[test]
    fn cancel_run_kills_the_daemon_and_fails_the_stage() {
        use std::os::unix::process::CommandExt;
        let (_t, mut app, _tx, _rx) = test_app();
        std::fs::create_dir_all(app.dirs.runs_dir()).unwrap();

        // No active run: a notice, not a panic.
        app.cancel_run();
        assert!(app.status_msg.as_deref().unwrap().contains("no active run"));

        // A live own-group child stands in for the detached daemon.
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .process_group(0)
            .spawn()
            .unwrap();
        std::fs::write(
            app.dirs.runs_dir().join("r-cancel.status"),
            format!(
                r#"{{"pid":{},"stage":"dual-review","branch":"main"}}"#,
                child.id()
            ),
        )
        .unwrap();
        app.current_run_id = Some("r-cancel".into());
        app.running = Some(StageId::DualReview);

        app.cancel_run();
        assert!(!child.wait().unwrap().success(), "SIGTERM delivered");
        assert!(app.running.is_none());
        assert!(app.current_run_id.is_none());
        assert_eq!(
            app.state
                .features
                .get(&app.slug)
                .unwrap()
                .stage(StageId::DualReview)
                .status,
            StageStatus::Failed
        );
        assert!(app.status_msg.as_deref().unwrap().contains("cancelled"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn on_chat_exited_matrix_and_queue_drain() {
        let (_t, mut app, tx, _rx) = test_app();
        app.open_chat(&tx);
        let spec = app.dirs.spec_file(&app.slug);
        let ok_outcome = || {
            Ok(RunOutcome {
                meta: crate::history::RunMeta {
                    ok: true,
                    total_cost_usd: Some(0.02),
                    ..Default::default()
                },
                archive: std::path::PathBuf::new(),
            })
        };

        // ok + changed doc -> spec marked done, "updated" note.
        app.doc_before = std::fs::read_to_string(&spec).unwrap_or_default();
        std::fs::write(&spec, "# Feature: x\n\n## Goal\nreal content now\n").unwrap();
        app.chat.as_mut().unwrap().in_flight = true;
        app.current_chat_run_id = Some("r1".into());
        app.on_chat_exited(ok_outcome(), &tx);
        assert_eq!(
            app.state
                .features
                .get(&app.slug)
                .unwrap()
                .stage(StageId::Spec)
                .status,
            StageStatus::Done
        );
        assert!(matches!(
            app.chat.as_ref().unwrap().transcript.last(),
            Some(ChatTurn::System(n)) if n.contains("updated")
        ));

        // ok + unchanged doc -> "no change" note, stage stays done.
        app.doc_before = std::fs::read_to_string(&spec).unwrap();
        app.chat.as_mut().unwrap().in_flight = true;
        app.current_chat_run_id = Some("r2".into());
        app.on_chat_exited(ok_outcome(), &tx);
        assert!(matches!(
            app.chat.as_ref().unwrap().transcript.last(),
            Some(ChatTurn::System(n)) if n.contains("no change")
        ));

        // Err -> failure note, in_flight reset.
        app.chat.as_mut().unwrap().in_flight = true;
        app.on_chat_exited(Err(anyhow::anyhow!("agent exploded")), &tx);
        let chat = app.chat.as_ref().unwrap();
        assert!(!chat.in_flight);
        assert!(matches!(
            chat.transcript.last(),
            Some(ChatTurn::System(n)) if n.contains("chat failed")
        ));

        // Queue drain: a pending message becomes the next User turn in flight.
        app.chat
            .as_mut()
            .unwrap()
            .pending
            .push_back("next msg".into());
        app.chat.as_mut().unwrap().in_flight = true;
        app.on_chat_exited(ok_outcome(), &tx);
        let chat = app.chat.as_ref().unwrap();
        assert!(
            chat.transcript
                .iter()
                .any(|t| matches!(t, ChatTurn::User(m) if m == "next msg")),
            "queued message drained as a user turn"
        );
        assert!(chat.in_flight, "drained message is immediately in flight");
        assert!(chat.pending.is_empty());
        if let Some(task) = app.chat_task.take() {
            task.abort();
        }

        // Chat closed mid-run: completion must not panic.
        app.chat = None;
        app.on_chat_exited(ok_outcome(), &tx);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn resume_run_edges() {
        let (_t, mut app, tx, _rx) = test_app();
        std::fs::create_dir_all(app.dirs.runs_dir()).unwrap();

        // Unparseable stage (a chat run) is never resumed into the pipeline.
        let chat_status = runner::RunStatus {
            pid: std::process::id(),
            stage: "spec-chat".into(),
            branch: "main".into(),
        };
        app.resume_run("r-chat".into(), chat_status, &tx);
        assert!(app.running.is_none());

        // Missing request.json falls back to the Claude agent and reattaches.
        let status = runner::RunStatus {
            pid: std::process::id(),
            stage: "plan-review".into(),
            branch: "main".into(),
        };
        app.resume_run("r-noreq".into(), status, &tx);
        assert_eq!(app.running, Some(StageId::PlanReview));
        assert_eq!(app.current_run_id.as_deref(), Some("r-noreq"));
        assert!(app.status_msg.as_deref().unwrap().contains("reattached"));
        if let Some(task) = app.run_task.take() {
            task.abort();
        }
    }

    #[test]
    fn reconcile_finalizes_every_stale_shape() {
        let (_t, mut app, _tx, _rx) = test_app();
        let runs = app.dirs.runs_dir();
        std::fs::create_dir_all(&runs).unwrap();
        // Finished-ok (unwatched), finished-failed, vanished, and no-run.
        std::fs::write(
            runs.join("r-ok.meta.json"),
            r#"{"run_id":"r-ok","ok":true}"#,
        )
        .unwrap();
        std::fs::write(
            runs.join("r-bad.meta.json"),
            r#"{"run_id":"r-bad","ok":false}"#,
        )
        .unwrap();
        // A still-live run must be left alone for resurrection.
        std::fs::write(
            runs.join("r-live.status"),
            format!(
                r#"{{"pid":{},"stage":"spec","branch":"detached"}}"#,
                std::process::id()
            ),
        )
        .unwrap();
        {
            let branch = app.branch.clone();
            let f = app.state.feature_for_branch_mut(&branch);
            let mut set = |id: StageId, runs: Vec<String>| {
                let e = f.stages.entry(id).or_default();
                e.status = StageStatus::Running;
                e.runs = runs;
            };
            set(StageId::PlanReview, vec!["r-ok".into()]);
            set(StageId::DualReview, vec!["r-bad".into()]);
            set(StageId::TestsRed, vec!["r-gone".into()]);
            set(StageId::Implement, vec![]);
            set(StageId::Spec, vec!["r-live".into()]);
        }

        app.reconcile_stale_runs();
        let f = app.state.features.get(&app.slug).unwrap();
        assert_eq!(
            f.stage(StageId::PlanReview).status,
            StageStatus::NeedsAttention
        );
        assert_eq!(f.stage(StageId::DualReview).status, StageStatus::Failed);
        assert_eq!(f.stage(StageId::TestsRed).status, StageStatus::Failed);
        assert_eq!(
            f.stage(StageId::Implement).status,
            StageStatus::NeedsAttention
        );
        assert_eq!(f.stage(StageId::Spec).status, StageStatus::Running);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn run_custom_expands_every_template_placeholder() {
        let (_t, mut app, tx, _rx) = test_app();
        std::fs::create_dir_all(app.dirs.findings_dir()).unwrap();
        std::fs::write(
            app.dirs
                .findings_dir()
                .join("20260712T000000Z-dual-review.json"),
            r#"{"stage":"dual-review","findings":[
                {"title":"t","file":"src/a.rs","line":42,"severity":"major",
                 "verdict":"confirmed","action":"pending"}]}"#,
        )
        .unwrap();
        app.reload_artifacts();
        app.current_run_id = Some("r-777".into());
        app.cfg.commands = vec![(
            "dump".into(),
            "printf '%s|%s|%s|%s' '{{branch}}' '{{finding.file}}' '{{finding.line}}' '{{run_id}}' > custom-out.txt".into(),
        )];

        app.run_custom(0, &tx);
        let out = app.dirs.work_root.join("custom-out.txt");
        for _ in 0..100 {
            if out.exists() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        let text = std::fs::read_to_string(&out).expect("custom command ran");
        assert_eq!(text, format!("{}|src/a.rs|42|r-777", app.branch));
    }

    #[test]
    fn takeover_notices_when_nothing_is_resumable() {
        let (_t, mut app, _tx, _rx) = test_app();
        // No recorded runs for the selected stage.
        app.takeover();
        assert!(
            app.status_msg
                .as_deref()
                .unwrap()
                .contains("no recorded runs")
        );
        // A recorded run without a session id.
        let branch = app.branch.clone();
        let f = app.state.feature_for_branch_mut(&branch);
        f.stages.entry(StageId::Spec).or_default().runs = vec!["r-nosession".into()];
        app.selected = 0; // spec
        app.takeover();
        assert!(
            app.status_msg
                .as_deref()
                .unwrap()
                .contains("recorded no session id")
        );
        assert!(app.pending_attached.is_none());
    }

    #[test]
    fn dismissing_the_last_visible_finding_clamps_selection() {
        let (_t, mut app, _tx, _rx) = test_app();
        std::fs::create_dir_all(app.dirs.findings_dir()).unwrap();
        std::fs::write(
            app.dirs
                .findings_dir()
                .join("20260712T000000Z-dual-review.json"),
            r#"{"stage":"dual-review","findings":[
                {"title":"first","severity":"critical","verdict":"confirmed"},
                {"title":"second","severity":"minor","verdict":"confirmed"}]}"#,
        )
        .unwrap();
        app.reload_artifacts();
        app.tab = Tab::Findings;

        app.selected_finding = 1;
        app.finding_set_action("dismissed"); // the LAST visible one
        assert_eq!(app.selected_finding, 0, "selection clamped down");
        app.finding_set_action("dismissed"); // now the list is empty
        assert_eq!(app.selected_finding, 0);
        // Toggling resolved back on keeps the selection in range.
        app.show_resolved = true;
        app.clamp_selected_finding();
        assert!(app.selected_finding < 2);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn chat_targets_grow_plan_sections_after_the_draft_lands() {
        let (_t, mut app, tx, _rx) = test_app();
        app.open_chat(&tx);
        // Before: the plan target exists but is missing (draft-from-spec).
        let plan_idx = {
            let chat = app.chat.as_ref().unwrap();
            let idx = chat
                .targets
                .iter()
                .position(|t| t.doc == stages::DocKind::Plan)
                .expect("plan offered even when missing");
            assert!(chat.targets[idx].missing);
            assert!(
                !chat
                    .targets
                    .iter()
                    .any(|t| t.doc == stages::DocKind::Plan && t.section.is_some()),
                "no plan sections yet"
            );
            idx
        };
        app.chat.as_mut().unwrap().target_idx = plan_idx;

        // The "edit" drafts the plan; completion rebuilds the target list.
        app.doc_before = String::new();
        std::fs::write(
            app.dirs.plan_file(&app.slug),
            "# Plan\n\n## Steps\n1. do it\n\n## Risks\nnone\n",
        )
        .unwrap();
        app.chat.as_mut().unwrap().in_flight = true;
        app.on_chat_exited(
            Ok(RunOutcome {
                meta: crate::history::RunMeta {
                    ok: true,
                    ..Default::default()
                },
                archive: std::path::PathBuf::new(),
            }),
            &tx,
        );
        let chat = app.chat.as_ref().unwrap();
        assert!(
            chat.targets
                .iter()
                .any(|t| t.doc == stages::DocKind::Plan && t.section.as_deref() == Some("Steps")),
            "plan sections appear after the draft: {:?}",
            chat.targets.iter().map(|t| t.label()).collect::<Vec<_>>()
        );
        assert_eq!(
            app.state
                .features
                .get(&app.slug)
                .unwrap()
                .stage(StageId::Plan)
                .status,
            StageStatus::Done
        );
    }

    #[test]
    fn slash_filter_narrows_findings_clamps_and_acts_on_the_visible_row() {
        let (_t, mut app, tx, _rx) = test_app();
        std::fs::create_dir_all(app.dirs.findings_dir()).unwrap();
        std::fs::write(
            app.dirs
                .findings_dir()
                .join("20260712T000000Z-dual-review.json"),
            r#"{"stage":"dual-review","findings":[
                {"id":1,"title":"race in state save","file":"src/state.rs","line":9,"severity":"critical","verdict":"confirmed"},
                {"id":2,"title":"unbuffered write","file":"src/io.rs","line":3,"severity":"major","verdict":"confirmed"},
                {"id":3,"title":"another race window","file":"src/run.rs","line":7,"severity":"minor","verdict":"confirmed"}]}"#,
        )
        .unwrap();
        app.reload_artifacts();
        app.tab = Tab::Findings;

        // `/` opens editing; typing narrows to the two "race" findings.
        app.dispatch(Action::Filter, &tx);
        assert!(app.filter_editing);
        for c in "race".chars() {
            app.filter_input(KeyCode::Char(c));
        }
        assert_eq!(app.visible_findings().len(), 2);
        assert!(app.filter_active());

        // Enter keeps the filter and returns to navigation.
        app.filter_input(KeyCode::Enter);
        assert!(!app.filter_editing);
        assert!(app.filter_active());

        // Selecting past the filtered length was clamped, and f/d act on the
        // VISIBLE row's real finding, not the underlying full-list index.
        app.selected_finding = 5;
        app.clamp_selected_finding();
        assert_eq!(app.selected_finding, 1); // 2 visible → max index 1
        app.selected_finding = 1;
        app.finding_set_action("dismissed");
        // "another race window" (2nd visible) is now dismissed in the file.
        let dismissed = app
            .findings
            .iter()
            .flat_map(|l| &l.file.findings)
            .find(|f| f.title == "another race window")
            .unwrap();
        assert_eq!(dismissed.action, "dismissed");

        // Esc clears the filter; leaving the tab also clears it.
        app.filter_input(KeyCode::Char('x'));
        app.filter_input(KeyCode::Esc);
        assert!(!app.filter_active());
        assert_eq!(app.filter, "");

        // A history filter matches stage/agent/run_id and drops on tab switch.
        app.metas = vec![
            crate::history::RunMeta {
                run_id: "r1".into(),
                stage: "plan-review".into(),
                agent: "claude".into(),
                ..Default::default()
            },
            crate::history::RunMeta {
                run_id: "r2".into(),
                stage: "dual-review".into(),
                agent: "codex".into(),
                ..Default::default()
            },
        ];
        app.tab = Tab::History;
        app.dispatch(Action::Filter, &tx);
        for c in "dual".chars() {
            app.filter_input(KeyCode::Char(c));
        }
        assert_eq!(app.visible_metas().len(), 1);
        app.dispatch(Action::TabLive, &tx);
        assert!(!app.filter_active(), "filter dropped on tab switch");
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
        app.open_chat(&tx);
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
        app.open_chat(&tx);
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
        app.open_chat(&tx);
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
    fn chat_undo_walks_the_stack_and_alt_z_redoes() {
        let (_t, mut app, tx, _rx) = test_app();
        app.open_chat(&tx);
        let spec = app.dirs.spec_file(&app.slug);
        // Two edit cycles: each pushes the pre-edit state (what spawn_doc_chat
        // does), then "Claude" writes new content.
        std::fs::write(&spec, "V0\n").unwrap();
        crate::undo::push(&app.dirs, &app.slug, "spec", "V0\n").unwrap();
        std::fs::write(&spec, "V1\n").unwrap();
        crate::undo::push(&app.dirs, &app.slug, "spec", "V1\n").unwrap();
        std::fs::write(&spec, "V2\n").unwrap();

        let ctrl_z = KeyEvent::new(KeyCode::Char('z'), KeyModifiers::CONTROL);
        let alt_z = KeyEvent::new(KeyCode::Char('z'), KeyModifiers::ALT);
        app.chat_input(ctrl_z, &tx);
        assert_eq!(std::fs::read_to_string(&spec).unwrap(), "V1\n");
        app.chat_input(ctrl_z, &tx);
        assert_eq!(std::fs::read_to_string(&spec).unwrap(), "V0\n");
        // Alt+Z walks forward again.
        app.chat_input(alt_z, &tx);
        assert_eq!(std::fs::read_to_string(&spec).unwrap(), "V1\n");
        app.chat_input(alt_z, &tx);
        assert_eq!(std::fs::read_to_string(&spec).unwrap(), "V2\n");
        // Blocked while in flight.
        app.chat.as_mut().unwrap().in_flight = true;
        app.chat_input(ctrl_z, &tx);
        assert_eq!(std::fs::read_to_string(&spec).unwrap(), "V2\n");
        assert!(matches!(
            app.chat.as_ref().unwrap().transcript.last(),
            Some(ChatTurn::System(n)) if n.contains("in flight")
        ));
    }

    #[test]
    fn chat_enter_while_in_flight_queues_with_cap() {
        let (_t, mut app, tx, _rx) = test_app();
        app.open_chat(&tx);
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
        app.open_chat(&tx);
        // Nothing in flight: informational note only.
        app.chat_input(
            KeyEvent::new(KeyCode::Char('x'), KeyModifiers::CONTROL),
            &tx,
        );
        assert!(matches!(
            app.chat.as_ref().unwrap().transcript.last(),
            Some(ChatTurn::System(n)) if n.contains("nothing in flight")
        ));
        // In flight (no real daemon, so kill_run on a missing id is a no-op).
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
        app.open_chat(&tx);
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
    fn pasted_multiline_text_never_submits_mid_paste() {
        let (_t, mut app, tx, _rx) = test_app();
        app.open_chat(&tx);
        app.on_input(Event::Paste("line one\nline two\nline three".into()), &tx);
        let chat = app.chat.as_ref().unwrap();
        // The whole blob is one input value with literal newlines...
        assert_eq!(
            chat.input.iter().collect::<String>(),
            "line one\nline two\nline three"
        );
        // ...and nothing was submitted or queued.
        assert!(!chat.in_flight);
        assert!(chat.pending.is_empty());
        assert_eq!(
            chat.cursor,
            "line one\nline two\nline three".chars().count()
        );

        // A paste at a mid-string caret splices in place.
        {
            let chat = app.chat.as_mut().unwrap();
            chat.input = "ab".chars().collect();
            chat.cursor = 1;
        }
        app.on_input(Event::Paste("XY".into()), &tx);
        assert_eq!(
            app.chat.as_ref().unwrap().input.iter().collect::<String>(),
            "aXYb"
        );

        // Paste into the palette flattens to a single line (a filter).
        app.chat = None;
        app.palette = Some(PaletteState::default());
        app.on_input(Event::Paste("run\tplan\nreview".into()), &tx);
        assert_eq!(app.palette.as_ref().unwrap().input, "runplanreview");
    }

    #[test]
    fn chat_submit_records_clears_and_guards() {
        let (_t, mut app, tx, _rx) = test_app();
        app.open_chat(&tx);
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
        app.open_chat(&tx);
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
        let (_t, mut app, tx, _rx) = test_app();
        app.open_chat(&tx);
        // spec.md was created from the template.
        assert!(app.dirs.spec_file(&app.slug).exists());
        let chat = app.chat.as_ref().unwrap();
        // First target is the whole spec; a Behavior section target exists.
        assert!(matches!(chat.targets[0].doc, stages::DocKind::Spec));
        assert!(chat.targets[0].section.is_none());
        assert!(
            chat.targets
                .iter()
                .any(|t| t.section.as_deref() == Some("Behavior (the contract: WHAT, not HOW)"))
        );
        // The MISSING plan is still a target, labeled as a draft.
        let plan_target = chat
            .targets
            .iter()
            .find(|t| t.doc == stages::DocKind::Plan)
            .expect("plan target offered even when plan.md is absent");
        assert!(plan_target.missing);
        assert_eq!(plan_target.label(), "plan (draft from spec)");

        // Once the plan exists, the label and sections normalize.
        std::fs::write(
            app.dirs.plan_file(&app.slug),
            "# Plan\n\n## Steps\n1. do it\n",
        )
        .unwrap();
        let targets = app.build_chat_targets();
        let plan_target = targets
            .iter()
            .find(|t| t.doc == stages::DocKind::Plan && t.section.is_none())
            .unwrap();
        assert!(!plan_target.missing);
        assert_eq!(plan_target.label(), "plan · whole");
        assert!(
            targets
                .iter()
                .any(|t| t.doc == stages::DocKind::Plan && t.section.as_deref() == Some("Steps"))
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
    fn retry_with_model_appears_only_for_failed_stages_and_sets_override() {
        let (_t, mut app, tx, _rx) = test_app();
        app.cfg.retry_models = vec!["claude-sonnet-5".into()];
        app.palette = Some(PaletteState {
            input: "retry".into(),
            selected: 0,
        });

        // Healthy pipeline -> no retry entries.
        assert!(
            !app.palette_filtered()
                .iter()
                .any(|(l, _)| l.starts_with("retry")),
            "no retry offers while nothing failed"
        );

        // A failed dual-review offers each alternate model.
        app.set_stage(StageId::DualReview, StageStatus::Failed, None);
        let entries = app.palette_filtered();
        assert!(entries.iter().any(|(l, a)| {
            l == "retry dual-review with claude-sonnet-5"
                && matches!(a, Action::RetryStage(StageId::DualReview, 0))
        }));
        // Interactive stages never offer retries even when failed.
        app.set_stage(StageId::Implement, StageStatus::Failed, None);
        assert!(
            !app.palette_filtered()
                .iter()
                .any(|(l, _)| l.contains("retry implement"))
        );
        let _ = tx; // dispatch would spawn; consumption is on_enter's take()
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
