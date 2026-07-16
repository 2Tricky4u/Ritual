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
    /// A claude plan fix finished; the tail carries the run's final
    /// assistant text (the ANSWERS block source) plus last-words context.
    FixExited(Box<Result<RunOutcome>>, FixTail),
    /// The code-fix run finished (leg 1 of the code-fix pipeline).
    CodeFixExited(Box<Result<RunOutcome>>, FixTail),
    /// The code-fix verification `./check.sh` finished (leg 2).
    CodeGateDone {
        ok: bool,
        tail: String,
    },
    /// The code-fix re-review run finished (leg 3).
    CodeReviewExited(Box<Result<RunOutcome>>, FixTail),
    CheckDone {
        ok: bool,
        tail: String,
    },
    AgentsStatus(Box<crate::agents_status::AgentsStatus>),
    FileChanged,
    Tick,
}

/// What the fix tail saw: the final result text (where the ANSWERS block
/// lives) and the last assistant prose (context when a failure has no
/// recorded reason at all).
#[derive(Debug, Default)]
pub struct FixTail {
    pub result_text: Option<String>,
    pub last_text: Option<String>,
}

/// Deferred request to hand the terminal to a child process.
#[derive(Debug, Clone)]
pub struct AttachedRequest {
    pub stage: Option<StageId>,
    pub argv: Vec<String>,
    pub cwd: std::path::PathBuf,
}

/// One queued finding inside a batch fix. Tracked by findings-file PATH, not
/// index: `reload_artifacts` invalidates indices, and the fix run can't touch
/// findings JSON (its tool lock only allows the plan file), so path+pos stay
/// stable across the run.
#[derive(Debug)]
struct FixItem {
    findings_path: std::path::PathBuf,
    pos: usize,
    /// 1-based number in the batch prompt (the ANSWERS block key).
    number: u32,
    /// `None` = the step couldn't be located: this item contributes a
    /// whole-doc range, degrading the union gate.
    section: Option<String>,
    range: std::ops::Range<usize>,
}

/// Everything `on_fix_exited` needs to gate + write back a batch plan fix.
#[derive(Debug)]
struct BatchFixCtx {
    slug: String,
    /// Branch the batch belongs to (meta records, notifications).
    branch: String,
    plan_path: std::path::PathBuf,
    items: Vec<FixItem>,
}

/// One queued CODE finding in a code-fix batch, tracked by findings-file PATH.
#[derive(Debug, Clone)]
struct CodeFixItem {
    findings_path: std::path::PathBuf,
    pos: usize,
    number: u32,
}

/// An owned copy of a code finding's data, kept so the re-review command (built
/// later, after the fix + check legs) can be reconstructed without re-reading
/// the findings.
#[derive(Debug, Clone)]
struct OwnedCodeBrief {
    number: u32,
    title: String,
    severity: String,
    scenario: String,
    file: String,
    line: Option<u32>,
    snippet: Option<String>,
}

/// The in-flight code-fix batch: its git snapshot (for auto-revert-on-failure),
/// the findings it targets, and the multi-leg state (fix → check.sh → review).
#[derive(Debug)]
struct CodeFixCtx {
    branch: String,
    run_cwd: std::path::PathBuf,
    snap: crate::git::GitSnapshot,
    items: Vec<CodeFixItem>,
    numbers: Vec<u32>,
    briefs: Vec<OwnedCodeBrief>,
    phase: crate::code_fix::CodePhase,
    answers: std::collections::HashMap<u32, crate::answers::AnswerVerdict>,
    /// The fix's rendered ChangeSet, captured right after the fix run (before
    /// check.sh, whose artifacts must not pollute the review evidence). The
    /// review leg consumes it; None there fails the batch - the reviewer is
    /// never handed an empty diff.
    fix_change: Option<String>,
}

/// The last APPLIED batch, so `u` can revert it: the one undo snapshot plus
/// which findings it marked fixed (declined ones are already back in triage).
struct LastBatch {
    slug: String,
    plan_path: std::path::PathBuf,
    fixed: Vec<(std::path::PathBuf, usize)>,
}

/// The F-apply confirm modal: what a `y` would spawn.
pub struct ApplyConfirm {
    pub slug: String,
    pub count: usize,
    /// Queued PLAN findings on this feature (→ the section-gated plan-fix).
    pub plan_count: usize,
    /// Queued CODE findings on this feature (→ the check.sh + re-review
    /// code-fix; a passing fix stays in the worktree for git).
    pub code_count: usize,
    /// Queued findings on OTHER features (skipped by this apply).
    pub skipped_other_features: usize,
    /// Items whose plan step no longer locates (whole-plan scope, gate off).
    pub anchor_lost: usize,
    /// The finding F was pressed on (`u` in the modal unqueues just it);
    /// None when opened from the palette.
    pub unqueue: Option<(std::path::PathBuf, usize)>,
}

/// The `t` one-touch triage confirm: every recommended disposition, staged.
/// Identities by PATH (a background reload must not retarget the writes).
pub struct TriageConfirm {
    pub items: Vec<(std::path::PathBuf, usize, crate::findings::Recommendation)>,
    pub archive: usize,
    pub queue_auto: usize,
    pub queue_manual: usize,
    pub dismiss: usize,
    pub needs_you: usize,
}

/// The `d` dismiss prompt: identity captured at open (by PATH - a background
/// reload must not retarget the write), plus the reason being typed.
pub struct DismissPrompt {
    pub findings_path: std::path::PathBuf,
    pub pos: usize,
    pub title: String,
    pub input: String,
}

pub struct App {
    pub cfg: Config,
    pub dirs: RitualDirs,
    pub state: State,
    /// mtime of `state.json` at the last reload, so a Tick only re-reads it when
    /// a concurrent CLI command actually changed it.
    state_mtime: Option<std::time::SystemTime>,
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
    /// `d`'s one-line reason prompt (None = closed).
    pub dismiss_prompt: Option<DismissPrompt>,
    /// The F-apply confirm modal (None = closed).
    pub apply_confirm: Option<ApplyConfirm>,
    /// The `t` triage-all confirm modal (None = closed).
    pub triage_confirm: Option<TriageConfirm>,
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
    /// A `reset-plan` (palette) is awaiting y/n confirmation.
    pub reset_plan_confirm: bool,
    pub quit: bool,
    pub palette: Option<PaletteState>,
    pub plan_scroll: usize,
    pub guide_scroll: usize,
    pub chat: Option<ChatState>,
    /// The `S` settings editor overlay (None = closed).
    pub settings: Option<SettingsState>,
    /// The `implement` copy-paste-prompt overlay shown before the handover.
    pub implement_hint: Option<ImplementHint>,

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
    /// The in-flight batch plan fix, if any (one at a time).
    fix_ctx: Option<BatchFixCtx>,
    /// The last APPLIED batch, revertable with `u` until a newer doc edit lands.
    last_fix: Option<LastBatch>,
    /// The in-flight code-fix batch (fix → check.sh → re-review), if any.
    code_fix_ctx: Option<CodeFixCtx>,
    code_fix_task: Option<JoinHandle<()>>,
    current_code_run_id: Option<String>,
    /// True while the code-fix gate's detached `check.sh` is in flight. It has
    /// no join handle (it runs off-loop), so cancelling the batch can't kill it;
    /// this flag keeps `run_check` from launching a SECOND check.sh against the
    /// same checkout (build-lock contention) until the orphan finishes.
    gate_check_running: bool,
    /// Open plan findings whose step no longer locates (path, pos): shown as
    /// ⚓ instead of silently mis-anchoring. Rebuilt on every artifact reload.
    anchor_lost: std::collections::HashSet<(std::path::PathBuf, usize)>,
    /// CLI --theme/--ascii, stashed so a settings write can re-run the full
    /// layered Config::load with the flags still winning.
    theme_flag: Option<String>,
    ascii_flag: bool,
}

/// Command palette state: typed filter + selection over matching entries.
#[derive(Debug, Clone, Default)]
pub struct PaletteState {
    pub input: String,
    pub selected: usize,
}

/// The `implement` launch prompt-overlay. An interactive `claude --resume`
/// can't be handed an opening message, so before the handover ritual shows the
/// suggested instruction for the user to copy and paste into the resumed
/// session. `enter` opens the session; `esc` cancels.
#[derive(Debug, Clone)]
pub struct ImplementHint {
    pub req: AttachedRequest,
    /// true = resuming the pinned tests-red session; false = the resume picker.
    pub resuming: bool,
    /// Whether the implement prompt was copied to the system clipboard.
    pub copied: bool,
}

/// The `S` settings editor: a cursor over `settings::CATALOG` plus an
/// optional inline edit line. Writes are transactional (see apply_setting).
#[derive(Debug, Clone, Default)]
pub struct SettingsState {
    pub selected: usize,
    pub edit: Option<SettingsEdit>,
    /// Per-catalog-row source tags (default/user/project/flag), refreshed on
    /// open and after every write - cheap to cache, wasteful per frame.
    pub sources: Vec<&'static str>,
}

/// Inline edit line for a numeric/text setting (Enter on a non-toggle row).
#[derive(Debug, Clone, Default)]
pub struct SettingsEdit {
    pub input: String,
    /// Validation error shown under the input; the prompt stays open.
    pub error: Option<String>,
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
        let state_mtime = std::fs::metadata(dirs.state_file())
            .and_then(|m| m.modified())
            .ok();
        let mut app = Self {
            cfg,
            dirs,
            state: st,
            state_mtime,
            branch,
            slug,
            selected: 0,
            tab: Tab::Live,
            stream: Vec::new(),
            stream_scroll: None,
            findings,
            selected_finding: 0,
            finding_detail: false,
            dismiss_prompt: None,
            apply_confirm: None,
            triage_confirm: None,
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
            reset_plan_confirm: false,
            quit: false,
            palette: None,
            plan_scroll: 0,
            guide_scroll: 0,
            chat: None,
            settings: None,
            implement_hint: None,
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
            code_fix_ctx: None,
            code_fix_task: None,
            gate_check_running: false,
            current_code_run_id: None,
            anchor_lost: std::collections::HashSet::new(),
            theme_flag: None,
            ascii_flag: false,
        };
        app.recompute_anchors();
        // One-time startup warning when THIS feature's slug is shared by
        // another local branch: their state/plan/findings scopes silently
        // merge (one git call; empty outside a repo).
        if let Some((_, branches)) = crate::state::slug_collisions(&app.dirs.work_root)
            .into_iter()
            .find(|(slug, _)| slug == &app.slug)
        {
            app.status_msg = Some(format!(
                "warning: branches {} share state slug '{}' - rename one",
                branches.join(" and "),
                app.slug
            ));
        }
        Ok(app)
    }

    /// True while a claude plan fix (`F`) is running.
    pub fn fix_running(&self) -> bool {
        self.fix_ctx.is_some() || self.code_fix_ctx.is_some()
    }

    /// Statusline / overlay label for the in-flight fix, e.g. `fix §Steps`.
    pub fn fix_label(&self) -> Option<String> {
        if let Some(c) = &self.code_fix_ctx {
            let leg = match c.phase {
                crate::code_fix::CodePhase::Fixing => "fixing",
                crate::code_fix::CodePhase::Checking => "check.sh",
                crate::code_fix::CodePhase::Reviewing => "reviewing",
            };
            return Some(format!("code-fix: {leg} ⚑{}", c.items.len()));
        }
        self.fix_ctx
            .as_ref()
            .map(|c| format!("fix ⚑{}", c.items.len()))
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
        // Reload-merge-save: fold in any concurrent CLI write BEFORE applying our
        // delta, so the TUI's save can't clobber it (the load-once TUI otherwise
        // overwrites the whole file with a stale snapshot).
        self.reload_state();
        crate::run_cmd::set_stage(&mut self.state, &self.branch, stage, status, run_id);
        let _ = self.state.save(&self.dirs);
        self.state_mtime = std::fs::metadata(self.dirs.state_file())
            .and_then(|m| m.modified())
            .ok();
    }

    /// Adopt state written by a concurrent CLI command WITHOUT clobbering the
    /// TUI's own in-flight pipeline run (the TUI owns `self.running`; disk may
    /// not reflect it yet). Also refreshes findings/metas, which the same CLI
    /// commands change.
    fn reload_state(&mut self) {
        let Ok(mut disk) = State::load(&self.dirs) else {
            return;
        };
        if let Some(stage) = self.running {
            let slug = state::branch_slug(&self.branch);
            if let Some(mem) = self
                .state
                .features
                .get(&slug)
                .and_then(|f| f.stages.get(&stage).cloned())
            {
                disk.feature_for_branch_mut(&self.branch)
                    .stages
                    .insert(stage, mem);
            }
        }
        self.state = disk;
        self.reload_artifacts();
    }

    fn reload_artifacts(&mut self) {
        self.findings = crate::findings::load_all(&self.dirs.findings_dir()).unwrap_or_default();
        self.metas = crate::history::load_all(&self.dirs.runs_dir()).unwrap_or_default();
        self.recompute_anchors();
    }

    /// Re-resolve every open plan finding's step against the CURRENT plan.
    /// Any edit (batch fix, chat, undo, external) can move or delete a step;
    /// findings whose anchor no longer locates get an explicit ⚓ marker
    /// instead of silently mis-anchoring. Runs on every artifact reload -
    /// the funnel every edit path already goes through.
    fn recompute_anchors(&mut self) {
        let mut plans: std::collections::HashMap<String, String> = std::collections::HashMap::new();
        let mut lost = std::collections::HashSet::new();
        for (file_idx, lf) in self.findings.iter().enumerate() {
            for (pos, f) in lf.file.findings.iter().enumerate() {
                if f.resolved() || f.file.is_some() {
                    continue;
                }
                let Some(step) = f.plan_step.as_deref() else {
                    continue;
                };
                let slug = self.finding_slug(file_idx);
                let plan = plans.entry(slug.clone()).or_insert_with(|| {
                    std::fs::read_to_string(self.dirs.plan_file(&slug)).unwrap_or_default()
                });
                if locate_plan_step(plan, step).is_none() {
                    lost.insert((lf.path.clone(), pos));
                }
            }
        }
        self.anchor_lost = lost;
    }

    /// True when this finding's plan step no longer locates in the plan.
    pub fn is_anchor_lost(&self, af: &crate::findings::AggregatedFinding) -> bool {
        self.findings
            .get(af.file_idx)
            .is_some_and(|lf| self.anchor_lost.contains(&(lf.path.clone(), af.pos)))
    }

    /// Handle one message. Side effects that need the terminal (attached
    /// children) are deferred via `pending_attached`.
    pub fn update(&mut self, msg: AppMsg, tx: &mpsc::Sender<AppMsg>) {
        match msg {
            AppMsg::Tick => {
                self.spinner = self.spinner.wrapping_add(1);
                // Pick up state a concurrent CLI command wrote (`ritual run`/
                // `complete`/`reset-plan`), gated on the file's mtime so we only
                // re-read when it actually changed.
                if let Ok(m) =
                    std::fs::metadata(self.dirs.state_file()).and_then(|md| md.modified())
                    && Some(m) != self.state_mtime
                {
                    self.state_mtime = Some(m);
                    self.reload_state();
                }
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
            AppMsg::FixExited(outcome, tail) => self.on_fix_exited(*outcome, tail, tx),
            AppMsg::CodeFixExited(outcome, tail) => self.on_code_fix_exited(*outcome, tail, tx),
            AppMsg::CodeGateDone { ok, tail } => self.on_code_gate_done(ok, tail, tx),
            AppMsg::CodeReviewExited(outcome, tail) => {
                self.on_code_review_exited(*outcome, tail, tx)
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
        if self.reset_plan_confirm {
            match key.code {
                KeyCode::Char('y') => self.do_reset_plan(),
                _ => self.reset_plan_confirm = false,
            }
            return;
        }
        if self.dismiss_prompt.is_some() {
            self.dismiss_input(key.code);
            return;
        }
        if self.apply_confirm.is_some() {
            self.apply_confirm_input(key.code, tx);
            return;
        }
        if self.triage_confirm.is_some() {
            self.triage_confirm_input(key.code);
            return;
        }
        if self.implement_hint.is_some() {
            self.implement_hint_input(key.code);
            return;
        }
        if self.show_help {
            // The which-key cheat-sheet stays up until you dismiss it with the
            // help key again or Esc; other keys are swallowed so it can be read.
            if key.code == KeyCode::Esc
                || self.cfg.keymap.resolve(key.code, key.modifiers) == Some(Action::Help)
            {
                self.show_help = false;
            }
            return;
        }
        if self.settings.is_some() {
            self.settings_input(key.code);
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
            Action::Help => self.show_help = !self.show_help,
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
            Action::Cancel => self.cancel_run(tx),
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
            Action::FindingDismiss => self.open_dismiss_prompt(),
            Action::FindingClaudeFix => self.finding_claude_answer(tx),
            Action::FindingManual => self.finding_toggle_manual(),
            Action::FindingsApply => self.findings_apply_from_palette(tx),
            Action::TriageAll => self.open_triage_confirm(),
            Action::QueueAllCode => self.queue_all_code(),
            Action::DocUndo => self.doc_undo(),
            Action::Settings => self.toggle_settings(),
            Action::ResetPlan => {
                if self.fix_running() || self.chat_running() {
                    self.status_msg = Some("a fix/chat is running; wait before resetting".into());
                } else {
                    self.reset_plan_confirm = true;
                }
            }
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
        if self.running.is_some() || self.chat_running() || self.fix_running() {
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
        // Pin/resolve the claude session so the tests-red → implement handoff is
        // deterministic. tests-red mints a ritual-owned id (persisted only once
        // the launch is committed, below); implement resumes that exact id, or
        // falls back to the `--resume` picker when none is pinned.
        let session: Option<String> = match stage {
            StageId::TestsRed => Some(crate::export::fresh_session_id()),
            StageId::Implement => self.state.stage_session_id(&self.slug, StageId::TestsRed),
            _ => None,
        };
        let cmd = match stages::build(
            stage,
            &self.cfg,
            &self.dirs,
            &self.slug,
            None,
            model_override.as_deref(),
            session.as_deref(),
            // The (possibly linked-worktree) checkout this run executes in:
            // git preflights must probe THIS tree, not work_root.
            Some(&run_cwd),
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
                // Now that the launch is committed, persist tests-red's pinned
                // session id so a later `implement` resumes this exact
                // conversation (survives quitting mid-run).
                if stage == StageId::TestsRed
                    && let Some(sid) = &session
                {
                    self.state.set_stage_session_id(
                        &self.slug,
                        StageId::TestsRed,
                        Some(sid.clone()),
                    );
                    let _ = self.state.save(&self.dirs);
                }
                let req = AttachedRequest {
                    stage: Some(stage),
                    argv: cmd.argv,
                    cwd: run_cwd,
                };
                // implement resumes a conversation the CLI can't be handed an
                // opening message for, so show the copy-paste prompt first;
                // `enter` in the overlay commits the handover.
                if stage == StageId::Implement {
                    // Copy the prompt straight to the clipboard so the user
                    // doesn't have to mouse-select it out of the float (which
                    // grabs the sidebar behind it too).
                    let copied = crate::clipboard::copy(stages::IMPLEMENT_PROMPT);
                    self.implement_hint = Some(ImplementHint {
                        req,
                        resuming: session.is_some(),
                        copied,
                    });
                } else {
                    self.pending_attached = Some(req);
                }
            }
            Mode::Headless => self.spawn_headless(stage, cmd, run_cwd, tx),
        }
    }

    /// `a`: reattach interactively to the selected stage's session
    /// (`claude --resume <session_id>`). Headless stages resolve the id from
    /// the last run's meta; interactive stages (tests-red/implement) use the
    /// session id ritual pinned on the stage.
    fn takeover(&mut self) {
        let stage = self.selected_stage();
        let st = self.state.features.get(&self.slug).map(|f| f.stage(stage));
        let from_run = st.as_ref().and_then(|s| s.runs.last()).and_then(|rid| {
            self.metas
                .iter()
                .find(|m| &m.run_id == rid)
                .and_then(|m| m.session_id.clone())
        });
        let Some(sid) = from_run.or_else(|| st.and_then(|s| s.session_id)) else {
            self.status_msg = Some(format!("no session recorded for {}", stage.label()));
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
            stdin: cmd.stdin,
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
            stdin: cmd.stdin,
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
        for feature in self.state.features.values() {
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
                } else if stage == StageId::Coverage {
                    // Coverage is Done ONLY at zero gaps + a real deliverables
                    // checklist (shared, print-free finalizer - never write to
                    // the alt-screen); route its message to the status line.
                    let (st, msgs) =
                        crate::coverage::finalize(&self.dirs, &self.branch, &new_findings);
                    if let Some(m) = msgs.into_iter().next_back() {
                        self.status_msg = Some(m);
                    }
                    st
                } else if new_findings.is_empty() {
                    StageStatus::NeedsAttention
                } else {
                    StageStatus::Done
                };
                // Stamp the real branch onto this run's findings so completeness
                // consumers scope by branch (parity with the CLI; after finalize
                // so A3's fingerprint reflects the post-tick tree, before reload).
                crate::findings::stamp_branch(
                    &self.dirs.findings_dir(),
                    &new_findings,
                    &self.branch,
                );
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
                    &if status == StageStatus::Failed {
                        format!(
                            "{}: {}",
                            self.branch,
                            crate::history::decode_failure(&out.meta)
                        )
                    } else {
                        format!(
                            "{}: {} new findings, ${:.2}",
                            self.branch,
                            new_findings.len(),
                            out.meta.total_cost_usd.unwrap_or(0.0)
                        )
                    },
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
                        crate::history::decode_failure(&out.meta)
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

    fn cancel_run(&mut self, tx: &mpsc::Sender<AppMsg>) {
        // A code-fix in flight: kill the live leg and drop the ctx. The attempt
        // is LEFT in the working tree (git is the undo); findings stay queued.
        // A late exit message lands on the now-None ctx and is ignored.
        if let Some(ctx) = self.code_fix_ctx.take() {
            if let Some(id) = self.current_code_run_id.take() {
                runner::kill_run(&self.dirs, &id);
            }
            if let Some(task) = self.code_fix_task.take() {
                task.abort();
            }
            self.status_msg = Some(format!(
                "code-fix cancelled; the attempt is in your working tree ({} stay queued)",
                ctx.items.len()
            ));
            self.drain_pending_chat(tx);
            return;
        }
        // A plan-fix in flight: kill the leg and REVERT the half-written plan.
        // Plan-fix's whole model is revert-on-failure and git does not cover the
        // doc edit, so - unlike code-fix - we must restore it here.
        if let Some(ctx) = self.fix_ctx.take() {
            if let Some(id) = self.current_fix_run_id.take() {
                runner::kill_run(&self.dirs, &id);
            }
            if let Some(task) = self.fix_task.take() {
                task.abort();
            }
            let _ = crate::undo::undo(
                &self.dirs,
                &ctx.slug,
                stages::DocKind::Plan.label(),
                &ctx.plan_path,
            );
            self.reload_artifacts();
            self.status_msg = Some(format!(
                "plan-fix cancelled; plan reverted ({} stay queued)",
                ctx.items.len()
            ));
            self.drain_pending_chat(tx);
            return;
        }
        // Otherwise a pipeline stage: a detached process group, so kill it there
        // then stop the local tail.
        let Some(run_id) = self.current_run_id.take() else {
            self.status_msg = Some("no active run".into());
            return;
        };
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
        // A fix runs its own check.sh gate in the checkout; a second one here
        // would fight over the build lock (the idle FileChanged path already
        // guards this, but the c/C keys and palette reach run_check directly).
        if self.fix_running() || self.gate_check_running {
            self.status_msg = Some("a fix is running; check resumes when it finishes".into());
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
            // Opens the reason prompt above the overlay; committing closes both.
            Some(Action::FindingDismiss) => self.open_dismiss_prompt(),
            // Stays open: queueing updates the footer, applying shows the
            // spinner, and on_fix_exited closes it with the verdict.
            Some(Action::FindingClaudeFix) => self.finding_claude_answer(tx),
            Some(Action::FindingManual) => self.finding_toggle_manual(),
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

    /// `d`: open the one-line reason prompt for the selected finding.
    /// Already-dismissed findings toggle straight back to pending (no prompt).
    /// Keys while the implement launch overlay is up: `enter` commits the
    /// handover to `claude --resume`; `c` re-copies the prompt; `esc`/other
    /// cancels (nothing launched).
    fn implement_hint_input(&mut self, code: KeyCode) {
        match code {
            KeyCode::Enter => {
                if let Some(hint) = self.implement_hint.take() {
                    self.pending_attached = Some(hint.req);
                }
            }
            KeyCode::Char('c') => {
                let ok = crate::clipboard::copy(stages::IMPLEMENT_PROMPT);
                if let Some(h) = self.implement_hint.as_mut() {
                    h.copied = ok;
                }
                self.status_msg = Some(if ok {
                    "implement prompt copied to clipboard".into()
                } else {
                    "couldn't reach a clipboard - select the prompt manually".into()
                });
            }
            _ => {
                self.implement_hint = None;
                self.status_msg = Some("implement cancelled".into());
            }
        }
    }

    /// `S`: open/close the settings editor overlay (any tab, like help).
    fn toggle_settings(&mut self) {
        self.settings = match self.settings {
            Some(_) => None,
            None => Some(SettingsState {
                selected: 0,
                edit: None,
                sources: self.compute_setting_sources(),
            }),
        };
    }

    /// Per-catalog-row source tags. "flag" marks the two keys a CLI flag
    /// shadows this session (the write still lands and wins next launch).
    fn compute_setting_sources(&self) -> Vec<&'static str> {
        crate::settings::CATALOG
            .iter()
            .map(|d| self.setting_source_tag(d.key))
            .collect()
    }

    fn setting_source_tag(&self, key: &str) -> &'static str {
        if (key == "theme" && self.theme_flag.is_some()) || (key == "icons" && self.ascii_flag) {
            return "flag";
        }
        let user = dirs::config_dir().map(|d| d.join("ritual/config.toml"));
        let project = self.dirs.project_root.join(".ritual/config.toml");
        crate::settings::source_of(user.as_deref(), &project, key).tag()
    }

    /// Keys while the settings overlay is up; it consumes everything.
    fn settings_input(&mut self, code: KeyCode) {
        if self.settings.as_ref().is_some_and(|s| s.edit.is_some()) {
            self.settings_edit_input(code);
            return;
        }
        let last = crate::settings::CATALOG.len().saturating_sub(1);
        match code {
            KeyCode::Esc | KeyCode::Char('S') => self.settings = None,
            KeyCode::Char('j') | KeyCode::Down => {
                if let Some(s) = self.settings.as_mut() {
                    s.selected = (s.selected + 1).min(last);
                }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                if let Some(s) = self.settings.as_mut() {
                    s.selected = s.selected.saturating_sub(1);
                }
            }
            KeyCode::Char('g') => {
                if let Some(s) = self.settings.as_mut() {
                    s.selected = 0;
                }
            }
            KeyCode::Char('G') => {
                if let Some(s) = self.settings.as_mut() {
                    s.selected = last;
                }
            }
            KeyCode::Enter => self.settings_activate(),
            _ => {}
        }
    }

    /// Enter on a settings row: toggles/cycles write immediately; numeric and
    /// text kinds open the inline edit prompt (P3).
    fn settings_activate(&mut self) {
        use crate::settings::{SettingKind, SettingValue};
        let Some(idx) = self.settings.as_ref().map(|s| s.selected) else {
            return;
        };
        let Some(def) = crate::settings::CATALOG.get(idx) else {
            return;
        };
        let current = (def.get)(&self.cfg);
        match def.kind {
            SettingKind::Bool => {
                let now = current.as_deref() == Some("true");
                self.apply_setting(idx, Some(SettingValue::Bool(!now)));
            }
            SettingKind::Enum(variants) => {
                let pos = current
                    .as_deref()
                    .and_then(|cur| variants.iter().position(|v| *v == cur));
                let next = variants[pos.map_or(0, |p| (p + 1) % variants.len())];
                self.apply_setting(idx, Some(SettingValue::Str(next.to_string())));
            }
            SettingKind::OptEnum(variants) => {
                // variants… → unset → first again.
                let pos = current
                    .as_deref()
                    .and_then(|cur| variants.iter().position(|v| *v == cur));
                match pos {
                    Some(p) if p + 1 < variants.len() => {
                        self.apply_setting(idx, Some(SettingValue::Str(variants[p + 1].into())));
                    }
                    Some(_) => self.apply_setting(idx, None),
                    None => self.apply_setting(idx, Some(SettingValue::Str(variants[0].into()))),
                }
            }
            _ => {
                // Numeric/text kinds edit inline, prefilled with the
                // effective value (empty when unset).
                if let Some(s) = self.settings.as_mut() {
                    s.edit = Some(SettingsEdit {
                        input: current.unwrap_or_default(),
                        error: None,
                    });
                }
            }
        }
    }

    /// Keys while the inline edit line is open: Enter validates then applies
    /// (empty clears Opt* keys), a rejected value keeps the prompt open with
    /// the error shown, Esc drops the edit but keeps the overlay.
    fn settings_edit_input(&mut self, code: KeyCode) {
        match code {
            KeyCode::Esc => {
                if let Some(s) = self.settings.as_mut() {
                    s.edit = None;
                }
            }
            KeyCode::Enter => {
                let Some(s) = self.settings.as_ref() else {
                    return;
                };
                let idx = s.selected;
                let Some(def) = crate::settings::CATALOG.get(idx) else {
                    return;
                };
                let input = s.edit.as_ref().map(|e| e.input.clone()).unwrap_or_default();
                match crate::settings::validate(&def.kind, &input) {
                    Err(msg) => {
                        if let Some(e) = self.settings.as_mut().and_then(|s| s.edit.as_mut()) {
                            e.error = Some(msg);
                        }
                    }
                    Ok(value) => {
                        if let Some(s) = self.settings.as_mut() {
                            s.edit = None;
                        }
                        self.apply_setting(idx, value);
                    }
                }
            }
            KeyCode::Backspace => {
                if let Some(e) = self.settings.as_mut().and_then(|s| s.edit.as_mut()) {
                    e.input.pop();
                    e.error = None;
                }
            }
            KeyCode::Char(c) => {
                if let Some(e) = self.settings.as_mut().and_then(|s| s.edit.as_mut()) {
                    e.input.push(c);
                    e.error = None;
                }
            }
            _ => {}
        }
    }

    /// One settings write, transactionally: write the project config, re-run
    /// the full layered Config::load with the stashed CLI flags, swap it in -
    /// or restore the previous bytes. The file on disk always passes
    /// Config::load.
    fn apply_setting(&mut self, idx: usize, value: Option<crate::settings::SettingValue>) {
        let Some(def) = crate::settings::CATALOG.get(idx) else {
            return;
        };
        let path = self.dirs.project_root.join(".ritual/config.toml");
        let prev = std::fs::read_to_string(&path).ok(); // None = file absent
        if let Err(e) = crate::settings::write_setting(&path, def.key, value.as_ref()) {
            self.status_msg = Some(format!("could not write config: {e:#}"));
            return;
        }
        match Config::load(
            &self.dirs.project_root,
            self.theme_flag.as_deref(),
            self.ascii_flag,
        ) {
            Ok(cfg) => {
                self.cfg = cfg;
                if let Some(s) = self.settings.as_mut() {
                    s.sources = Vec::new(); // rebuilt below, after the borrow ends
                }
                let sources = self.compute_setting_sources();
                let mut msg = match &value {
                    Some(v) => format!("set {} = {v} (project)", def.key),
                    None => {
                        let effective = (def.get)(&self.cfg).unwrap_or_else(|| "unset".into());
                        let source = sources.get(idx).copied().unwrap_or("default");
                        format!("cleared {} (project) → now {effective} ({source})", def.key)
                    }
                };
                if (def.key == "theme" && self.theme_flag.is_some())
                    || (def.key == "icons" && self.ascii_flag)
                {
                    msg.push_str(" - a CLI flag overrides this session");
                }
                if let Some(s) = self.settings.as_mut() {
                    s.sources = sources;
                }
                self.status_msg = Some(msg);
            }
            Err(e) => {
                let restore = match prev {
                    Some(text) => std::fs::write(&path, text),
                    None => std::fs::remove_file(&path),
                };
                self.status_msg = Some(match restore {
                    Ok(()) => format!("config rejected, reverted: {e:#}"),
                    Err(re) => format!("config rejected AND revert failed ({re}): {e:#}"),
                });
            }
        }
    }

    fn open_dismiss_prompt(&mut self) {
        if self.tab != Tab::Findings {
            self.status_msg = Some("dismiss works on the findings tab (2)".into());
            return;
        }
        let Some(af) = self.selected_finding_af() else {
            self.status_msg = Some("no finding selected".into());
            return;
        };
        if af.finding.action == "dismissed" {
            self.finding_set_action("dismissed"); // same-value toggle -> pending
            return;
        }
        self.dismiss_prompt = Some(DismissPrompt {
            findings_path: self.findings[af.file_idx].path.clone(),
            pos: af.pos,
            title: af.finding.title.clone(),
            input: String::new(),
        });
    }

    /// Keys while the dismiss prompt is open: Enter commits (empty = plain
    /// dismiss), Esc cancels without writing anything.
    fn dismiss_input(&mut self, code: KeyCode) {
        match code {
            KeyCode::Esc => {
                self.dismiss_prompt = None;
            }
            KeyCode::Enter => {
                let Some(p) = self.dismiss_prompt.take() else {
                    return;
                };
                // Re-find by PATH: a background reload may have shifted indices.
                let Some(i) = self
                    .findings
                    .iter()
                    .position(|lf| lf.path == p.findings_path)
                else {
                    self.status_msg = Some("finding vanished; nothing dismissed".into());
                    return;
                };
                let reason = p.input.trim();
                let reason = (!reason.is_empty()).then_some(reason);
                match crate::findings::set_action_with_reason(
                    &mut self.findings,
                    i,
                    p.pos,
                    "dismissed",
                    reason,
                ) {
                    Ok(()) => {
                        self.status_msg = Some(match reason {
                            Some(r) => format!("{}: dismissed - {r}", p.title),
                            None => format!("{}: dismissed", p.title),
                        });
                    }
                    Err(e) => self.status_msg = Some(format!("could not dismiss: {e:#}")),
                }
                self.finding_detail = false;
                self.clamp_selected_finding();
            }
            KeyCode::Backspace => {
                if let Some(p) = self.dismiss_prompt.as_mut() {
                    p.input.pop();
                }
            }
            KeyCode::Char(c) => {
                if let Some(p) = self.dismiss_prompt.as_mut() {
                    p.input.push(c);
                }
            }
            _ => {}
        }
    }

    /// `m`: toggle the "I'll fix this myself" answer on any open finding
    /// (code findings are the canonical manual case - `Q` routes them).
    fn finding_toggle_manual(&mut self) {
        if self.tab != Tab::Findings {
            self.status_msg = Some("m works on the findings tab (2)".into());
            return;
        }
        let Some(af) = self.selected_finding_af() else {
            self.status_msg = Some("no finding selected".into());
            return;
        };
        if af.finding.resolved() {
            self.status_msg = Some("finding is already resolved".into());
            return;
        }
        let manual = af.finding.answer.as_deref() == Some("manual");
        let next = if manual { None } else { Some("manual") };
        match crate::findings::set_answer(&mut self.findings, af.file_idx, af.pos, next, None) {
            Ok(()) => {
                self.status_msg = Some(if manual {
                    format!("{}: manual answer cleared", af.finding.title)
                } else {
                    format!(
                        "⚑M {}: yours to fix (Q routes manual findings)",
                        af.finding.title
                    )
                });
            }
            Err(e) => self.status_msg = Some(format!("could not update finding: {e:#}")),
        }
    }

    /// Open findings queued for the claude batch (answer == "auto").
    pub fn queued_auto(&self) -> Vec<crate::findings::AggregatedFinding> {
        crate::findings::aggregate(&self.findings, false)
            .into_iter()
            .filter(|af| af.finding.answer.as_deref() == Some("auto"))
            // An anchorless finding (no file AND no plan_step) has nothing to
            // fix; a hand-edited `answer:"auto"` on one would inflate the apply
            // count and then dead-end. Neither batch would select it.
            .filter(|af| af.finding.file.is_some() || af.finding.plan_step.is_some())
            .collect()
    }

    /// `F`: answer a plan finding with "claude fixes it" (⚑A, toggled), the
    /// triage half of answer-all-then-apply-once.
    fn finding_claude_answer(&mut self, tx: &mpsc::Sender<AppMsg>) {
        let _ = tx; // the apply confirm (P6) spawns; queueing itself never does
        if self.tab != Tab::Findings {
            self.status_msg = Some("F answers findings on the findings tab (2)".into());
            return;
        }
        let Some(af) = self.selected_finding_af() else {
            self.status_msg = Some("no finding selected".into());
            return;
        };
        // Both plan findings (plan_step) and code findings (file:line) queue
        // for claude; only a finding with neither anchor has nothing to fix.
        if af.finding.file.is_none() && af.finding.plan_step.is_none() {
            self.status_msg = Some("finding has no location to fix".into());
            return;
        }
        if af.finding.resolved() {
            self.status_msg = Some("finding is already resolved".into());
            return;
        }
        if af.finding.answer.as_deref() == Some("auto") {
            // F on a queued finding = time to apply (or unqueue just this one).
            let unqueue = Some((self.findings[af.file_idx].path.clone(), af.pos));
            self.open_apply_confirm(unqueue);
            return;
        }
        let kind = if af.finding.file.is_some() {
            "code-fix"
        } else {
            "claude"
        };
        match crate::findings::set_answer(
            &mut self.findings,
            af.file_idx,
            af.pos,
            Some("auto"),
            None,
        ) {
            Ok(()) => {
                self.status_msg = Some(format!(
                    "⚑A queued for {kind} ({} queued) · F again to apply",
                    self.queued_auto().len()
                ));
            }
            Err(e) => self.status_msg = Some(format!("could not update finding: {e:#}")),
        }
    }

    /// `A`: queue EVERY confirmed, unresolved, un-answered CODE finding on this
    /// feature for the code-fix batch ("fix all automatically"). Then F / the
    /// apply modal runs one code-fix run over the lot.
    fn queue_all_code(&mut self) {
        if self.tab != Tab::Findings {
            self.status_msg = Some("A queues code fixes on the findings tab (2)".into());
            return;
        }
        // Collect (file_idx, pos) by PATH-stable identity before mutating.
        let targets: Vec<(usize, usize)> = self
            .visible_findings()
            .iter()
            .filter(|af| self.finding_slug(af.file_idx) == self.slug)
            .filter(|af| {
                af.finding.file.is_some()
                    && crate::findings::verdict_confirmed(&af.finding.verdict)
                    && !af.finding.resolved()
                    && af.finding.answer.is_none()
            })
            .map(|af| (af.file_idx, af.pos))
            .collect();
        if targets.is_empty() {
            self.status_msg = Some("no un-queued confirmed code findings to fix".into());
            return;
        }
        let mut queued = 0usize;
        for (file_idx, pos) in targets {
            if crate::findings::set_answer(&mut self.findings, file_idx, pos, Some("auto"), None)
                .is_ok()
            {
                queued += 1;
            }
        }
        self.status_msg = Some(format!(
            "⚑A queued {queued} code finding(s) - F or apply to run the code-fix batch"
        ));
    }

    /// Palette "findings: apply answers": same modal, nothing to unqueue.
    fn findings_apply_from_palette(&mut self, tx: &mpsc::Sender<AppMsg>) {
        let _ = tx;
        self.open_apply_confirm(None);
    }

    /// Open the apply-confirm modal for the feature in view: how many queued
    /// answers a `y` would send, what gets skipped, and how degraded the
    /// gate would be.
    fn open_apply_confirm(&mut self, unqueue: Option<(std::path::PathBuf, usize)>) {
        let queued = self.queued_auto();
        let (mine, other): (Vec<_>, Vec<_>) = queued
            .into_iter()
            .partition(|af| self.finding_slug(af.file_idx) == self.slug);
        if mine.is_empty() {
            self.status_msg = Some(if other.is_empty() {
                "no queued answers - F queues a finding for claude".into()
            } else {
                format!(
                    "{} queued on other features; switch with [ ] to apply them",
                    other.len()
                )
            });
            return;
        }
        let plan_count = mine
            .iter()
            .filter(|af| af.finding.file.is_none() && af.finding.plan_step.is_some())
            .count();
        let code_count = mine.iter().filter(|af| af.finding.file.is_some()).count();
        // Anchor health is a PLAN concern; only compute it when the batch that
        // would run first (plan) has queued findings.
        let anchor_lost = if plan_count > 0 {
            let plan_text =
                std::fs::read_to_string(self.dirs.plan_file(&self.slug)).unwrap_or_default();
            mine.iter()
                .filter(|af| af.finding.file.is_none() && af.finding.plan_step.is_some())
                .filter(|af| {
                    af.finding
                        .plan_step
                        .as_deref()
                        .and_then(|s| locate_plan_step(&plan_text, s))
                        .is_none()
                })
                .count()
        } else {
            0
        };
        self.apply_confirm = Some(ApplyConfirm {
            slug: self.slug.clone(),
            count: mine.len(),
            plan_count,
            code_count,
            skipped_other_features: other.len(),
            anchor_lost,
            unqueue,
        });
    }

    /// Keys while the apply-confirm modal is open: `y` fires the batch run,
    /// `u` unqueues the finding F was pressed on, anything else closes.
    fn apply_confirm_input(&mut self, code: KeyCode, tx: &mpsc::Sender<AppMsg>) {
        let Some(confirm) = self.apply_confirm.take() else {
            return;
        };
        match code {
            KeyCode::Char('y') => {
                // One type per apply, and only one fix runs at a time. Plan
                // findings (section-gated, u-revertable) go first; the code
                // batch (check.sh + re-review, git-is-undo) runs on the next
                // apply once the plan queue is clear.
                if confirm.plan_count > 0 {
                    self.spawn_findings_apply(&confirm.slug, tx);
                } else {
                    self.spawn_code_fix(&confirm.slug, tx);
                }
            }
            KeyCode::Char('u') => {
                let Some((path, pos)) = confirm.unqueue else {
                    self.status_msg = Some("nothing selected to unqueue".into());
                    return;
                };
                if let Some(i) = self.findings.iter().position(|lf| lf.path == path) {
                    let _ = crate::findings::set_answer(&mut self.findings, i, pos, None, None);
                    self.status_msg = Some(format!(
                        "unqueued ({} still queued)",
                        self.queued_auto().len()
                    ));
                }
            }
            _ => {}
        }
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
        let (entries, manual_only) = self.quickfix_entries();
        let title = if manual_only {
            "ritual manual findings"
        } else {
            "ritual findings"
        };
        match crate::nvim::send_quickfix(&server, &entries, title) {
            Ok(n) => {
                self.status_msg = Some(if manual_only {
                    format!(" {n} manual finding(s) → nvim quickfix")
                } else {
                    format!(" {n} finding(s) → nvim quickfix")
                });
            }
            Err(e) => self.status_msg = Some(format!("nvim: {e:#}")),
        }
    }

    /// Quickfix candidates. When any open ⚑M (queued-manual) finding is
    /// locatable, `Q` becomes the MANUAL PASS and sends only those; with no
    /// manual queue it sends every locatable finding as before.
    pub fn quickfix_entries(&self) -> (Vec<crate::nvim::QfEntry>, bool) {
        let cwd = self
            .run_cwd()
            .unwrap_or_else(|| self.dirs.work_root.clone());
        let all = crate::findings::aggregate(&self.findings, self.show_resolved);
        let manual_only = all
            .iter()
            .any(|af| !af.finding.resolved() && af.finding.answer.as_deref() == Some("manual"));
        let entries = all
            .into_iter()
            .filter(|af| {
                !manual_only
                    || (!af.finding.resolved() && af.finding.answer.as_deref() == Some("manual"))
            })
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
        (entries, manual_only)
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

    /// `t`: compute the recommended disposition for every VISIBLE open
    /// finding (filter + hidden-resolved honored - exactly what the user
    /// sees) and stage them behind a confirm modal. Dispositions only:
    /// the plan itself still changes exclusively through F-apply.
    fn open_triage_confirm(&mut self) {
        if self.tab != Tab::Findings {
            self.status_msg = Some("t triages on the findings tab (2)".into());
            return;
        }
        let mut items = Vec::new();
        let (mut archive, mut qa, mut qm, mut dismiss, mut needs_you) = (0, 0, 0, 0, 0);
        for af in self.visible_findings() {
            let Some(rec) = crate::findings::recommend(&af.finding) else {
                continue; // resolved / triaged / declined: already handled
            };
            use crate::findings::Recommendation as R;
            match rec {
                R::Archive => archive += 1,
                R::QueueAuto => qa += 1,
                R::QueueManual => qm += 1,
                R::Dismiss(_) => dismiss += 1,
                R::NeedsYou => {
                    needs_you += 1;
                    continue; // shown in the count, never auto-applied
                }
            }
            items.push((self.findings[af.file_idx].path.clone(), af.pos, rec));
        }
        if items.is_empty() {
            self.status_msg = Some(if needs_you > 0 {
                format!("nothing to auto-triage - {needs_you} need your judgment")
            } else {
                "nothing to triage".into()
            });
            return;
        }
        self.triage_confirm = Some(TriageConfirm {
            items,
            archive,
            queue_auto: qa,
            queue_manual: qm,
            dismiss,
            needs_you,
        });
    }

    /// Keys while the triage confirm is open: `y` writes every staged
    /// disposition, anything else closes without writing.
    fn triage_confirm_input(&mut self, code: KeyCode) {
        let Some(confirm) = self.triage_confirm.take() else {
            return;
        };
        if code != KeyCode::Char('y') {
            return;
        }
        let mut applied = 0usize;
        for (path, pos, rec) in &confirm.items {
            // Re-find by PATH: a background reload may have shifted indices.
            let Some(i) = self.findings.iter().position(|lf| lf.path == *path) else {
                continue;
            };
            // A finding resolved mid-modal wins over the staged decision.
            if self.findings[i]
                .file
                .findings
                .get(*pos)
                .is_none_or(|f| f.resolved())
            {
                continue;
            }
            use crate::findings::Recommendation as R;
            let ok = match rec {
                // set_action_with_reason migrates prose actions into reason.
                R::Archive => crate::findings::set_action_with_reason(
                    &mut self.findings,
                    i,
                    *pos,
                    "fixed",
                    None,
                ),
                R::Dismiss(reason) => crate::findings::set_action_with_reason(
                    &mut self.findings,
                    i,
                    *pos,
                    "dismissed",
                    Some(reason),
                ),
                R::QueueAuto => {
                    crate::findings::set_answer(&mut self.findings, i, *pos, Some("auto"), None)
                }
                R::QueueManual => {
                    crate::findings::set_answer(&mut self.findings, i, *pos, Some("manual"), None)
                }
                R::NeedsYou => Ok(()), // never staged
            };
            if ok.is_ok() {
                applied += 1;
            }
        }
        self.recompute_anchors();
        self.clamp_selected_finding();
        self.status_msg = Some(format!(
            "triaged {applied}: {} archived · {} ⚑A · {} ⚑M · {} dismissed · {} need you",
            confirm.archive,
            confirm.queue_auto,
            confirm.queue_manual,
            confirm.dismiss,
            confirm.needs_you
        ));
    }

    /// Everything that must hold before the batch apply may spawn, plus the
    /// built command and write-back context. Side-effect-free until the last
    /// two steps (undo push + `fix_doc_before` snapshot), after every guard.
    /// ONE plan snapshot: every anchor resolves against the same text, so no
    /// fix can rot the next finding's anchor.
    fn prepare_findings_apply(
        &mut self,
        slug: &str,
    ) -> Result<(stages::StageCommand, BatchFixCtx), String> {
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
        let queued = self.queued_auto();
        let mine: Vec<_> = queued
            .iter()
            .filter(|af| self.finding_slug(af.file_idx) == slug)
            .filter(|af| af.finding.file.is_none() && af.finding.plan_step.is_some())
            .collect();
        if mine.is_empty() {
            return Err("no queued answers on this feature".into());
        }
        let plan_path = self.dirs.plan_file(slug);
        let Ok(text) = std::fs::read_to_string(&plan_path) else {
            return Err("no plan.md on disk; run the plan stage first".into());
        };
        let all_sections = crate::spec::sections(&text);
        // Items + briefs share the 1-based numbering the ANSWERS block keys on.
        let mut items: Vec<FixItem> = Vec::new();
        let mut briefs: Vec<(u32, stages::FindingBrief)> = Vec::new();
        for (i, af) in mine.iter().enumerate() {
            let f = &af.finding;
            let step = f.plan_step.as_deref().unwrap_or_default();
            // Step -> line -> containing `##` section. Unlocatable steps fall
            // back to a whole-doc range, degrading the union gate (the apply
            // confirm surfaced that count before we got here).
            let (section, range) = match locate_plan_step(&text, step).and_then(|line| {
                all_sections
                    .iter()
                    .find(|(_, r)| r.contains(&((line - 1) as usize)))
                    .cloned()
            }) {
                Some((name, range)) => (Some(name), range),
                None => (None, 0..text.lines().count()),
            };
            let number = (i + 1) as u32;
            items.push(FixItem {
                findings_path: self.findings[af.file_idx].path.clone(),
                pos: af.pos,
                number,
                section,
                range,
            });
            briefs.push((
                number,
                stages::FindingBrief {
                    title: &f.title,
                    severity: f.severity.label(),
                    scenario: &f.scenario,
                    plan_step: step,
                    snippet: f.snippet.as_deref(),
                },
            ));
        }
        // Prompt-level scope: the deduped section names - unless any anchor
        // was lost, in which case the honest scope is the whole plan.
        let mut section_names: Vec<&str> = Vec::new();
        let mut any_lost = false;
        for it in &items {
            match &it.section {
                Some(s) if !section_names.contains(&s.as_str()) => section_names.push(s),
                Some(_) => {}
                None => any_lost = true,
            }
        }
        if any_lost {
            section_names.clear();
        }
        let spec_path = self.dirs.spec_file(slug);
        let spec_path = spec_path.exists().then_some(spec_path);
        let invariants = stages::meaningful_invariants(&self.dirs);
        let cmd = stages::findings_batch_fix_command(
            &self.cfg,
            &plan_path,
            &section_names,
            &briefs,
            spec_path.as_deref(),
            invariants.as_deref(),
        );
        let branch = mine
            .first()
            .and_then(|af| self.findings.get(af.file_idx))
            .map(|lf| lf.file.branch.clone())
            .filter(|b| !b.is_empty())
            .unwrap_or_else(|| self.branch.clone());
        // Guards all passed: snapshot for the gate and persist the undo point.
        let _ = std::fs::create_dir_all(self.dirs.feature_dir(slug));
        let _ = crate::undo::push(&self.dirs, slug, stages::DocKind::Plan.label(), &text);
        self.fix_doc_before = text;
        Ok((
            cmd,
            BatchFixCtx {
                slug: slug.to_string(),
                branch,
                plan_path,
                items,
            },
        ))
    }

    /// `y` in the apply confirm: spawn ONE headless claude run answering
    /// every queued finding of this feature (gate + verdicts + `u` reverts).
    fn spawn_findings_apply(&mut self, slug: &str, tx: &mpsc::Sender<AppMsg>) {
        let Some(run_cwd) = self.run_cwd() else {
            self.status_msg = Some(format!("branch '{}' has no checkout", self.branch));
            return;
        };
        let (cmd, ctx) = match self.prepare_findings_apply(slug) {
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
            stdin: cmd.stdin,
            stage: "plan-fix".into(),
            feature: title,
            branch: ctx.branch.clone(),
            redact: self.cfg.redaction,
            repro: None, // scoped doc edit; skip provenance like doc-chat
            cwd: run_cwd,
            wrapper: stages::wrapper_argv(&self.cfg, cmd.mode),
        };
        let run_id = runner::new_run_id("plan-fix");
        if let Err(e) = runner::spawn_detached(&self.dirs, &req, &run_id) {
            self.status_msg = Some(format!("plan-fix failed to start: {e:#}"));
            return;
        }
        self.status_msg = Some(format!(
            "applying {} answer(s) via claude…",
            ctx.items.len()
        ));
        self.last_fix = None; // the new run owns the top of the undo stack
        self.fix_ctx = Some(ctx);
        self.attach_fix_tail(run_id, req.agent, tx);
    }

    /// Follow a plan-fix run to completion. Events are not streamed to the
    /// UI, but the tail watches for the final `Completed` event and carries
    /// its result text home - that is where the ANSWERS block lives. The
    /// sender drops when tail_run returns, ending the watcher loop; the
    /// archive (incl. the result line) is fully flushed before that.
    fn attach_fix_tail(
        &mut self,
        run_id: String,
        agent: runner::AgentKind,
        tx: &mpsc::Sender<AppMsg>,
    ) {
        self.current_fix_run_id = Some(run_id.clone());
        self.fix_task = Some(self.attach_result_tail(run_id, agent, tx, AppMsg::FixExited));
    }

    /// Follow a detached run to completion off the event loop, capturing its
    /// final result text (the ANSWERS/REVIEW block source) + last-words tail,
    /// then send `mk(outcome, tail)`. Shared by the plan-fix and the three
    /// legs of the code-fix pipeline. The caller stores the returned handle and
    /// the run id on the fields it owns.
    fn attach_result_tail(
        &self,
        run_id: String,
        agent: runner::AgentKind,
        tx: &mpsc::Sender<AppMsg>,
        mk: fn(Box<Result<RunOutcome>>, FixTail) -> AppMsg,
    ) -> JoinHandle<()> {
        let dirs = self.dirs.clone();
        let tx_done = tx.clone();
        tokio::spawn(async move {
            let (etx, mut erx) = mpsc::channel::<AgentEvent>(256);
            let watch = tokio::spawn(async move {
                let mut tail = FixTail::default();
                while let Some(ev) = erx.recv().await {
                    match ev {
                        AgentEvent::Completed { result_text, .. } => {
                            tail.result_text = result_text; // last Completed wins
                        }
                        AgentEvent::Text { text } if !text.trim().is_empty() => {
                            let t = text.trim();
                            let skip = t.chars().count().saturating_sub(200);
                            tail.last_text = Some(t.chars().skip(skip).collect());
                        }
                        _ => {}
                    }
                }
                tail
            });
            let outcome = runner::tail_run(&dirs, agent, &run_id, etx).await;
            let tail = watch.await.unwrap_or_default();
            let _ = tx_done.send(mk(Box::new(outcome), tail)).await;
        })
    }

    /// The batch fix finished: enforce the union gate, then honor the
    /// per-finding ANSWERS verdicts - FIXED auto-marks, DECLINED returns the
    /// finding to triage with the reason. A leak reverts EVERYTHING and the
    /// whole queue survives.
    fn on_fix_exited(
        &mut self,
        outcome: Result<RunOutcome>,
        tail: FixTail,
        tx: &mpsc::Sender<AppMsg>,
    ) {
        self.fix_task = None;
        let run_id = self.current_fix_run_id.take().unwrap_or_default();
        let Some(ctx) = self.fix_ctx.take() else {
            return;
        };
        self.finding_detail = false;
        let plan_label = stages::DocKind::Plan.label();
        let content = std::fs::read_to_string(&ctx.plan_path).unwrap_or_default();
        let changed = content != self.fix_doc_before;
        let n = ctx.items.len();
        match outcome {
            Err(e) if changed => {
                let _ = crate::undo::undo(&self.dirs, &ctx.slug, plan_label, &ctx.plan_path);
                self.reload_artifacts();
                self.status_msg = Some(format!("plan-fix failed mid-edit; reverted ({e:#})"));
            }
            Err(e) => self.status_msg = Some(format!("plan-fix failed: {e:#}")),
            Ok(o) if !o.meta.ok => {
                let mut reason = crate::history::decode_failure(&o.meta);
                // Nothing recorded at all? The agent's last words are the
                // only context there is.
                if reason.starts_with("agent reported failure")
                    && let Some(last) = tail.last_text.as_deref()
                {
                    reason = format!("{reason} · last: \"{last}\"");
                }
                if changed {
                    let _ = crate::undo::undo(&self.dirs, &ctx.slug, plan_label, &ctx.plan_path);
                    self.reload_artifacts();
                    self.status_msg =
                        Some(format!("plan-fix failed mid-edit; reverted - {reason}"));
                } else {
                    self.status_msg = Some(format!(
                        "plan-fix failed: {reason} · ritual attach {run_id}"
                    ));
                }
                crate::notify::notify(
                    self.cfg.notifications,
                    "ritual: plan-fix failed",
                    &format!("{}: {reason}", ctx.branch),
                );
            }
            Ok(o) => {
                let cost = o.meta.total_cost_usd.unwrap_or(0.0);
                // Gate the batch by heading structure (closes the decoy bypass
                // AND reports which queued section actually moved). When an
                // anchor was lost a finding carries no section, so fall back to
                // the positional gate (degenerate whole-doc) and the global
                // `changed` flag, exactly as the apply confirm warned.
                let any_anchor_lost = ctx.items.iter().any(|i| i.section.is_none());
                let gate: Option<(usize, usize, Option<Vec<String>>)> = if any_anchor_lost {
                    let ranges: Vec<std::ops::Range<usize>> =
                        ctx.items.iter().map(|i| i.range.clone()).collect();
                    crate::spec::edits_confined_multi(&self.fix_doc_before, &content, &ranges)
                        .map(|(a, r)| (a, r, None))
                } else {
                    let queued: Vec<String> =
                        ctx.items.iter().filter_map(|i| i.section.clone()).collect();
                    crate::spec::confine_by_heading(&self.fix_doc_before, &content, &queued)
                        .map(|rep| (rep.added, rep.removed, Some(rep.changed)))
                };
                match gate {
                    None => {
                        let _ =
                            crate::undo::undo(&self.dirs, &ctx.slug, plan_label, &ctx.plan_path);
                        self.reload_artifacts();
                        self.status_msg = Some(format!(
                            "reverted: batch fix leaked outside the queued sections; {n} stay queued"
                        ));
                        crate::notify::notify(
                            self.cfg.notifications,
                            "ritual: plan-fix reverted",
                            &format!("{}: leaked edit rolled back", ctx.branch),
                        );
                    }
                    Some((added, removed, changed_titles)) => {
                        self.reload_artifacts();
                        let verdicts = tail
                            .result_text
                            .as_deref()
                            .map(crate::answers::parse_answers)
                            .unwrap_or_default();
                        let mut fixed_list: Vec<(std::path::PathBuf, usize)> = Vec::new();
                        let mut declined = 0usize;
                        for item in &ctx.items {
                            // Re-find the findings file by PATH: reload shifted
                            // indices, but the run can't rewrite findings JSON
                            // (tool lock) and set_action/set_answer never
                            // reorder, so path+pos stay stable.
                            let Some(i) = self
                                .findings
                                .iter()
                                .position(|lf| lf.path == item.findings_path)
                            else {
                                continue;
                            };
                            // A mid-run f/d by the user wins over any verdict.
                            if self.findings[i]
                                .file
                                .findings
                                .get(item.pos)
                                .is_none_or(|f| f.resolved())
                            {
                                continue;
                            }
                            // A `#n: FIXED` is honored only if the finding's OWN
                            // section actually moved - not merely that the plan
                            // changed somewhere - so an over-claim on an
                            // untouched section is downgraded to declined.
                            let section_moved = match (&changed_titles, &item.section) {
                                (Some(titles), Some(t)) => titles.contains(t),
                                _ => changed, // degenerate / anchor-lost: global flag
                            };
                            let verdict = match verdicts.get(&item.number) {
                                Some(crate::answers::AnswerVerdict::Fixed) if section_moved => {
                                    crate::answers::AnswerVerdict::Fixed
                                }
                                Some(crate::answers::AnswerVerdict::Fixed) if !changed => {
                                    crate::answers::AnswerVerdict::Declined(
                                        "claimed fixed but made no edit".into(),
                                    )
                                }
                                Some(crate::answers::AnswerVerdict::Fixed) => {
                                    crate::answers::AnswerVerdict::Declined(
                                        "claimed fixed but its section was unchanged".into(),
                                    )
                                }
                                Some(v) => v.clone(),
                                None => crate::answers::AnswerVerdict::Declined(
                                    "run gave no verdict".into(),
                                ),
                            };
                            match verdict {
                                crate::answers::AnswerVerdict::Fixed => {
                                    let _ = crate::findings::set_action(
                                        &mut self.findings,
                                        i,
                                        item.pos,
                                        "fixed",
                                    );
                                    let _ = crate::findings::set_answer(
                                        &mut self.findings,
                                        i,
                                        item.pos,
                                        None,
                                        None,
                                    );
                                    fixed_list.push((item.findings_path.clone(), item.pos));
                                }
                                crate::answers::AnswerVerdict::Declined(reason) => {
                                    let _ = crate::findings::set_answer(
                                        &mut self.findings,
                                        i,
                                        item.pos,
                                        None,
                                        Some(&reason),
                                    );
                                    declined += 1;
                                }
                            }
                        }
                        self.clamp_selected_finding();
                        let f = fixed_list.len();
                        self.status_msg = Some(if changed {
                            format!(
                                "✓ plan rewritten (+{added}/−{removed}) · {f} fixed · {declined} declined · ${cost:.3} · u reverts"
                            )
                        } else {
                            format!("plan unchanged · {declined} declined · ${cost:.3}")
                        });
                        crate::notify::notify(
                            self.cfg.notifications,
                            "ritual: plan-fix done",
                            &format!("{}: {f} fixed · {declined} declined", ctx.branch),
                        );
                        if !fixed_list.is_empty() || changed {
                            self.last_fix = Some(LastBatch {
                                slug: ctx.slug.clone(),
                                plan_path: ctx.plan_path.clone(),
                                fixed: fixed_list,
                            });
                        }
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

    /// Build the code-fix batch: every queued CODE finding of this feature,
    /// numbered, with a git snapshot taken so a failed gate can auto-revert.
    /// No plan snapshot / undo::push - a passing code fix stays in the tree.
    fn prepare_code_fix_apply(
        &mut self,
        slug: &str,
    ) -> Result<(stages::StageCommand, CodeFixCtx), String> {
        if self.fix_running() {
            return Err("a fix is already running".into());
        }
        if self.chat_running() {
            return Err("a chat edit is in flight; wait for it to finish".into());
        }
        if let Some((spent, budget)) = crate::run_cmd::budget_exceeded(&self.cfg, &self.dirs) {
            return Err(format!(
                "daily budget reached (${spent:.2}/${budget:.2}); raise budget_daily_usd to override"
            ));
        }
        let Some(run_cwd) = self.run_cwd() else {
            return Err(format!("branch '{}' has no checkout", self.branch));
        };
        let queued = self.queued_auto();
        let mine: Vec<_> = queued
            .iter()
            .filter(|af| self.finding_slug(af.file_idx) == slug)
            .filter(|af| af.finding.file.is_some())
            .collect();
        if mine.is_empty() {
            return Err("no queued code findings on this feature".into());
        }
        // Hash the findings' target files explicitly: a GITIGNORED target's
        // edit is invisible to both the tracked diff and the untracked listing.
        let targets: Vec<std::path::PathBuf> = mine
            .iter()
            .filter_map(|af| af.finding.file.as_deref())
            .map(std::path::PathBuf::from)
            .collect();
        let snap = crate::git::snapshot(&run_cwd, &targets)
            .map_err(|e| format!("git snapshot failed: {e:#}"))?;
        let mut items: Vec<CodeFixItem> = Vec::new();
        let mut owned: Vec<OwnedCodeBrief> = Vec::new();
        for (i, af) in mine.iter().enumerate() {
            let f = &af.finding;
            let number = (i + 1) as u32;
            items.push(CodeFixItem {
                findings_path: self.findings[af.file_idx].path.clone(),
                pos: af.pos,
                number,
            });
            owned.push(OwnedCodeBrief {
                number,
                title: f.title.clone(),
                severity: f.severity.label().to_string(),
                scenario: f.scenario.clone(),
                file: f.file.clone().unwrap_or_default(),
                line: f.line,
                snippet: f.snippet.clone(),
            });
        }
        let invariants = stages::meaningful_invariants(&self.dirs);
        let cmd = {
            let briefs: Vec<(u32, stages::CodeFindingBrief)> = owned
                .iter()
                .map(|b| {
                    (
                        b.number,
                        stages::CodeFindingBrief {
                            title: &b.title,
                            severity: &b.severity,
                            scenario: &b.scenario,
                            file: &b.file,
                            line: b.line,
                            snippet: b.snippet.as_deref(),
                        },
                    )
                })
                .collect();
            stages::findings_code_fix_command(&self.cfg, &briefs, invariants.as_deref())
        };
        let branch = mine
            .first()
            .and_then(|af| self.findings.get(af.file_idx))
            .map(|lf| lf.file.branch.clone())
            .filter(|b| !b.is_empty())
            .unwrap_or_else(|| self.branch.clone());
        let numbers = items.iter().map(|it| it.number).collect();
        Ok((
            cmd,
            CodeFixCtx {
                branch,
                run_cwd,
                snap,
                items,
                numbers,
                briefs: owned,
                phase: crate::code_fix::CodePhase::Fixing,
                answers: std::collections::HashMap::new(),
                fix_change: None,
            },
        ))
    }

    /// Spawn leg 1: one headless claude run fixing all queued code findings.
    fn spawn_code_fix(&mut self, slug: &str, tx: &mpsc::Sender<AppMsg>) {
        let (cmd, ctx) = match self.prepare_code_fix_apply(slug) {
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
            stdin: cmd.stdin,
            stage: "code-fix".into(),
            feature: title,
            branch: ctx.branch.clone(),
            redact: self.cfg.redaction,
            repro: None,
            cwd: ctx.run_cwd.clone(),
            wrapper: stages::wrapper_argv(&self.cfg, cmd.mode),
        };
        let run_id = runner::new_run_id("code-fix");
        if let Err(e) = runner::spawn_detached(&self.dirs, &req, &run_id) {
            self.status_msg = Some(format!("code-fix failed to start: {e:#}"));
            return;
        }
        self.status_msg = Some(format!(
            "fixing {} code finding(s) via claude…",
            ctx.items.len()
        ));
        self.current_code_run_id = Some(run_id.clone());
        let task = self.attach_result_tail(run_id, req.agent, tx, AppMsg::CodeFixExited);
        self.code_fix_ctx = Some(ctx);
        self.code_fix_task = Some(task);
    }

    /// Leg 1 done → run the check.sh gate, or revert.
    fn on_code_fix_exited(
        &mut self,
        outcome: Result<RunOutcome>,
        tail: FixTail,
        tx: &mpsc::Sender<AppMsg>,
    ) {
        self.code_fix_task = None;
        let run_id = self.current_code_run_id.take().unwrap_or_default();
        let Some(mut ctx) = self.code_fix_ctx.take() else {
            return;
        };
        self.finding_detail = false;
        let answers = tail
            .result_text
            .as_deref()
            .map(crate::answers::parse_answers)
            .unwrap_or_default();
        let event = match outcome {
            Err(e) => crate::code_fix::GateEvent::FixFailed(format!("{e:#}")),
            Ok(o) if !o.meta.ok => {
                let mut reason = crate::history::decode_failure(&o.meta);
                if reason.starts_with("agent reported failure")
                    && let Some(last) = tail.last_text.as_deref()
                {
                    reason = format!("{reason} · last: \"{last}\"");
                }
                crate::code_fix::GateEvent::FixFailed(reason)
            }
            Ok(_) => crate::code_fix::GateEvent::FixOk {
                answers: answers.clone(),
            },
        };
        ctx.answers = answers;
        let numbers = ctx.numbers.clone();
        let (_next, step) = crate::code_fix::advance(
            crate::code_fix::CodePhase::Fixing,
            event,
            &numbers,
            &ctx.answers,
        );
        match step {
            crate::code_fix::Step::SpawnCheck => {
                // Fail fast, before spending a full check.sh run: the fixer must
                // not have moved HEAD (a rogue commit/reset the guardrail
                // forbids), and it must have produced an OBSERVABLE change -
                // content-hashed, so an edit to an untracked target still
                // counts. This restores the "changed nothing" guard v0.10.1 had
                // to drop, now correct on untracked code.
                if crate::git::head_moved(&ctx.run_cwd, &ctx.snap) {
                    self.fail_code_batch(
                        ctx,
                        "the fix agent moved HEAD (commit/reset/checkout); inspect with git reflog",
                        Some(&run_id),
                        tx,
                    );
                    return;
                }
                // Fail CLOSED on a git error: an unverifiable change must never
                // slide through as "there was a change" with an empty review
                // diff. Capture the render HERE, before check.sh - artifacts
                // the check writes must not pollute the review evidence.
                match crate::git::observed_change(&ctx.run_cwd, &ctx.snap) {
                    Err(e) => {
                        let reason = format!("git could not verify the fix: {e:#}");
                        self.fail_code_batch(ctx, &reason, Some(&run_id), tx);
                        return;
                    }
                    Ok(c) if c.is_empty() => {
                        self.fail_code_batch(
                            ctx,
                            "fix produced no observable change",
                            Some(&run_id),
                            tx,
                        );
                        return;
                    }
                    Ok(c) => ctx.fix_change = Some(c.render()),
                }
                if !ctx.run_cwd.join("check.sh").exists() {
                    let reason = format!(
                        "no check.sh in {}; the code-fix gate needs one",
                        ctx.run_cwd.display()
                    );
                    self.fail_code_batch(ctx, &reason, None, tx);
                    return;
                }
                ctx.phase = crate::code_fix::CodePhase::Checking;
                let run_cwd = ctx.run_cwd.clone();
                self.status_msg = Some("code fix applied; verifying with check.sh…".into());
                self.code_fix_ctx = Some(ctx);
                self.gate_check_running = true;
                self.spawn_gate_check(run_cwd, tx);
            }
            crate::code_fix::Step::Revert(reason) => {
                self.fail_code_batch(ctx, &reason, Some(&run_id), tx)
            }
            _ => self.fail_code_batch(ctx, "internal: unexpected step after fix", None, tx),
        }
    }

    /// Leg 2: run `./check.sh` (full) in the checkout, off the event loop.
    fn spawn_gate_check(&self, run_cwd: std::path::PathBuf, tx: &mpsc::Sender<AppMsg>) {
        let timeout = self.cfg.check_timeout_secs;
        let tx = tx.clone();
        tokio::task::spawn_blocking(move || {
            let (ok, tail) = match tempfile::NamedTempFile::new() {
                Ok(log) => {
                    let mut cmd = std::process::Command::new("./check.sh");
                    cmd.current_dir(&run_cwd)
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
            let _ = tx.blocking_send(AppMsg::CodeGateDone { ok, tail });
        });
    }

    /// Leg 2 done → spawn the read-only re-review, or revert on a red check.
    fn on_code_gate_done(&mut self, ok: bool, tail: String, tx: &mpsc::Sender<AppMsg>) {
        // Always clear first, even if the batch was cancelled mid-check: this
        // message means the (possibly orphaned) gate check.sh has finished, so
        // `run_check` may launch again.
        self.gate_check_running = false;
        let Some(mut ctx) = self.code_fix_ctx.take() else {
            return;
        };
        let event = if ok {
            crate::code_fix::GateEvent::CheckGreen
        } else {
            crate::code_fix::GateEvent::CheckRed(tail)
        };
        let numbers = ctx.numbers.clone();
        let (_next, step) = crate::code_fix::advance(
            crate::code_fix::CodePhase::Checking,
            event,
            &numbers,
            &ctx.answers,
        );
        match step {
            crate::code_fix::Step::SpawnReview => {
                ctx.phase = crate::code_fix::CodePhase::Reviewing;
                // The change set captured right after the fix run: content-
                // hashed (untracked/ignored targets included) and free of
                // check.sh artifacts. Never hand the reviewer an empty diff -
                // a missing capture fails the batch instead.
                let Some(diff) = ctx.fix_change.take() else {
                    self.fail_code_batch(ctx, "internal: fix change set missing", None, tx);
                    return;
                };
                let cmd = {
                    let briefs: Vec<(u32, stages::CodeFindingBrief)> = ctx
                        .briefs
                        .iter()
                        .map(|b| {
                            (
                                b.number,
                                stages::CodeFindingBrief {
                                    title: &b.title,
                                    severity: &b.severity,
                                    scenario: &b.scenario,
                                    file: &b.file,
                                    line: b.line,
                                    snippet: b.snippet.as_deref(),
                                },
                            )
                        })
                        .collect();
                    stages::code_fix_review_command(&self.cfg, &diff, &briefs)
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
                    stdin: cmd.stdin,
                    stage: "code-fix-review".into(),
                    feature: title,
                    branch: ctx.branch.clone(),
                    redact: self.cfg.redaction,
                    repro: None,
                    cwd: ctx.run_cwd.clone(),
                    wrapper: stages::wrapper_argv(&self.cfg, cmd.mode),
                };
                let run_id = runner::new_run_id("code-fix-review");
                if let Err(e) = runner::spawn_detached(&self.dirs, &req, &run_id) {
                    self.fail_code_batch(
                        ctx,
                        &format!("re-review failed to start: {e:#}"),
                        None,
                        tx,
                    );
                    return;
                }
                self.status_msg = Some("check.sh green; re-reviewing the fix…".into());
                self.current_code_run_id = Some(run_id.clone());
                let task = self.attach_result_tail(run_id, req.agent, tx, AppMsg::CodeReviewExited);
                self.code_fix_ctx = Some(ctx);
                self.code_fix_task = Some(task);
            }
            // check.sh red: no agent run to attach; the tail IS the diagnosis.
            crate::code_fix::Step::Revert(reason) => self.fail_code_batch(ctx, &reason, None, tx),
            _ => self.fail_code_batch(ctx, "internal: unexpected step after check", None, tx),
        }
    }

    /// Leg 3 done → the strict decision: accept the whole batch or revert it.
    fn on_code_review_exited(
        &mut self,
        outcome: Result<RunOutcome>,
        tail: FixTail,
        tx: &mpsc::Sender<AppMsg>,
    ) {
        self.code_fix_task = None;
        let run_id = self.current_code_run_id.take().unwrap_or_default();
        let Some(ctx) = self.code_fix_ctx.take() else {
            return;
        };
        // Parse the verdict once so the accept path can annotate the findings
        // it does NOT accept with the reviewer's reason.
        let review = match &outcome {
            Ok(o) if o.meta.ok => tail
                .result_text
                .as_deref()
                .map(crate::review::parse_review)
                .unwrap_or_default(),
            _ => crate::review::ReviewVerdict::default(),
        };
        let event = match &outcome {
            Ok(o) if o.meta.ok => crate::code_fix::GateEvent::ReviewOk(review.clone()),
            Ok(o) => {
                crate::code_fix::GateEvent::ReviewFailed(crate::history::decode_failure(&o.meta))
            }
            Err(e) => crate::code_fix::GateEvent::ReviewFailed(format!("{e:#}")),
        };
        let numbers = ctx.numbers.clone();
        let (_next, step) = crate::code_fix::advance(
            crate::code_fix::CodePhase::Reviewing,
            event,
            &numbers,
            &ctx.answers,
        );
        match step {
            crate::code_fix::Step::Accept(nums) => self.accept_code_batch(ctx, &nums, &review, tx),
            crate::code_fix::Step::Revert(reason) => {
                self.fail_code_batch(ctx, &reason, Some(&run_id), tx)
            }
            _ => self.fail_code_batch(ctx, "internal: unexpected step after review", None, tx),
        }
    }

    /// Both gates passed: mark the fixed findings, leave the diff in the tree.
    fn accept_code_batch(
        &mut self,
        ctx: CodeFixCtx,
        nums: &[u32],
        review: &crate::review::ReviewVerdict,
        tx: &mpsc::Sender<AppMsg>,
    ) {
        self.reload_artifacts();
        let mut fixed = 0usize;
        let mut requeued = 0usize;
        for item in &ctx.items {
            let Some(i) = self
                .findings
                .iter()
                .position(|lf| lf.path == item.findings_path)
            else {
                continue;
            };
            // A mid-run f/d by the user wins.
            if self.findings[i]
                .file
                .findings
                .get(item.pos)
                .is_none_or(|f| f.resolved())
            {
                continue;
            }
            if nums.contains(&item.number) {
                let _ = crate::findings::set_action(&mut self.findings, i, item.pos, "fixed");
                let _ = crate::findings::set_answer(&mut self.findings, i, item.pos, None, None);
                fixed += 1;
            } else {
                // Not confirmed resolved for THIS finding: it stays queued (⚑A,
                // re-runs on the next apply over the same left-in-tree diff) with
                // the reviewer's reason attached so the user knows why.
                let reason = match review.per_finding.get(&item.number) {
                    Some(crate::review::FindingReview::Unresolved(r)) => r.clone(),
                    _ => match ctx.answers.get(&item.number) {
                        Some(crate::answers::AnswerVerdict::Declined(r)) => r.clone(),
                        _ => "not confirmed resolved".to_string(),
                    },
                };
                let _ = crate::findings::set_answer(
                    &mut self.findings,
                    i,
                    item.pos,
                    Some("auto"),
                    Some(&reason),
                );
                requeued += 1;
            }
        }
        self.clamp_selected_finding();
        self.status_msg = Some(format!(
            "✓ {fixed} code finding(s) fixed, {requeued} requeued; changes are in your working tree - review with git"
        ));
        crate::notify::notify(
            self.cfg.notifications,
            "ritual: code-fix done",
            &format!(
                "{}: {fixed} fixed, {requeued} requeued (check.sh + re-review passed)",
                ctx.branch
            ),
        );
        self.drain_pending_chat(tx);
    }

    /// A leg failed. The attempt is LEFT in the working tree (never deleted -
    /// git is the undo, and auto-restore is unreliable when the target code is
    /// untracked); the findings stay queued. Surface WHY, an `ritual attach`
    /// hint to replay the run, and how to keep/discard with git.
    fn fail_code_batch(
        &mut self,
        ctx: CodeFixCtx,
        reason: &str,
        attach: Option<&str>,
        tx: &mpsc::Sender<AppMsg>,
    ) {
        self.reload_artifacts();
        let n = ctx.items.len();
        let attach_hint = match attach {
            Some(id) if !id.is_empty() => format!(" · ritual attach {id}"),
            _ => String::new(),
        };
        self.status_msg = Some(format!(
            "code-fix failed: {reason}{attach_hint} · the attempt is in your working tree (git diff to review, git restore . / git stash to discard) · {n} stay queued"
        ));
        crate::notify::notify(
            self.cfg.notifications,
            "ritual: code-fix failed",
            &format!(
                "{}: {reason} - attempt left in the working tree",
                ctx.branch
            ),
        );
        self.drain_pending_chat(tx);
    }

    /// Send the next queued chat message once no fix/chat holds the agent.
    fn drain_pending_chat(&mut self, tx: &mpsc::Sender<AppMsg>) {
        if !self.chat_running()
            && let Some(msg) = self.chat.as_mut().and_then(|c| c.pending.pop_front())
        {
            if let Some(chat) = self.chat.as_mut() {
                chat.transcript.push(ChatTurn::User(msg.clone()));
            }
            self.spawn_doc_chat(msg, tx);
        }
    }

    /// `u`: revert the last APPLIED batch - one undo restores the plan, its
    /// FIXED findings reopen and requeue (⚑A) for another round.
    /// `reset-plan` confirmed: delete plan.md, reset the plan-derived stages,
    /// and clear the plan-review/coverage findings + plan undo stack. Code and
    /// git untouched; re-run the plan stage to start fresh from the spec.
    fn do_reset_plan(&mut self) {
        self.reset_plan_confirm = false;
        let sum = crate::reset::reset_plan(&self.dirs, &mut self.state, &self.branch);
        let _ = self.state.save(&self.dirs);
        self.last_fix = None; // its plan snapshot is gone
        self.reload_artifacts();
        self.clamp_selected_finding();
        self.status_msg = Some(format!(
            "plan reset to spec: plan.md {}, {} stage(s) reset, {} plan finding(s) cleared - re-run plan",
            if sum.plan_deleted {
                "deleted"
            } else {
                "absent"
            },
            sum.stages_reset,
            sum.findings_removed,
        ));
    }

    fn doc_undo(&mut self) {
        if self.fix_running() || self.chat_running() {
            self.status_msg = Some("busy: wait for the running edit to finish".into());
            return;
        }
        let Some(lb) = self.last_fix.take() else {
            self.status_msg = Some("no applied batch to revert (chat has Ctrl+Z)".into());
            return;
        };
        match crate::undo::undo(
            &self.dirs,
            &lb.slug,
            stages::DocKind::Plan.label(),
            &lb.plan_path,
        ) {
            Ok(true) => {
                self.reload_artifacts();
                let mut reopened = 0usize;
                for (path, pos) in &lb.fixed {
                    if let Some(i) = self.findings.iter().position(|x| &x.path == path)
                        && self.findings[i]
                            .file
                            .findings
                            .get(*pos)
                            .is_some_and(|f| f.action == "fixed")
                    {
                        // Same-value set_action toggles fixed -> pending; the
                        // answer flag comes back so the queue survives intact.
                        let _ = crate::findings::set_action(&mut self.findings, i, *pos, "fixed");
                        let _ = crate::findings::set_answer(
                            &mut self.findings,
                            i,
                            *pos,
                            Some("auto"),
                            None,
                        );
                        reopened += 1;
                    }
                }
                self.status_msg = Some(format!(
                    "batch reverted; {reopened} finding(s) queued again"
                ));
            }
            Ok(false) => self.status_msg = Some("nothing to undo".into()),
            Err(e) => self.status_msg = Some(format!("undo failed: {e:#}")),
        }
    }
}

/// Best-effort 1-based line in `plan` for a plan-review finding's free-text
/// `step`. Tries, in order: the whole step text, a leading headline like
/// "Step 2", and the ordered-list item ("2." / "2)") that plans number their
/// steps with. None when nothing matches - the caller opens the plan at the top.
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
pub async fn run(
    cfg: Config,
    dirs: RitualDirs,
    theme_flag: Option<String>,
    ascii_flag: bool,
) -> Result<()> {
    anyhow::ensure!(dirs.exists(), "no .ritual/ here; run `ritual init` first");
    let mut term = Term::enter()?;
    let (tx, mut rx) = mpsc::channel::<AppMsg>(512);

    let mut app = App::new(cfg, dirs).context("loading project state")?;
    app.theme_flag = theme_flag;
    app.ascii_flag = ascii_flag;
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
        let plan = "# Plan\n\n### Step 1 - scaffold\ndo thing\n\n### Step 2 - delete\nmore\n";
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

    fn seeded_json(app: &App) -> String {
        std::fs::read_to_string(
            app.dirs
                .findings_dir()
                .join("20260713T000000Z-plan-review.json"),
        )
        .unwrap()
    }

    #[test]
    fn f_key_queues_then_unqueues_plan_finding() {
        let (_t, mut app, tx, _rx) = test_app();
        seed_findings(
            &mut app,
            r#"{"stage":"plan-review","findings":[
                {"id":1,"title":"boom","plan_step":"Step 2","severity":"major","verdict":"confirmed"}]}"#,
        );
        app.dispatch(Action::FindingClaudeFix, &tx);
        assert!(seeded_json(&app).contains(r#""answer": "auto""#));
        assert_eq!(app.queued_auto().len(), 1);
        assert!(app.status_msg.as_deref().unwrap().contains("⚑A queued"));
        // F on a queued finding opens the apply confirm; `u` in it unqueues.
        app.dispatch(Action::FindingClaudeFix, &tx);
        assert!(app.apply_confirm.is_some());
        app.on_input(
            Event::Key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::NONE)),
            &tx,
        );
        assert!(app.apply_confirm.is_none());
        assert!(!seeded_json(&app).contains("answer"));
        assert_eq!(app.queued_auto().len(), 0);
    }

    #[test]
    fn m_routes_a_code_finding_to_the_manual_queue() {
        let (_t, mut app, tx, _rx) = test_app();
        seed_findings(
            &mut app,
            r#"{"stage":"dual-review","findings":[
                {"id":1,"title":"code bug","file":"src/a.rs","line":3,"severity":"major","verdict":"confirmed"}]}"#,
        );
        // m flags a code finding for the human (⚑M); m again clears it.
        let path = app
            .dirs
            .findings_dir()
            .join("20260713T000000Z-plan-review.json");
        app.dispatch(Action::FindingManual, &tx);
        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .contains(r#""answer": "manual""#)
        );
        app.dispatch(Action::FindingManual, &tx);
        assert!(!std::fs::read_to_string(&path).unwrap().contains("answer"));
    }

    #[test]
    fn m_switches_a_queued_auto_finding_to_manual() {
        let (_t, mut app, tx, _rx) = test_app();
        seed_findings(
            &mut app,
            r#"{"stage":"plan-review","findings":[
                {"id":1,"title":"boom","plan_step":"Step 2","severity":"major","verdict":"confirmed"}]}"#,
        );
        app.dispatch(Action::FindingClaudeFix, &tx);
        assert_eq!(app.queued_auto().len(), 1);
        app.dispatch(Action::FindingManual, &tx);
        assert!(seeded_json(&app).contains(r#""answer": "manual""#));
        assert_eq!(app.queued_auto().len(), 0);
    }

    #[test]
    fn d_prompt_commits_with_reason_and_esc_cancels() {
        let (_t, mut app, tx, _rx) = test_app();
        seed_findings(
            &mut app,
            r#"{"stage":"plan-review","findings":[
                {"id":1,"title":"noise","plan_step":"Step 3","severity":"minor","verdict":"confirmed"}]}"#,
        );
        // Esc cancels without writing.
        app.dispatch(Action::FindingDismiss, &tx);
        assert!(app.dismiss_prompt.is_some());
        app.on_input(
            Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
            &tx,
        );
        assert!(app.dismiss_prompt.is_none());
        assert!(!seeded_json(&app).contains("dismissed"));

        // Typed reason persists in the same write.
        app.dispatch(Action::FindingDismiss, &tx);
        for c in "out of scope".chars() {
            app.on_input(
                Event::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)),
                &tx,
            );
        }
        app.on_input(
            Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            &tx,
        );
        let json = seeded_json(&app);
        assert!(json.contains(r#""action": "dismissed""#));
        assert!(json.contains("out of scope"));
        assert!(app.status_msg.as_deref().unwrap().contains("out of scope"));
    }

    #[test]
    fn d_empty_enter_dismisses_plain_and_repeat_toggles_back() {
        let (_t, mut app, tx, _rx) = test_app();
        seed_findings(
            &mut app,
            r#"{"stage":"plan-review","findings":[
                {"id":1,"title":"noise","plan_step":"Step 3","severity":"minor","verdict":"confirmed"}]}"#,
        );
        app.dispatch(Action::FindingDismiss, &tx);
        app.on_input(
            Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            &tx,
        );
        assert!(seeded_json(&app).contains(r#""action": "dismissed""#));
        assert!(!seeded_json(&app).contains("reason"));

        // d on a dismissed finding: NO prompt, plain toggle back to pending.
        app.show_resolved = true;
        app.dispatch(Action::FindingDismiss, &tx);
        assert!(app.dismiss_prompt.is_none());
        assert!(seeded_json(&app).contains(r#""action": "pending""#));
    }

    fn catalog_idx(key: &str) -> usize {
        crate::settings::CATALOG
            .iter()
            .position(|d| d.key == key)
            .expect(key)
    }

    fn press(app: &mut App, tx: &mpsc::Sender<AppMsg>, code: KeyCode) {
        app.on_input(Event::Key(KeyEvent::new(code, KeyModifiers::NONE)), tx);
    }

    #[test]
    fn settings_overlay_opens_navigates_and_closes() {
        let (_t, mut app, tx, _rx) = test_app();
        app.dispatch(Action::Settings, &tx);
        assert!(app.settings.is_some(), "S opens the settings overlay");
        // j/k clamp at both ends.
        press(&mut app, &tx, KeyCode::Char('k'));
        assert_eq!(app.settings.as_ref().unwrap().selected, 0);
        for _ in 0..500 {
            press(&mut app, &tx, KeyCode::Char('j'));
        }
        assert_eq!(
            app.settings.as_ref().unwrap().selected,
            crate::settings::CATALOG.len() - 1
        );
        press(&mut app, &tx, KeyCode::Esc);
        assert!(app.settings.is_none(), "esc closes");
        // S toggles: open again via dispatch, S-as-toggle closes.
        app.dispatch(Action::Settings, &tx);
        app.dispatch(Action::Settings, &tx);
        assert!(app.settings.is_none());
    }

    #[test]
    fn settings_bool_enter_writes_project_config_and_live_applies() {
        let (_t, mut app, tx, _rx) = test_app();
        app.dispatch(Action::Settings, &tx);
        let idx = catalog_idx("notifications");
        app.settings.as_mut().unwrap().selected = idx;
        press(&mut app, &tx, KeyCode::Enter);
        let path = app.dirs.project_root.join(".ritual/config.toml");
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("notifications = false"), "{text}");
        assert!(!app.cfg.notifications, "cfg swapped live");
        assert!(app.status_msg.as_deref().unwrap().contains("(project)"));
        assert_eq!(
            app.settings.as_ref().unwrap().sources[idx],
            "project",
            "source tag refreshed after the write"
        );
    }

    #[test]
    fn settings_enum_cycle_rethemes_live_and_wraps() {
        let (_t, mut app, tx, _rx) = test_app();
        app.dispatch(Action::Settings, &tx);
        app.settings.as_mut().unwrap().selected = catalog_idx("theme");
        press(&mut app, &tx, KeyCode::Enter);
        assert_eq!(app.cfg.theme_name, "tokyonight", "live re-theme");
        let path = app.dirs.project_root.join(".ritual/config.toml");
        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .contains("theme = \"tokyonight\"")
        );
        press(&mut app, &tx, KeyCode::Enter);
        assert_eq!(app.cfg.theme_name, "eldritch", "cycle wraps");
    }

    #[test]
    fn settings_optenum_cycles_through_variants_to_unset() {
        let (_t, mut app, tx, _rx) = test_app();
        app.dispatch(Action::Settings, &tx);
        app.settings.as_mut().unwrap().selected = catalog_idx("effort.plan");
        press(&mut app, &tx, KeyCode::Enter);
        assert_eq!(app.cfg.effort.get("plan").map(String::as_str), Some("low"));
        let path = app.dirs.project_root.join(".ritual/config.toml");
        assert!(std::fs::read_to_string(&path).unwrap().contains("[effort]"));
        // low → medium → high → xhigh → unset.
        for _ in 0..4 {
            press(&mut app, &tx, KeyCode::Enter);
        }
        assert!(
            !app.cfg.effort.contains_key("plan"),
            "cycling past the last variant clears the key"
        );
        assert!(
            !std::fs::read_to_string(&path).unwrap().contains("plan ="),
            "key removed from the project file"
        );
    }

    fn type_str(app: &mut App, tx: &mpsc::Sender<AppMsg>, s: &str) {
        for c in s.chars() {
            press(app, tx, KeyCode::Char(c));
        }
    }

    fn clear_edit_input(app: &mut App, tx: &mpsc::Sender<AppMsg>) {
        let n = app
            .settings
            .as_ref()
            .and_then(|s| s.edit.as_ref())
            .map(|e| e.input.chars().count())
            .unwrap_or(0);
        for _ in 0..n {
            press(app, tx, KeyCode::Backspace);
        }
    }

    #[test]
    fn settings_edit_validates_rejects_then_writes_budget() {
        let (_t, mut app, tx, _rx) = test_app();
        app.dispatch(Action::Settings, &tx);
        app.settings.as_mut().unwrap().selected = catalog_idx("budget_finding_fix_usd");
        press(&mut app, &tx, KeyCode::Enter);
        let edit = app.settings.as_ref().unwrap().edit.clone();
        assert_eq!(
            edit.map(|e| e.input).as_deref(),
            Some("1"),
            "prompt opens prefilled with the effective value"
        );

        clear_edit_input(&mut app, &tx);
        type_str(&mut app, &tx, "abc");
        press(&mut app, &tx, KeyCode::Enter);
        let s = app.settings.as_ref().unwrap();
        assert!(
            s.edit.as_ref().is_some_and(|e| e.error.is_some()),
            "bad input shows an error and keeps the prompt open"
        );
        let path = app.dirs.project_root.join(".ritual/config.toml");
        assert!(!path.exists(), "nothing written on validation failure");

        clear_edit_input(&mut app, &tx);
        type_str(&mut app, &tx, "4.5");
        press(&mut app, &tx, KeyCode::Enter);
        assert!(
            app.settings.as_ref().unwrap().edit.is_none(),
            "valid input closes the prompt"
        );
        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .contains("budget_finding_fix_usd = 4.5")
        );
        assert_eq!(app.cfg.budget_finding_fix_usd, 4.5);
    }

    #[test]
    fn settings_edit_empty_clears_optional_key() {
        let (_t, mut app, tx, _rx) = test_app();
        app.dispatch(Action::Settings, &tx);
        app.settings.as_mut().unwrap().selected = catalog_idx("budget_daily_usd");
        press(&mut app, &tx, KeyCode::Enter);
        type_str(&mut app, &tx, "9");
        press(&mut app, &tx, KeyCode::Enter);
        assert_eq!(app.cfg.budget_daily_usd, Some(9.0));

        press(&mut app, &tx, KeyCode::Enter); // reopen, prefilled "9"
        clear_edit_input(&mut app, &tx);
        press(&mut app, &tx, KeyCode::Enter); // empty = clear
        assert_eq!(app.cfg.budget_daily_usd, None);
        let path = app.dirs.project_root.join(".ritual/config.toml");
        assert!(
            !std::fs::read_to_string(&path)
                .unwrap()
                .contains("budget_daily_usd")
        );
        assert!(app.status_msg.as_deref().unwrap().contains("cleared"));
    }

    #[test]
    fn settings_edit_models_and_timeout_types() {
        let (_t, mut app, tx, _rx) = test_app();
        app.dispatch(Action::Settings, &tx);
        let path = app.dirs.project_root.join(".ritual/config.toml");

        app.settings.as_mut().unwrap().selected = catalog_idx("models.plan");
        press(&mut app, &tx, KeyCode::Enter);
        type_str(&mut app, &tx, "opus");
        press(&mut app, &tx, KeyCode::Enter);
        let text = std::fs::read_to_string(&path).unwrap();
        let header = text.find("[models]").expect("header table");
        assert!(text[header..].contains("plan = \"opus\""));
        assert_eq!(app.cfg.models.get("plan").map(String::as_str), Some("opus"));

        press(&mut app, &tx, KeyCode::Enter); // reopen prefilled "opus"
        clear_edit_input(&mut app, &tx);
        press(&mut app, &tx, KeyCode::Enter); // empty clears
        assert!(!app.cfg.models.contains_key("plan"));

        app.settings.as_mut().unwrap().selected = catalog_idx("check_timeout_secs");
        press(&mut app, &tx, KeyCode::Enter);
        clear_edit_input(&mut app, &tx);
        type_str(&mut app, &tx, "0");
        press(&mut app, &tx, KeyCode::Enter);
        assert!(
            app.settings
                .as_ref()
                .unwrap()
                .edit
                .as_ref()
                .is_some_and(|e| e.error.is_some()),
            "0 seconds rejected"
        );
        clear_edit_input(&mut app, &tx);
        type_str(&mut app, &tx, "900");
        press(&mut app, &tx, KeyCode::Enter);
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.contains("check_timeout_secs = 900"),
            "written as a TOML integer: {text}"
        );
        assert!(!text.contains("900.0"));
    }

    #[test]
    fn settings_edit_required_text_rejects_empty_and_esc_keeps_overlay() {
        let (_t, mut app, tx, _rx) = test_app();
        app.dispatch(Action::Settings, &tx);
        app.settings.as_mut().unwrap().selected = catalog_idx("base_ref");
        press(&mut app, &tx, KeyCode::Enter);
        clear_edit_input(&mut app, &tx);
        press(&mut app, &tx, KeyCode::Enter);
        assert!(
            app.settings
                .as_ref()
                .unwrap()
                .edit
                .as_ref()
                .is_some_and(|e| e.error.is_some()),
            "required value rejects empty"
        );
        press(&mut app, &tx, KeyCode::Esc);
        let s = app.settings.as_ref().unwrap();
        assert!(s.edit.is_none(), "esc drops the edit line");
        assert!(app.settings.is_some(), "…but the overlay stays");
    }

    #[test]
    fn settings_apply_reverts_file_when_reload_fails() {
        let (_t, mut app, tx, _rx) = test_app();
        let path = app.dirs.project_root.join(".ritual/config.toml");
        std::fs::write(&path, "# keep me\nbase_ref = \"develop\"\n").unwrap();
        app.dispatch(Action::Settings, &tx);
        let before = std::fs::read_to_string(&path).unwrap();
        // Bypass validate() to hit the transaction's revert path: the write
        // lands, Config::load rejects the unknown theme, bytes come back.
        app.apply_setting(
            catalog_idx("theme"),
            Some(crate::settings::SettingValue::Str("no-such-theme".into())),
        );
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            before,
            "file restored byte-exact"
        );
        assert_eq!(app.cfg.theme_name, "eldritch", "cfg unchanged");
        assert!(app.status_msg.as_deref().unwrap().contains("reverted"));
    }

    #[test]
    fn detail_overlay_routes_triage_keys() {
        let (_t, mut app, tx, _rx) = test_app();
        seed_findings(
            &mut app,
            r#"{"stage":"plan-review","findings":[
                {"id":1,"title":"boom","plan_step":"Step 2","severity":"major","verdict":"confirmed"}]}"#,
        );
        app.dispatch(Action::Confirm, &tx);
        assert!(app.finding_detail);
        // F queues from inside the overlay; overlay stays open.
        app.on_input(
            Event::Key(KeyEvent::new(KeyCode::Char('F'), KeyModifiers::SHIFT)),
            &tx,
        );
        assert!(app.finding_detail);
        assert_eq!(app.queued_auto().len(), 1);
        // d opens the prompt ABOVE the overlay; committing closes both.
        app.on_input(
            Event::Key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE)),
            &tx,
        );
        assert!(app.dismiss_prompt.is_some());
        app.on_input(
            Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            &tx,
        );
        assert!(!app.finding_detail);
        assert!(seeded_json(&app).contains(r#""action": "dismissed""#));
    }

    const TRIAGE_SEED: &str = r#"{"stage":"plan-review","findings":[
        {"id":1,"title":"prose resolved","plan_step":"Step 1","severity":"major",
         "verdict":"accepted","action":"Resolved by narrowing the scope."},
        {"id":2,"title":"retracted","plan_step":"Step 2","severity":"minor","verdict":"refuted"},
        {"id":3,"title":"plan gap","plan_step":"Step 3","severity":"major","verdict":"confirmed"},
        {"id":4,"title":"code bug","file":"src/a.rs","line":3,"severity":"major","verdict":"confirmed"},
        {"id":5,"title":"maybe","plan_step":"Step 4","severity":"minor","verdict":"unconfirmed"},
        {"id":6,"title":"already queued","plan_step":"Step 5","severity":"minor",
         "verdict":"confirmed","answer":"manual"}]}"#;

    #[test]
    fn triage_all_counts_and_applies_to_disk() {
        let (_t, mut app, tx, _rx) = test_app();
        seed_findings(&mut app, TRIAGE_SEED);
        app.dispatch(Action::TriageAll, &tx);
        let c = app.triage_confirm.as_ref().expect("modal open");
        assert_eq!(
            (
                c.archive,
                c.queue_auto,
                c.queue_manual,
                c.dismiss,
                c.needs_you
            ),
            (1, 1, 1, 1, 1)
        );
        app.on_input(
            Event::Key(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE)),
            &tx,
        );
        assert!(app.triage_confirm.is_none());
        let json = seeded_json(&app);
        // Archive: fixed + prose preserved as reason.
        assert!(json.contains(r#""action": "fixed""#), "{json}");
        assert!(json.contains("Resolved by narrowing the scope."), "{json}");
        // Refuted: dismissed with the rule's verdict-specific reason.
        assert!(json.contains(r#""action": "dismissed""#), "{json}");
        assert!(json.contains("refuted by review"), "{json}");
        // Confirmed plan -> ⚑A; confirmed code -> ⚑M (plus the pre-queued one).
        assert!(json.contains(r#""answer": "auto""#), "{json}");
        assert_eq!(json.matches(r#""answer": "manual""#).count(), 2, "{json}");
        // Unconfirmed untouched.
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        let maybe = &parsed["findings"][4];
        assert_eq!(maybe["action"], "".to_string(), "untouched as seeded");
        assert!(maybe.get("answer").is_none());
        assert!(
            app.status_msg
                .as_deref()
                .is_some_and(|m| m.contains("1 archived · 1 ⚑A · 1 ⚑M · 1 dismissed · 1 need you"))
        );
    }

    #[test]
    fn triage_all_esc_writes_nothing() {
        let (_t, mut app, tx, _rx) = test_app();
        seed_findings(&mut app, TRIAGE_SEED);
        let before = seeded_json(&app);
        app.dispatch(Action::TriageAll, &tx);
        app.on_input(
            Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
            &tx,
        );
        assert!(app.triage_confirm.is_none());
        assert_eq!(seeded_json(&app), before, "esc must write nothing");
    }

    #[test]
    fn triage_all_respects_filter_scope() {
        let (_t, mut app, tx, _rx) = test_app();
        seed_findings(&mut app, TRIAGE_SEED);
        // Narrow to the code finding only; t must stage exactly it.
        app.dispatch(Action::Filter, &tx);
        for ch in "code bug".chars() {
            app.filter_input(KeyCode::Char(ch));
        }
        app.filter_input(KeyCode::Enter);
        assert_eq!(app.visible_findings().len(), 1);
        app.dispatch(Action::TriageAll, &tx);
        let c = app.triage_confirm.as_ref().expect("modal open");
        assert_eq!(
            (
                c.archive,
                c.queue_auto,
                c.queue_manual,
                c.dismiss,
                c.needs_you
            ),
            (0, 0, 1, 0, 0)
        );
        app.on_input(
            Event::Key(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE)),
            &tx,
        );
        let json = seeded_json(&app);
        // Only the code finding was written; the prose finding kept its prose action.
        assert_eq!(json.matches(r#""answer": "manual""#).count(), 2); // pre-queued + this
        assert!(json.contains("Resolved by narrowing the scope."));
        assert!(!json.contains(r#""action": "fixed""#));
    }

    /// Plan with two sections; "Step 2" maps into "## Steps" (lines 2..6),
    /// "chain breakage" into "## Risks" (6..8).
    const FIX_PLAN: &str =
        "# Plan\n\n## Steps\n1. first\n2. second\n\n## Risks\nchain breakage risk\n";

    /// Seed a plan + two QUEUED plan-review findings (one per section).
    fn seed_fixable(app: &mut App) {
        let plan = app.dirs.plan_file(&app.slug);
        std::fs::create_dir_all(plan.parent().unwrap()).unwrap();
        std::fs::write(&plan, FIX_PLAN).unwrap();
        seed_findings(
            app,
            r#"{"stage":"plan-review","findings":[
                {"id":1,"title":"step 2 is wrong","plan_step":"Step 2 (x)","severity":"major",
                 "scenario":"boom","verdict":"confirmed","answer":"auto"},
                {"id":2,"title":"risk is vague","plan_step":"chain breakage","severity":"minor",
                 "scenario":"unbounded","verdict":"confirmed","answer":"auto"}]}"#,
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
    fn prepare_apply_collects_only_current_features_queued() {
        let (_t, mut app, _tx, _rx) = test_app();
        seed_fixable(&mut app);
        // A queued finding on ANOTHER feature must not ride this batch.
        std::fs::write(
            app.dirs
                .findings_dir()
                .join("20260713T000001Z-plan-review.json"),
            r#"{"stage":"plan-review","branch":"other-feature","findings":[
                {"id":9,"title":"elsewhere","plan_step":"Step 1","severity":"major",
                 "verdict":"confirmed","answer":"auto"}]}"#,
        )
        .unwrap();
        app.reload_artifacts();
        let slug = app.slug.clone();
        let (cmd, ctx) = app.prepare_findings_apply(&slug).unwrap();
        assert_eq!(ctx.items.len(), 2, "other-feature finding skipped");
        assert_eq!(ctx.items[0].section.as_deref(), Some("Steps"));
        assert_eq!(ctx.items[1].section.as_deref(), Some("Risks"));
        let prompt = cmd.argv.iter().find(|a| a.starts_with("/spec")).unwrap();
        assert!(prompt.contains("FINDING #1:"));
        assert!(prompt.contains("FINDING #2:"));
        assert!(prompt.contains(r#"SCOPE: sections "Steps", "Risks""#));
        assert!(!prompt.contains("elsewhere"));
        assert_eq!(app.fix_doc_before, FIX_PLAN);
        assert_eq!(crate::undo::depth(&app.dirs, &ctx.slug, "plan"), 1);
    }

    #[test]
    fn apply_confirm_opens_counts_and_u_unqueues() {
        let (_t, mut app, tx, _rx) = test_app();
        seed_fixable(&mut app);
        // F on a queued finding opens the modal instead of unqueueing.
        app.dispatch(Action::FindingClaudeFix, &tx);
        let confirm = app.apply_confirm.as_ref().expect("modal open");
        assert_eq!(confirm.count, 2);
        assert_eq!(confirm.anchor_lost, 0);
        assert!(confirm.unqueue.is_some());
        // `u` unqueues just the cursor's finding and closes the modal.
        app.on_input(
            Event::Key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::NONE)),
            &tx,
        );
        assert!(app.apply_confirm.is_none());
        assert_eq!(app.queued_auto().len(), 1);
        // `y` routes into prepare: a zero daily budget stops it there
        // (proving the wiring without spawning a real daemon).
        app.cfg.budget_daily_usd = Some(0.0);
        // Move the cursor onto the still-queued finding (the unqueued one
        // would just get re-queued by F).
        app.selected_finding = 1;
        app.dispatch(Action::FindingClaudeFix, &tx); // reopen on the remaining ⚑A
        assert!(app.apply_confirm.is_some());
        app.on_input(
            Event::Key(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE)),
            &tx,
        );
        assert!(app.apply_confirm.is_none());
        assert!(
            app.status_msg
                .as_deref()
                .is_some_and(|m| m.contains("daily budget"))
        );
    }

    #[test]
    fn batch_confined_marks_fixed_and_declined_per_verdict() {
        let (_t, mut app, tx, _rx) = test_app();
        seed_fixable(&mut app);
        let slug = app.slug.clone();
        let (_cmd, ctx) = app.prepare_findings_apply(&slug).unwrap();
        let plan_path = ctx.plan_path.clone();
        app.fix_ctx = Some(ctx);
        app.finding_detail = true;
        // The "agent" edits BOTH allowed sections.
        std::fs::write(
            &plan_path,
            FIX_PLAN
                .replace("2. second", "2. second, hardened")
                .replace("chain breakage risk", "chain breakage risk, mitigated"),
        )
        .unwrap();
        app.on_fix_exited(
            fix_outcome(true, 0.08),
            FixTail {
                result_text: Some("ANSWERS:\n#1: FIXED\n#2: DECLINED needs a spec change".into()),
                ..Default::default()
            },
            &tx,
        );
        let json = seeded_json(&app);
        // #1 fixed, answer cleared; #2 back to triage with the reason.
        assert!(json.contains(r#""action": "fixed""#), "{json}");
        assert!(!json.contains(r#""answer": "auto""#), "{json}");
        assert!(json.contains("needs a spec change"), "{json}");
        let msg = app.status_msg.clone().unwrap_or_default();
        assert!(msg.contains("1 fixed · 1 declined"), "{msg}");
        assert!(msg.contains("u reverts"), "{msg}");
        assert!(app.fix_revertable());
        assert!(!app.finding_detail, "overlay closes with the verdict");
    }

    #[test]
    fn batch_leak_reverts_all_and_keeps_answers() {
        let (_t, mut app, tx, _rx) = test_app();
        seed_fixable(&mut app);
        let slug = app.slug.clone();
        let (_cmd, ctx) = app.prepare_findings_apply(&slug).unwrap();
        let plan_path = ctx.plan_path.clone();
        app.fix_ctx = Some(ctx);
        // The "agent" edits the locked title line between nothing - a leak.
        std::fs::write(&plan_path, FIX_PLAN.replace("# Plan", "# Plan v2")).unwrap();
        app.on_fix_exited(
            fix_outcome(true, 0.08),
            FixTail {
                result_text: Some("ANSWERS:\n#1: FIXED\n#2: FIXED".into()),
                ..Default::default()
            },
            &tx,
        );
        // Mechanically reverted; the whole queue survives; nothing revertable.
        assert_eq!(std::fs::read_to_string(&plan_path).unwrap(), FIX_PLAN);
        assert_eq!(app.queued_auto().len(), 2);
        assert!(!app.fix_revertable());
        let msg = app.status_msg.clone().unwrap_or_default();
        assert!(msg.contains("leaked"), "{msg}");
        assert!(msg.contains("2 stay queued"), "{msg}");
        assert!(!seeded_json(&app).contains(r#""action": "fixed""#));
    }

    #[test]
    fn missing_result_text_declines_everything() {
        let (_t, mut app, tx, _rx) = test_app();
        seed_fixable(&mut app);
        let slug = app.slug.clone();
        let (_cmd, ctx) = app.prepare_findings_apply(&slug).unwrap();
        let plan_path = ctx.plan_path.clone();
        app.fix_ctx = Some(ctx);
        std::fs::write(&plan_path, FIX_PLAN.replace("2. second", "2. improved")).unwrap();
        app.on_fix_exited(fix_outcome(true, 0.05), FixTail::default(), &tx);
        let json = seeded_json(&app);
        assert!(!json.contains(r#""action": "fixed""#), "{json}");
        assert_eq!(seeded_json(&app).matches("run gave no verdict").count(), 2);
        assert_eq!(app.queued_auto().len(), 0, "declined = back to unanswered");
        let msg = app.status_msg.clone().unwrap_or_default();
        assert!(msg.contains("0 fixed · 2 declined"), "{msg}");
    }

    #[test]
    fn unchanged_plan_defeats_fixed_verdicts() {
        let (_t, mut app, tx, _rx) = test_app();
        seed_fixable(&mut app);
        let slug = app.slug.clone();
        let (_cmd, ctx) = app.prepare_findings_apply(&slug).unwrap();
        app.fix_ctx = Some(ctx);
        // No edit at all, but the model CLAIMS it fixed #1.
        app.on_fix_exited(
            fix_outcome(true, 0.03),
            FixTail {
                result_text: Some("ANSWERS:\n#1: FIXED\n#2: DECLINED overlaps".into()),
                ..Default::default()
            },
            &tx,
        );
        let json = seeded_json(&app);
        assert!(!json.contains(r#""action": "fixed""#), "{json}");
        assert!(json.contains("claimed fixed but made no edit"), "{json}");
        let msg = app.status_msg.clone().unwrap_or_default();
        assert!(msg.contains("plan unchanged"), "{msg}");
        assert!(!app.fix_revertable(), "nothing to revert without an edit");
    }

    #[test]
    fn verdict_does_not_clobber_mid_run_dismissal() {
        let (_t, mut app, tx, _rx) = test_app();
        seed_fixable(&mut app);
        let slug = app.slug.clone();
        let (_cmd, ctx) = app.prepare_findings_apply(&slug).unwrap();
        let plan_path = ctx.plan_path.clone();
        app.fix_ctx = Some(ctx);
        // While the run works, the user dismisses finding #2 by hand.
        let pos2 = 1;
        crate::findings::set_action(&mut app.findings, 0, pos2, "dismissed").unwrap();
        std::fs::write(&plan_path, FIX_PLAN.replace("2. second", "2. improved")).unwrap();
        app.on_fix_exited(
            fix_outcome(true, 0.05),
            FixTail {
                result_text: Some("ANSWERS:\n#1: FIXED\n#2: FIXED".into()),
                ..Default::default()
            },
            &tx,
        );
        let json = seeded_json(&app);
        // The dismissal wins; only #1 got marked fixed.
        assert!(json.contains(r#""action": "dismissed""#), "{json}");
        assert_eq!(json.matches(r#""action": "fixed""#).count(), 1, "{json}");
    }

    #[test]
    fn doc_undo_batch_restores_plan_reopens_fixed_and_requeues() {
        let (_t, mut app, tx, _rx) = test_app();
        seed_fixable(&mut app);
        let slug = app.slug.clone();
        let (_cmd, ctx) = app.prepare_findings_apply(&slug).unwrap();
        let plan_path = ctx.plan_path.clone();
        app.fix_ctx = Some(ctx);
        std::fs::write(&plan_path, FIX_PLAN.replace("2. second", "2. improved")).unwrap();
        app.on_fix_exited(
            fix_outcome(true, 0.05),
            FixTail {
                result_text: Some("ANSWERS:\n#1: FIXED\n#2: DECLINED later".into()),
                ..Default::default()
            },
            &tx,
        );
        assert!(app.fix_revertable());

        app.doc_undo();
        assert_eq!(std::fs::read_to_string(&plan_path).unwrap(), FIX_PLAN);
        assert!(!app.fix_revertable());
        let json = seeded_json(&app);
        assert!(!json.contains(r#""action": "fixed""#), "{json}");
        // The reverted finding is queued (⚑A) again for another round.
        assert_eq!(app.queued_auto().len(), 1);
        assert!(
            app.status_msg
                .as_deref()
                .is_some_and(|m| m.contains("1 finding(s) queued again"))
        );
        // Nothing left to revert.
        app.doc_undo();
        assert!(
            app.status_msg
                .as_deref()
                .is_some_and(|m| m.contains("no applied batch to revert"))
        );
    }

    #[test]
    fn fix_failure_status_names_the_reason_and_run_id() {
        let (_t, mut app, tx, _rx) = test_app();
        seed_fixable(&mut app);
        let slug = app.slug.clone();
        let (_cmd, ctx) = app.prepare_findings_apply(&slug).unwrap();
        app.fix_ctx = Some(ctx);
        app.current_fix_run_id = Some("20260713T115537337Z-64f67-0-plan-fix".into());
        // A budget-killed run with denials - today's real failure shape.
        let outcome = Ok(RunOutcome {
            meta: RunMeta {
                ok: false,
                stage: "plan-fix".into(),
                error: Some("agent reported failure".into()),
                error_subtype: Some("error_max_budget_usd".into()),
                num_turns: Some(8),
                total_cost_usd: Some(1.26),
                permission_denials: vec![
                    serde_json::json!(
                        {"tool_name":"Edit","tool_input":{"file_path":"/x/plan.md"}}
                    );
                    3
                ],
                ..Default::default()
            },
            archive: std::path::PathBuf::new(),
        });
        app.on_fix_exited(outcome, FixTail::default(), &tx);
        let msg = app.status_msg.clone().unwrap_or_default();
        assert!(
            msg.contains("budget cap hit after 8 turns ($1.26 spent)"),
            "{msg}"
        );
        assert!(msg.contains("raise budget_finding_fix_usd"), "{msg}");
        assert!(msg.contains("3 Edit(s) denied"), "{msg}");
        assert!(
            msg.contains("ritual attach 20260713T115537337Z-64f67-0-plan-fix"),
            "{msg}"
        );
        // Queue survives an outright failure.
        assert_eq!(app.queued_auto().len(), 2);

        // When NOTHING is recorded, the agent's last words are the context.
        let (_cmd, ctx) = app.prepare_findings_apply(&slug).unwrap();
        app.fix_ctx = Some(ctx);
        app.on_fix_exited(
            fix_outcome(false, 0.01),
            FixTail {
                result_text: None,
                last_text: Some("Let me check the invariants file".into()),
            },
            &tx,
        );
        let msg = app.status_msg.clone().unwrap_or_default();
        assert!(msg.contains("Let me check the invariants file"), "{msg}");
    }

    #[test]
    fn failed_run_mid_edit_reverts_batch() {
        let (_t, mut app, tx, _rx) = test_app();
        seed_fixable(&mut app);
        let slug = app.slug.clone();
        let (_cmd, ctx) = app.prepare_findings_apply(&slug).unwrap();
        let plan_path = ctx.plan_path.clone();
        app.fix_ctx = Some(ctx);
        std::fs::write(&plan_path, "half-written garbage\n").unwrap();
        app.on_fix_exited(fix_outcome(false, 0.01), FixTail::default(), &tx);
        assert_eq!(std::fs::read_to_string(&plan_path).unwrap(), FIX_PLAN);
        assert!(
            app.status_msg
                .as_deref()
                .is_some_and(|m| m.contains("reverted"))
        );
        assert_eq!(app.queued_auto().len(), 2, "queue survives a failed run");
    }

    #[test]
    fn anchor_lost_flags_after_plan_edit_and_recovers() {
        let (_t, mut app, _tx, _rx) = test_app();
        seed_fixable(&mut app);
        // Both steps locate against the seeded plan.
        assert!(
            app.visible_findings()
                .iter()
                .all(|af| !app.is_anchor_lost(af))
        );
        // Rewrite the plan without the numbered list: "Step 2 (x)" is lost,
        // "chain breakage" still matches verbatim.
        let plan = app.dirs.plan_file(&app.slug);
        std::fs::write(
            &plan,
            "# Plan\n\n## Steps\nrewritten prose\n\n## Risks\nchain breakage risk\n",
        )
        .unwrap();
        app.reload_artifacts();
        let lost: Vec<bool> = app
            .visible_findings()
            .iter()
            .map(|af| app.is_anchor_lost(af))
            .collect();
        assert_eq!(lost, vec![true, false], "step 2 lost, chain breakage held");
        // Restoring the plan clears the marker.
        std::fs::write(&plan, FIX_PLAN).unwrap();
        app.reload_artifacts();
        assert!(
            app.visible_findings()
                .iter()
                .all(|af| !app.is_anchor_lost(af))
        );
    }

    #[test]
    fn quickfix_entries_narrow_to_manual_when_queued() {
        let (_t, mut app, _tx, _rx) = test_app();
        let plan = app.dirs.plan_file(&app.slug);
        std::fs::create_dir_all(plan.parent().unwrap()).unwrap();
        std::fs::write(&plan, FIX_PLAN).unwrap();
        seed_findings(
            &mut app,
            r#"{"stage":"plan-review","findings":[
                {"id":1,"title":"code bug","file":"src/a.rs","line":3,"severity":"major","verdict":"confirmed"},
                {"id":2,"title":"plan gap","plan_step":"Step 2 (x)","severity":"minor","verdict":"confirmed"}]}"#,
        );
        // No manual queue -> every locatable finding rides (old behavior).
        let (entries, manual_only) = app.quickfix_entries();
        assert!(!manual_only);
        assert_eq!(entries.len(), 2);
        // Queue the code finding manually -> Q becomes the manual pass.
        app.dispatch(Action::FindingManual, &_tx);
        let (entries, manual_only) = app.quickfix_entries();
        assert!(manual_only);
        assert_eq!(entries.len(), 1);
        assert!(entries[0].file.ends_with("src/a.rs"));
        assert!(entries[0].text.contains("code bug"));
    }

    #[test]
    fn chat_send_and_ctrl_z_are_held_while_fix_runs() {
        let (_t, mut app, tx, _rx) = test_app();
        seed_fixable(&mut app);
        let slug = app.slug.clone();
        let (_cmd, ctx) = app.prepare_findings_apply(&slug).unwrap();
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
            "# Plan\n\n## Phases\n\n### Step 2 - delete via load_all\nbody\n",
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
        app.cancel_run(&_tx);
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

        app.cancel_run(&_tx);
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
        // No recorded runs and no pinned session for the selected stage.
        app.takeover();
        assert!(
            app.status_msg
                .as_deref()
                .unwrap()
                .contains("no session recorded")
        );
        // A recorded run whose meta carries no session id, and no pinned
        // stage session either → still nothing to resume.
        let branch = app.branch.clone();
        let f = app.state.feature_for_branch_mut(&branch);
        f.stages.entry(StageId::Spec).or_default().runs = vec!["r-nosession".into()];
        app.selected = 0; // spec
        app.takeover();
        assert!(
            app.status_msg
                .as_deref()
                .unwrap()
                .contains("no session recorded")
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

        // d opens the reason prompt; an empty Enter is the plain dismissal.
        app.dispatch(Action::FindingDismiss, &tx);
        app.on_input(
            Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            &tx,
        );
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
    fn tests_red_pins_a_session_that_implement_resumes() {
        let (_t, mut app, tx, _rx) = test_app();

        // Launching tests-red mints + stores a session id and pins it in argv.
        app.dispatch(Action::RunStage(StageId::TestsRed), &tx);
        let req = app.pending_attached.take().expect("tests-red queued");
        let i = req
            .argv
            .iter()
            .position(|a| a == "--session-id")
            .expect("tests-red pins --session-id");
        let sid = req.argv[i + 1].clone();
        assert!(crate::export::is_uuid(&sid), "pinned a real uuid: {sid}");
        assert_eq!(
            app.state
                .stage_session_id(&app.slug, StageId::TestsRed)
                .as_deref(),
            Some(sid.as_str()),
            "persisted to state so implement can find it"
        );

        // implement opens the copy-paste overlay first, staging a resume of
        // THAT exact session; enter commits the handover.
        app.dispatch(Action::RunStage(StageId::Implement), &tx);
        assert!(
            app.pending_attached.is_none(),
            "overlay first, not a direct launch"
        );
        let hint = app.implement_hint.clone().expect("implement hint shown");
        assert!(hint.resuming);
        assert_eq!(
            hint.req.argv,
            vec!["claude".to_string(), "--resume".into(), sid.clone()]
        );
        assert!(!hint.req.argv.iter().any(|a| a == "--continue"));
        // enter commits the resume.
        press(&mut app, &tx, KeyCode::Enter);
        assert!(app.implement_hint.is_none());
        let req = app
            .pending_attached
            .take()
            .expect("resume queued after enter");
        assert_eq!(req.argv, vec!["claude".to_string(), "--resume".into(), sid]);
    }

    #[test]
    fn implement_without_pinned_session_opens_the_picker() {
        let (_t, mut app, tx, _rx) = test_app();
        app.dispatch(Action::RunStage(StageId::Implement), &tx);
        let hint = app.implement_hint.clone().expect("implement hint shown");
        assert!(!hint.resuming, "no pin → picker fallback");
        // Picker form: bare `claude --resume` (a positional would be consumed
        // as the picker's search term), never --continue.
        assert_eq!(hint.req.argv, vec!["claude", "--resume"]);
        assert!(!hint.req.argv.iter().any(|a| a == "--continue"));
    }

    #[test]
    fn implement_hint_cancels_without_launching() {
        let (_t, mut app, tx, _rx) = test_app();
        app.dispatch(Action::RunStage(StageId::Implement), &tx);
        assert!(app.implement_hint.is_some());
        press(&mut app, &tx, KeyCode::Esc);
        assert!(app.implement_hint.is_none());
        assert!(app.pending_attached.is_none(), "esc launches nothing");
    }

    // ---- code-fix pipeline -------------------------------------------------

    fn git(root: &std::path::Path, args: &[&str]) {
        std::process::Command::new("git")
            .args(args)
            .current_dir(root)
            .output()
            .unwrap();
    }

    /// Turn the test app's work_root into a git repo with one committed file,
    /// and seed one queued (⚑A) code finding pointing at it.
    fn seed_code_repo(app: &mut App) -> std::path::PathBuf {
        let root = app.dirs.work_root.clone();
        git(&root, &["init", "-q"]);
        git(&root, &["config", "user.email", "t@t"]);
        git(&root, &["config", "user.name", "t"]);
        std::fs::write(root.join("x.rs"), "fn x() {}\n").unwrap();
        git(&root, &["add", "x.rs"]);
        git(&root, &["commit", "-qm", "init"]);
        seed_findings(
            &mut *app,
            r#"{"stage":"dual-review","findings":[
                {"id":1,"title":"bug","file":"x.rs","line":1,"severity":"major",
                 "verdict":"confirmed","action":"pending","answer":"auto"}]}"#,
        );
        root
    }

    fn ok_outcome() -> Result<RunOutcome> {
        Ok(RunOutcome {
            meta: crate::history::RunMeta {
                ok: true,
                stage: "code-fix".into(),
                ..Default::default()
            },
            archive: std::path::PathBuf::new(),
        })
    }

    #[test]
    fn f_now_queues_a_code_finding() {
        let (_t, mut app, tx, _rx) = test_app();
        seed_findings(
            &mut app,
            r#"{"stage":"dual-review","findings":[
                {"id":1,"title":"bug","file":"src/a.rs","line":3,"severity":"major",
                 "verdict":"confirmed","action":"pending"}]}"#,
        );
        app.dispatch(Action::FindingClaudeFix, &tx);
        assert!(
            seeded_json(&app).contains(r#""answer": "auto""#),
            "F queues the code finding (was rejected before)"
        );
        assert_eq!(app.queued_auto().len(), 1);
        assert!(app.status_msg.as_deref().unwrap().contains("code-fix"));
    }

    #[test]
    fn queue_all_code_selects_only_confirmed_unresolved_code() {
        let (_t, mut app, tx, _rx) = test_app();
        seed_findings(
            &mut app,
            r#"{"stage":"dual-review","findings":[
                {"id":1,"title":"code confirmed","file":"src/a.rs","line":1,"verdict":"confirmed","action":"pending"},
                {"id":2,"title":"code unconfirmed","file":"src/b.rs","line":2,"verdict":"unconfirmed","action":"pending"},
                {"id":3,"title":"plan finding","plan_step":"Step 1","verdict":"confirmed","action":"pending"},
                {"id":4,"title":"code already fixed","file":"src/c.rs","line":3,"verdict":"confirmed","action":"fixed"}]}"#,
        );
        app.dispatch(Action::QueueAllCode, &tx);
        // Only #1 qualifies (confirmed code, unresolved, un-answered).
        assert_eq!(app.queued_auto().len(), 1);
        let json = seeded_json(&app);
        assert!(json.contains(r#""title": "code confirmed""#));
        // The plan finding was NOT auto-queued by A.
        assert_eq!(
            json.matches(r#""answer": "auto""#).count(),
            1,
            "A queues only the confirmed code finding"
        );
    }

    #[test]
    fn apply_confirm_partitions_plan_and_code() {
        let (_t, mut app, _tx, _rx) = test_app();
        seed_findings(
            &mut app,
            r#"{"stage":"dual-review","findings":[
                {"id":1,"title":"code","file":"src/a.rs","line":1,"verdict":"confirmed","action":"pending","answer":"auto"},
                {"id":2,"title":"plan","plan_step":"Step 1","verdict":"confirmed","action":"pending","answer":"auto"}]}"#,
        );
        app.open_apply_confirm(None);
        let c = app.apply_confirm.as_ref().expect("modal open");
        assert_eq!(c.plan_count, 1);
        assert_eq!(c.code_count, 1);
    }

    #[test]
    fn queued_auto_excludes_anchorless_findings() {
        let (_t, mut app, _tx, _rx) = test_app();
        // A hand-edited answer:"auto" on a finding with neither file nor
        // plan_step: nothing to fix, so it must not inflate the queue.
        seed_findings(
            &mut app,
            r#"{"stage":"dual-review","findings":[
                {"id":1,"title":"floating","verdict":"confirmed","action":"pending","answer":"auto"}]}"#,
        );
        assert_eq!(app.queued_auto().len(), 0);
    }

    #[test]
    fn code_fix_gate_red_reverts_and_requeues() {
        let (_t, mut app, tx, _rx) = test_app();
        let root = seed_code_repo(&mut app);
        let slug = app.slug.clone();
        let (_cmd, ctx) = app.prepare_code_fix_apply(&slug).unwrap();
        app.code_fix_ctx = Some(ctx);
        app.code_fix_ctx.as_mut().unwrap().phase = crate::code_fix::CodePhase::Checking;
        // The fix edited the file; then check.sh comes back red.
        std::fs::write(root.join("x.rs"), "fn x() { BROKEN }\n").unwrap();
        app.on_code_gate_done(false, "clippy: error".into(), &tx);
        assert!(app.code_fix_ctx.is_none(), "batch cleared");
        assert_eq!(
            std::fs::read_to_string(root.join("x.rs")).unwrap(),
            "fn x() { BROKEN }\n",
            "the attempt is LEFT in the tree, not deleted"
        );
        assert_eq!(app.queued_auto().len(), 1, "finding stays queued");
        let msg = app.status_msg.as_deref().unwrap();
        assert!(msg.contains("failed") && msg.contains("check.sh"), "{msg}");
        assert!(msg.contains("working tree"), "{msg}");
    }

    #[test]
    fn code_fix_accepts_when_both_gates_pass() {
        let (_t, mut app, tx, _rx) = test_app();
        let root = seed_code_repo(&mut app);
        let slug = app.slug.clone();
        let (_cmd, mut ctx) = app.prepare_code_fix_apply(&slug).unwrap();
        ctx.phase = crate::code_fix::CodePhase::Reviewing;
        ctx.answers.insert(1, crate::answers::AnswerVerdict::Fixed);
        app.code_fix_ctx = Some(ctx);
        // The fix left a real edit in the tree.
        std::fs::write(root.join("x.rs"), "fn x() { /* fixed */ }\n").unwrap();
        let tail = FixTail {
            result_text: Some("REVIEW:\n#1: RESOLVED\nREGRESSIONS: NONE".into()),
            last_text: None,
        };
        app.on_code_review_exited(ok_outcome(), tail, &tx);
        assert!(app.code_fix_ctx.is_none());
        assert!(
            seeded_json(&app).contains(r#""action": "fixed""#),
            "finding marked fixed"
        );
        assert_eq!(
            std::fs::read_to_string(root.join("x.rs")).unwrap(),
            "fn x() { /* fixed */ }\n",
            "a passing fix is LEFT in the worktree"
        );
    }

    #[test]
    fn code_fix_leaves_the_attempt_on_reported_regression() {
        let (_t, mut app, tx, _rx) = test_app();
        let root = seed_code_repo(&mut app);
        let slug = app.slug.clone();
        let (_cmd, mut ctx) = app.prepare_code_fix_apply(&slug).unwrap();
        ctx.phase = crate::code_fix::CodePhase::Reviewing;
        ctx.answers.insert(1, crate::answers::AnswerVerdict::Fixed);
        app.code_fix_ctx = Some(ctx);
        std::fs::write(root.join("x.rs"), "fn x() { regressed }\n").unwrap();
        let tail = FixTail {
            result_text: Some("REVIEW:\n#1: RESOLVED\nREGRESSIONS: breaks the caller".into()),
            last_text: None,
        };
        app.on_code_review_exited(ok_outcome(), tail, &tx);
        assert_eq!(
            std::fs::read_to_string(root.join("x.rs")).unwrap(),
            "fn x() { regressed }\n",
            "the attempt is LEFT in the tree even on a rejected review"
        );
        assert_eq!(app.queued_auto().len(), 1, "requeued");
        assert!(!seeded_json(&app).contains(r#""action": "fixed""#));
        assert!(app.status_msg.as_deref().unwrap().contains("regression"));
    }

    #[test]
    fn code_fix_ctx_gates_other_actions() {
        let (_t, mut app, _tx, _rx) = test_app();
        let root = seed_code_repo(&mut app);
        let slug = app.slug.clone();
        let (_cmd, ctx) = app.prepare_code_fix_apply(&slug).unwrap();
        assert!(!app.fix_running());
        app.code_fix_ctx = Some(ctx);
        assert!(
            app.fix_running(),
            "a code-fix batch counts as a running fix"
        );
        assert!(app.fix_label().unwrap().starts_with("code-fix:"));
        let _ = root;
    }

    #[test]
    fn fixed_verdict_downgraded_when_its_own_section_is_untouched() {
        let (_t, mut app, tx, _rx) = test_app();
        seed_fixable(&mut app);
        let slug = app.slug.clone();
        let (_cmd, ctx) = app.prepare_findings_apply(&slug).unwrap();
        let plan_path = ctx.plan_path.clone();
        app.fix_ctx = Some(ctx);
        // The agent edits ONLY the Steps section but over-claims BOTH fixed.
        std::fs::write(
            &plan_path,
            FIX_PLAN.replace("2. second", "2. second, hardened"),
        )
        .unwrap();
        app.on_fix_exited(
            fix_outcome(true, 0.05),
            FixTail {
                result_text: Some("ANSWERS:\n#1: FIXED\n#2: FIXED".into()),
                ..Default::default()
            },
            &tx,
        );
        let json = seeded_json(&app);
        // #1 (Steps) really moved -> fixed. #2 (Risks) untouched -> declined,
        // even though the plan changed elsewhere.
        assert_eq!(json.matches(r#""action": "fixed""#).count(), 1, "{json}");
        assert!(json.contains("its section was unchanged"), "{json}");
        assert!(
            app.status_msg
                .as_deref()
                .unwrap()
                .contains("1 fixed · 1 declined")
        );
    }

    #[test]
    fn code_fix_partial_accept_marks_resolved_and_requeues_the_rest() {
        let (_t, mut app, tx, _rx) = test_app();
        let root = app.dirs.work_root.clone();
        git(&root, &["init", "-q"]);
        git(&root, &["config", "user.email", "t@t"]);
        git(&root, &["config", "user.name", "t"]);
        std::fs::write(root.join("x.rs"), "fn x() {}\n").unwrap();
        git(&root, &["add", "x.rs"]);
        git(&root, &["commit", "-qm", "init"]);
        seed_findings(
            &mut app,
            r#"{"stage":"dual-review","findings":[
                {"id":1,"title":"bug1","file":"x.rs","line":1,"severity":"major","verdict":"confirmed","action":"pending","answer":"auto"},
                {"id":2,"title":"bug2","file":"x.rs","line":2,"severity":"major","verdict":"confirmed","action":"pending","answer":"auto"}]}"#,
        );
        let slug = app.slug.clone();
        let (_cmd, mut ctx) = app.prepare_code_fix_apply(&slug).unwrap();
        ctx.phase = crate::code_fix::CodePhase::Reviewing;
        ctx.answers.insert(1, crate::answers::AnswerVerdict::Fixed);
        ctx.answers.insert(2, crate::answers::AnswerVerdict::Fixed);
        app.code_fix_ctx = Some(ctx);
        std::fs::write(root.join("x.rs"), "fn x() { /* fixed */ }\n").unwrap();
        let tail = FixTail {
            result_text: Some(
                "REVIEW:\n#1: RESOLVED\n#2: UNRESOLVED still leaks\nREGRESSIONS: NONE".into(),
            ),
            last_text: None,
        };
        app.on_code_review_exited(ok_outcome(), tail, &tx);
        assert!(app.code_fix_ctx.is_none());
        let json = seeded_json(&app);
        assert_eq!(
            json.matches(r#""action": "fixed""#).count(),
            1,
            "only #1: {json}"
        );
        assert!(
            json.contains("still leaks"),
            "requeue reason attached: {json}"
        );
        assert_eq!(app.queued_auto().len(), 1, "#2 stays queued");
        assert!(
            app.status_msg
                .as_deref()
                .unwrap()
                .contains("1 code finding(s) fixed, 1 requeued")
        );
    }

    #[test]
    fn code_fix_fails_closed_on_no_observable_change() {
        let (_t, mut app, tx, _rx) = test_app();
        let _root = seed_code_repo(&mut app);
        let slug = app.slug.clone();
        let (_cmd, ctx) = app.prepare_code_fix_apply(&slug).unwrap();
        app.code_fix_ctx = Some(ctx); // phase Fixing
        // The run claims FIXED but never edits anything.
        app.on_code_fix_exited(
            ok_outcome(),
            FixTail {
                result_text: Some("ANSWERS:\n#1: FIXED".into()),
                last_text: None,
            },
            &tx,
        );
        assert!(app.code_fix_ctx.is_none(), "batch cleared");
        assert!(!seeded_json(&app).contains(r#""action": "fixed""#));
        assert_eq!(app.queued_auto().len(), 1, "finding stays queued");
        assert!(
            app.status_msg
                .as_deref()
                .unwrap()
                .contains("no observable change")
        );
    }

    #[test]
    fn code_fix_aborts_when_the_agent_moves_head() {
        let (_t, mut app, tx, _rx) = test_app();
        let root = seed_code_repo(&mut app);
        let slug = app.slug.clone();
        let (_cmd, ctx) = app.prepare_code_fix_apply(&slug).unwrap();
        app.code_fix_ctx = Some(ctx);
        // The fixer commits (forbidden) - HEAD moves.
        std::fs::write(root.join("x.rs"), "fn x() { fixed }\n").unwrap();
        git(&root, &["add", "x.rs"]);
        git(&root, &["commit", "-qm", "rogue"]);
        app.on_code_fix_exited(
            ok_outcome(),
            FixTail {
                result_text: Some("ANSWERS:\n#1: FIXED".into()),
                last_text: None,
            },
            &tx,
        );
        assert!(app.code_fix_ctx.is_none());
        assert!(app.status_msg.as_deref().unwrap().contains("moved HEAD"));
        assert_eq!(app.queued_auto().len(), 1, "finding stays queued");
    }

    #[test]
    fn cancel_leaves_a_code_fix_in_the_tree() {
        let (_t, mut app, tx, _rx) = test_app();
        let root = seed_code_repo(&mut app);
        let slug = app.slug.clone();
        let (_cmd, ctx) = app.prepare_code_fix_apply(&slug).unwrap();
        std::fs::write(root.join("x.rs"), "fn x() { half done }\n").unwrap();
        app.code_fix_ctx = Some(ctx);
        app.cancel_run(&tx);
        assert!(app.code_fix_ctx.is_none(), "ctx cleared");
        assert_eq!(
            std::fs::read_to_string(root.join("x.rs")).unwrap(),
            "fn x() { half done }\n",
            "the attempt is LEFT in the tree",
        );
        assert_eq!(app.queued_auto().len(), 1, "finding stays queued");
        let msg = app.status_msg.as_deref().unwrap();
        assert!(
            msg.contains("cancelled") && msg.contains("working tree"),
            "{msg}"
        );
    }

    #[test]
    fn cancel_reverts_a_plan_fix() {
        let (_t, mut app, tx, _rx) = test_app();
        seed_fixable(&mut app);
        let slug = app.slug.clone();
        let (_cmd, ctx) = app.prepare_findings_apply(&slug).unwrap();
        let plan_path = ctx.plan_path.clone();
        app.fix_ctx = Some(ctx);
        // The agent half-wrote the plan; cancelling restores it.
        std::fs::write(&plan_path, FIX_PLAN.replace("2. second", "2. HALF")).unwrap();
        app.cancel_run(&tx);
        assert!(app.fix_ctx.is_none(), "ctx cleared");
        assert_eq!(
            std::fs::read_to_string(&plan_path).unwrap(),
            FIX_PLAN,
            "plan reverted on cancel",
        );
        assert!(app.status_msg.as_deref().unwrap().contains("plan reverted"));
    }

    #[test]
    fn reload_state_adopts_cli_writes_but_keeps_the_tui_in_flight_run() {
        let (_t, mut app, _tx, _rx) = test_app();
        let slug = app.slug.clone();
        // The TUI is running dual-review and owns that live run.
        app.running = Some(StageId::DualReview);
        crate::run_cmd::set_stage(
            &mut app.state,
            &app.branch,
            StageId::DualReview,
            StageStatus::Running,
            None,
        );
        // A concurrent CLI process completes plan-review (and, wrongly, flips
        // dual-review to Failed) and saves to disk.
        let mut disk = app.state.clone();
        crate::run_cmd::set_stage(
            &mut disk,
            &app.branch,
            StageId::PlanReview,
            StageStatus::Done,
            None,
        );
        crate::run_cmd::set_stage(
            &mut disk,
            &app.branch,
            StageId::DualReview,
            StageStatus::Failed,
            None,
        );
        disk.save(&app.dirs).unwrap();

        app.reload_state();
        let feat = app.state.features.get(&slug).unwrap();
        assert_eq!(
            feat.stage(StageId::PlanReview).status,
            StageStatus::Done,
            "adopts the CLI's write"
        );
        assert_eq!(
            feat.stage(StageId::DualReview).status,
            StageStatus::Running,
            "keeps the TUI's own in-flight run, not disk's stale Failed"
        );
    }

    #[test]
    fn reset_plan_action_confirms_then_wipes_the_plan() {
        let (_t, mut app, tx, _rx) = test_app();
        let slug = app.slug.clone();
        std::fs::create_dir_all(app.dirs.feature_dir(&slug)).unwrap();
        std::fs::write(app.dirs.plan_file(&slug), "# Plan\n").unwrap();
        crate::run_cmd::set_stage(
            &mut app.state,
            &app.branch,
            StageId::Plan,
            StageStatus::Done,
            None,
        );

        // The palette action opens a confirm, changes nothing yet.
        app.dispatch(Action::ResetPlan, &tx);
        assert!(app.reset_plan_confirm);
        assert!(app.dirs.plan_file(&slug).exists());

        // Confirming wipes plan.md and resets the plan stage to Pending.
        app.do_reset_plan();
        assert!(!app.reset_plan_confirm);
        assert!(!app.dirs.plan_file(&slug).exists());
        assert_eq!(
            app.state
                .features
                .get(&slug)
                .unwrap()
                .stage(StageId::Plan)
                .status,
            StageStatus::Pending
        );
    }

    #[test]
    fn orphaned_gate_check_blocks_a_second_check() {
        let (_t, mut app, tx, _rx) = test_app();
        // Post-cancel state: the batch is gone but its detached check.sh (which
        // has no join handle) is still running.
        app.gate_check_running = true;
        app.run_check(&tx, true);
        assert_ne!(
            app.check,
            CheckState::Running,
            "no second check.sh while the gate check is in flight",
        );
        // The orphan finishing clears the flag even though the ctx is gone.
        app.on_code_gate_done(true, "ok".into(), &tx);
        assert!(!app.gate_check_running);
    }

    #[test]
    fn takeover_reads_the_pinned_tests_red_session() {
        let (_t, mut app, tx, _rx) = test_app();
        app.dispatch(Action::RunStage(StageId::TestsRed), &tx);
        let sid = app
            .state
            .stage_session_id(&app.slug, StageId::TestsRed)
            .expect("pinned");
        app.pending_attached = None;
        app.selected = PIPELINE
            .iter()
            .position(|s| *s == StageId::TestsRed)
            .unwrap();
        app.takeover();
        let req = app.pending_attached.take().expect("takeover queued");
        assert!(req.argv.contains(&"--resume".to_string()));
        assert!(req.argv.contains(&sid));
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
