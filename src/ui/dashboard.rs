//! All drawing. Pure: reads App, writes the frame. No state mutations -
//! with ONE deliberate exception: viewport extents land in `App.view_max`
//! (Cell) so the input path can clamp scrolls and implement G=bottom; the
//! renderer is the only place those extents are known.
//!
//! Visual language: NvChad/base46, borderless panels separated by background
//! shades (darker sidebar, statusline_bg bottom bar), powerline statusline
//! with the user's live separator glyphs ( / ), PmenuSel purple selection,
//! tabufline pills, nvdash greeter, telescope-style palette, which-key help.
//! Focus cue: the sidebar selection row is bright only while the pipeline
//! is actually focused (`App::pipeline_focused`), dimmed otherwise.

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph, Wrap};

use crate::findings::Severity;
use crate::keymap::{Action, describe};
use crate::runner::events::AgentEvent;
use crate::state::{PIPELINE, StageStatus};
use crate::theme::Theme;
use crate::ui::app::{App, ChatState, ChatTurn, CheckState, TABS, Tab};

const SPINNER: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
const SIDEBAR_W: u16 = 28;
const MIN_SIDEBAR_TERM_W: u16 = 70;
/// In chat mode the sidebar only survives on wide terminals; below this the
/// preview | chat split takes the full width (fits an 80-col terminal).
const CHAT_SIDEBAR_MIN_TERM_W: u16 = 100;
/// Below this main width, chat stacks vertically (preview above, chat below).
const CHAT_STACK_MIN_W: u16 = 55;

/// Whether the sidebar is dropped at this terminal width. The single
/// threshold predicate shared by the renderer (layout) and the input path
/// (`App::sidebar_hidden`, the focus-left refusal): they cannot disagree.
pub fn sidebar_hidden(term_width: u16, chat_open: bool) -> bool {
    let min = if chat_open {
        CHAT_SIDEBAR_MIN_TERM_W
    } else {
        MIN_SIDEBAR_TERM_W
    };
    term_width < min
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

fn visible_width(spans: &[Span]) -> usize {
    spans.iter().map(|s| s.content.chars().count()).sum()
}

/// Pad a row with trailing space so `bg` covers the full width.
fn fill_row<'a>(mut spans: Vec<Span<'a>>, width: u16, bg: ratatui::style::Color) -> Line<'a> {
    let used = visible_width(&spans);
    if (used as u16) < width {
        spans.push(Span::styled(
            " ".repeat(width as usize - used),
            Style::default().bg(bg),
        ));
    }
    Line::from(spans)
}

/// `fill_row` with a right-aligned chip. The chip WINS over the row tail:
/// when both don't fit, the row content is truncated char-safe with an
/// ellipsis so the triage state stays visible. Only degenerate widths
/// (nothing left for content) fall back to a plain fill.
fn fill_row_chip<'a>(
    spans: Vec<Span<'a>>,
    chip: Vec<Span<'a>>,
    width: u16,
    bg: ratatui::style::Color,
) -> Line<'a> {
    let chip_w = visible_width(&chip);
    let width = width as usize;
    if chip_w == 0 || width < chip_w + 4 {
        return fill_row(spans, width as u16, bg);
    }
    let budget = width - chip_w - 1; // row content + at least one gap space
    let mut out: Vec<Span> = Vec::new();
    let mut used = 0usize;
    for s in spans {
        let w = s.content.chars().count();
        if used + w <= budget {
            used += w;
            out.push(s);
        } else {
            // saturating: previous spans can consume EXACTLY budget, and
            // `budget - used - 1` would underflow (debug panic on resize).
            let take = budget.saturating_sub(used + 1);
            let clipped: String = s.content.chars().take(take).collect();
            out.push(Span::styled(format!("{clipped}…"), s.style));
            used += take + 1;
            break;
        }
    }
    out.push(Span::styled(
        " ".repeat(width - chip_w - used),
        Style::default().bg(bg),
    ));
    out.extend(chip);
    Line::from(out)
}

/// A powerline segment:  cap, colored body, slanted  tail (user's NvChad
/// "default" style). `base` is the bar background the caps blend into.
fn pl_segment<'a>(
    t: &Theme,
    text: String,
    seg_bg: ratatui::style::Color,
    base: ratatui::style::Color,
) -> Vec<Span<'a>> {
    vec![
        Span::styled(
            t.sep_left().to_string(),
            Style::default().fg(seg_bg).bg(base),
        ),
        Span::styled(
            text,
            Style::default()
                .fg(t.on_accent())
                .bg(seg_bg)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            t.sep_right().to_string(),
            Style::default().fg(seg_bg).bg(base),
        ),
    ]
}

/// A rounded pill (tabufline / severity badges): text on colored bg.
fn pill<'a>(t: &Theme, text: String, bg: ratatui::style::Color) -> Vec<Span<'a>> {
    match t.icons {
        crate::theme::IconSet::Nerd => vec![
            Span::styled("\u{e0b6}".to_string(), Style::default().fg(bg)),
            Span::styled(
                text,
                Style::default()
                    .fg(t.on_accent())
                    .bg(bg)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled("\u{e0b4}".to_string(), Style::default().fg(bg)),
        ],
        crate::theme::IconSet::Ascii => vec![Span::styled(
            format!("[{}]", text.trim()),
            Style::default().fg(bg).add_modifier(Modifier::BOLD),
        )],
    }
}

/// NvChad section header: ` ICON LABEL ───────`.
fn section_header<'a>(t: &Theme, icon: &'a str, label: &'a str, width: u16) -> Line<'a> {
    let head = format!(" {icon} {label} ");
    let used = head.chars().count();
    let rule_len = (width as usize).saturating_sub(used + 1);
    Line::from(vec![
        Span::styled(
            head,
            Style::default().fg(t.accent()).add_modifier(Modifier::BOLD),
        ),
        Span::styled("─".repeat(rule_len), Style::default().fg(t.divider())),
    ])
}

/// A which-key style keycap chip. Returns an owned span (the label is
/// formatted, never borrowed), so callers may pass a temporary `&String`.
fn keycap(t: &Theme, key: &str) -> Span<'static> {
    Span::styled(
        format!(" {key} "),
        Style::default()
            .fg(t.highlight())
            .bg(t.bg_row2())
            .add_modifier(Modifier::BOLD),
    )
}

fn centered_rect(parent: Rect, width: u16, height: u16) -> Rect {
    let w = width.min(parent.width);
    let h = height.min(parent.height);
    Rect::new(
        parent.x + (parent.width - w) / 2,
        parent.y + (parent.height - h) / 2,
        w,
        h,
    )
}

/// Opaque float: cleared area, black bg, purple rounded border (FloatBorder).
fn float_panel(f: &mut Frame, t: &Theme, area: Rect, title: &str) -> Rect {
    use ratatui::widgets::{Block, BorderType, Borders};
    f.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(t.accent()))
        .style(Style::default().bg(t.bg_float()))
        .title(Span::styled(
            format!(" {title} "),
            Style::default().fg(t.accent()).add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(area);
    f.render_widget(block, area);
    inner
}

// ---------------------------------------------------------------------------
// top-level layout
// ---------------------------------------------------------------------------

pub fn draw(f: &mut Frame, app: &App) {
    let t = &app.cfg.theme;
    f.render_widget(
        ratatui::widgets::Block::default().style(Style::default().bg(t.bg())),
        f.area(),
    );

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(3), Constraint::Length(1)])
        .split(f.area());

    let content = rows[0];
    // The input path needs the width for the focus-left refusal and the
    // chat's sidebar mode - the sanctioned renderer→input channel.
    app.view_max.term_width.set(f.area().width);
    // Chat mode needs the width for its own preview|chat split, so the
    // sidebar only survives on wide terminals.
    if !sidebar_hidden(content.width, app.chat.is_some()) {
        let cols = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Length(SIDEBAR_W),
                Constraint::Length(1),
                Constraint::Min(20),
            ])
            .split(content);
        draw_sidebar(f, app, cols[0]);
        draw_divider(f, t, cols[1]);
        draw_main(f, app, cols[2]);
    } else {
        draw_main(f, app, content);
    }
    draw_statusline(f, app, rows[1]);

    if app.finding_detail {
        draw_finding_detail(f, app);
    }
    if app.stage_detail {
        draw_stage_detail(f, app);
    }
    if app.dismiss_prompt.is_some() {
        draw_dismiss_prompt(f, app);
    }
    if app.apply_confirm.is_some() {
        draw_apply_confirm(f, app);
    }
    if app.triage_confirm.is_some() {
        draw_triage_confirm(f, app);
    }
    if app.implement_hint.is_some() {
        draw_implement_hint(f, app);
    }
    if app.settings.is_some() {
        draw_settings(f, app);
    }
    if app.show_help {
        draw_help(f, app);
    }
    if app.palette.is_some() {
        draw_palette(f, app);
    }
    if app.confirm_quit {
        draw_confirm_quit(f, t);
    }
    if app.reset_plan_confirm {
        draw_reset_confirm(f, t);
    }
}

fn draw_divider(f: &mut Frame, t: &Theme, area: Rect) {
    let lines: Vec<Line> = (0..area.height)
        .map(|_| {
            Line::from(Span::styled(
                "│",
                Style::default().fg(t.divider()).bg(t.bg()),
            ))
        })
        .collect();
    f.render_widget(Paragraph::new(lines), area);
}

// ---------------------------------------------------------------------------
// sidebar
// ---------------------------------------------------------------------------

fn stage_icon_color(t: &Theme, status: StageStatus) -> (&'static str, ratatui::style::Color) {
    match status {
        StageStatus::Pending => (t.icon_pending(), t.muted()),
        StageStatus::Running => (t.icon_running(), t.info()),
        StageStatus::Done => (t.icon_done(), t.ok()),
        StageStatus::Failed => (t.icon_failed(), t.error()),
        StageStatus::NeedsAttention => (t.icon_attention(), t.attention()),
        StageStatus::Skipped => (t.icon_skipped(), t.muted()),
    }
}

fn draw_sidebar(f: &mut Frame, app: &App, area: Rect) {
    let t = &app.cfg.theme;
    let bg = t.bg_sidebar();
    f.render_widget(
        ratatui::widgets::Block::default().style(Style::default().bg(bg)),
        area,
    );

    let w = area.width;
    let mut lines: Vec<Line> = vec![Line::default()];

    // FEATURES (only when more than one is in flight)
    let order = app.feature_order();
    if order.len() > 1 {
        lines.push(section_header(t, t.icon_features(), "FEATURES", w));
        for slug in order.iter().take(4) {
            let selected = *slug == app.slug;
            let needs = app.feature_needs_you(slug);
            let row_bg = if selected { t.bg_row2() } else { bg };
            let mut spans = vec![Span::styled("  ", Style::default().bg(row_bg))];
            if needs {
                spans.push(Span::styled(
                    format!("{} ", t.icon_attention()),
                    Style::default().fg(t.attention()).bg(row_bg),
                ));
            } else {
                spans.push(Span::styled("  ", Style::default().bg(row_bg)));
            }
            spans.push(Span::styled(
                slug.clone(),
                if selected {
                    Style::default()
                        .fg(t.fg())
                        .bg(row_bg)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(t.muted()).bg(row_bg)
                },
            ));
            lines.push(fill_row(spans, w, row_bg));
        }
        lines.push(Line::default());
    }

    // PIPELINE. The selection row is bright only when j/k actually drives
    // it: pipeline focus, or the Live greeter fallback (empty stream, no
    // chat) - an always-bright row would claim a focus it doesn't have.
    let cursor_live = app.pipeline_focused()
        || (app.tab == Tab::Live && app.stream.is_empty() && app.chat.is_none());
    lines.push(section_header(t, t.icon_pipeline(), "PIPELINE", w));
    for (i, id) in PIPELINE.iter().enumerate() {
        let status = app.stage_status(*id);
        let (mut icon, icon_color) = stage_icon_color(t, status);
        if status == StageStatus::Running {
            icon = SPINNER[app.spinner % SPINNER.len()];
        }
        let selected = i == app.selected;
        // Attempt history at a glance: ×N once a stage has been re-run.
        let attempts = app.stage_attempts(*id);
        let mut suffix = if attempts > 1 {
            format!(" ×{attempts}")
        } else {
            String::new()
        };
        // Done-but-stale (guidance): the inputs moved since this ran.
        let stale = app
            .guidance
            .as_ref()
            .and_then(|g| g.stages.get(id))
            .is_some_and(|sg| sg.stale.is_some());
        if stale {
            suffix.push(' ');
            suffix.push_str(t.icon_stale());
        }
        if selected && cursor_live {
            // PmenuSel: purple row, dark text.
            let spans = vec![Span::styled(
                format!("  {icon} {}{suffix}", id.label()),
                Style::default().fg(t.on_accent()).bg(t.bg_selection()),
            )];
            lines.push(fill_row(spans, w, t.bg_selection()));
        } else if selected {
            // Dimmed cue: the cursor is here, but j/k drives the panel.
            let spans = vec![Span::styled(
                format!("  {icon} {}{suffix}", id.label()),
                Style::default().fg(t.fg()).bg(t.bg_row2()),
            )];
            lines.push(fill_row(spans, w, t.bg_row2()));
        } else {
            let label_color = match status {
                StageStatus::Done if stale => t.warn(),
                StageStatus::Done => t.grey_fg(),
                StageStatus::Running => t.info(),
                StageStatus::Failed => t.error(),
                StageStatus::NeedsAttention => t.attention(),
                _ => t.muted(),
            };
            let spans = vec![
                Span::styled(format!("  {icon} "), Style::default().fg(icon_color).bg(bg)),
                Span::styled(
                    format!("{}{suffix}", id.label()),
                    Style::default().fg(label_color).bg(bg),
                ),
            ];
            lines.push(fill_row(spans, w, bg));
        }
    }
    // Guidance strip: the next actionable stage + its top note/warning
    // (cached on App; the renderer only reads).
    if let Some(g) = &app.guidance {
        if let Some(next) = g.next {
            lines.push(fill_row(
                vec![Span::styled(
                    format!("  » next: {}", next.label()),
                    Style::default().fg(t.info()).bg(bg),
                )],
                w,
                bg,
            ));
        }
        if let Some(note) = g
            .next_note
            .as_deref()
            .or_else(|| g.warnings.first().map(String::as_str))
        {
            // Wrap across the narrow sidebar instead of clipping mid-word: the
            // 2-space lead matches the rows above, continuation rows hang under
            // the text (indent 2 within the w-2 content budget).
            for row in wrap_plain(note, w.saturating_sub(2) as usize, 2) {
                lines.push(fill_row(
                    vec![Span::styled(
                        format!("  {row}"),
                        Style::default().fg(t.warn()).bg(bg),
                    )],
                    w,
                    bg,
                ));
            }
        }
    }
    lines.push(Line::default());

    // AGENTS
    lines.push(section_header(t, t.icon_agent(), "AGENTS", w));
    let claude = match &app.agents.claude {
        Some(a) if a.logged_in => (
            format!("claude {}", a.subscription_type.as_deref().unwrap_or("ok")),
            t.ok(),
        ),
        Some(_) => ("claude: logged out".into(), t.error()),
        None => ("claude …".into(), t.comment()),
    };
    let codex = match app.agents.codex_cli_ok {
        Some(true) => ("codex ok".to_string(), t.ok()),
        Some(false) => ("codex: login needed".into(), t.error()),
        None => ("codex …".into(), t.comment()),
    };
    let bridge = match app.agents.mcp_codex_connected {
        Some(true) => ("bridge ok".to_string(), t.ok()),
        Some(false) => ("bridge down".into(), t.error()),
        None => ("bridge …".into(), t.comment()),
    };
    let check = match &app.check {
        CheckState::Unknown => ("check ?".to_string(), t.comment()),
        CheckState::Running => (
            format!("check {}", SPINNER[app.spinner % SPINNER.len()]),
            t.info(),
        ),
        CheckState::Green => ("check green".into(), t.ok()),
        CheckState::Red { .. } => ("check RED".into(), t.error()),
    };
    let nvim = match &app.agents.nvim {
        Some(_) => ("nvim ok".to_string(), t.ok()),
        None => ("nvim …".to_string(), t.comment()),
    };
    for (icon, (text, color)) in [
        (t.icon_agent(), claude),
        (t.icon_agent(), codex),
        (t.icon_bridge(), bridge),
        (t.icon_nvim(), nvim),
        (t.icon_check(), check),
    ] {
        let spans = vec![
            Span::styled(
                format!("  {icon} "),
                Style::default().fg(t.grey_fg()).bg(bg),
            ),
            Span::styled(text, Style::default().fg(color).bg(bg)),
        ];
        lines.push(fill_row(spans, w, bg));
    }

    // Branch, receded at the bottom of the stack.
    lines.push(Line::default());
    let spans = vec![
        Span::styled(
            format!("  {} ", t.icon_branch()),
            Style::default().fg(t.highlight()).bg(bg),
        ),
        Span::styled(app.branch.clone(), Style::default().fg(t.muted()).bg(bg)),
    ];
    lines.push(fill_row(spans, w, bg));

    f.render_widget(Paragraph::new(lines), area);
}

// ---------------------------------------------------------------------------
// main pane: tabufline + content
// ---------------------------------------------------------------------------

fn draw_main(f: &mut Frame, app: &App, area: Rect) {
    let t = &app.cfg.theme;
    if app.chat.is_some() {
        draw_chat(f, app, area);
        return;
    }
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(0),
        ])
        .split(area);

    // Tabufline pills.
    let mut spans: Vec<Span> = vec![Span::raw(" ")];
    for (tab, name) in TABS {
        if *tab == app.tab {
            spans.extend(pill(t, format!(" {name} "), t.accent()));
        } else {
            spans.push(Span::styled(
                format!("  {name}  "),
                Style::default().fg(t.grey_fg()),
            ));
        }
    }
    f.render_widget(Paragraph::new(Line::from(spans)), chunks[0]);
    // Thin rule under the tabs.
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "─".repeat(chunks[1].width as usize),
            Style::default().fg(t.divider()),
        ))),
        chunks[1],
    );

    let content = Rect::new(
        chunks[2].x + 1,
        chunks[2].y,
        chunks[2].width.saturating_sub(2),
        chunks[2].height,
    );
    // A one-row `/` filter bar sits above the findings/history list when active.
    let content = if app.filter_active() {
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(1), Constraint::Min(0)])
            .split(content);
        draw_filter_bar(f, app, rows[0]);
        rows[1]
    } else {
        content
    };
    match app.tab {
        Tab::Live => draw_live(f, app, content),
        Tab::Findings => draw_findings(f, app, content),
        Tab::History => draw_history(f, app, content),
        Tab::Plan => draw_plan(f, app, content),
        Tab::Guide => draw_guide(f, app, content),
    }
}

/// The `/` filter bar: the live needle plus a match count, a caret while
/// typing. Rendered only when [`App::filter_active`].
fn draw_filter_bar(f: &mut Frame, app: &App, area: Rect) {
    let t = &app.cfg.theme;
    let count = match app.tab {
        Tab::Findings => app.visible_findings().len(),
        Tab::History => app.visible_metas().len(),
        _ => 0,
    };
    let caret = if app.filter_editing { "▏" } else { "" };
    let spans = vec![
        Span::styled(" / ", Style::default().fg(t.accent()).bg(t.bg_row())),
        Span::styled(
            format!("{}{caret}", app.filter),
            Style::default().fg(t.fg()).bg(t.bg_row()),
        ),
        Span::styled(
            format!("  {count} match{}", if count == 1 { "" } else { "es" }),
            Style::default().fg(t.comment()).bg(t.bg_row()),
        ),
    ];
    f.render_widget(
        Paragraph::new(fill_row(spans, area.width, t.bg_row())),
        area,
    );
}

// ---------------------------------------------------------------------------
// spec/plan chat: live doc preview (left) + conversation (right)
// ---------------------------------------------------------------------------

fn draw_chat(f: &mut Frame, app: &App, area: Rect) {
    let t = &app.cfg.theme;
    let Some(chat) = &app.chat else { return };
    if area.width >= CHAT_STACK_MIN_W {
        let chat_w = (area.width * 45 / 100).clamp(30, 60);
        let cols = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Min(24),
                Constraint::Length(1),
                Constraint::Length(chat_w),
            ])
            .split(area);
        draw_chat_preview(f, app, chat, cols[0]);
        draw_divider(f, t, cols[1]);
        draw_chat_panel(f, app, chat, cols[2]);
    } else {
        // Too narrow to sit side by side: stack, keeping the input visible.
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(3),
                Constraint::Length(1),
                Constraint::Length(10),
            ])
            .split(area);
        draw_chat_preview(f, app, chat, rows[0]);
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "─".repeat(rows[1].width as usize),
                Style::default().fg(t.divider()),
            ))),
            rows[1],
        );
        draw_chat_panel(f, app, chat, rows[2]);
    }
}

/// The live document (from the App's mtime-gated cache, so edits appear
/// within a tick without per-frame disk reads), focused on the current
/// target: the whole doc, or one section's raw slice.
fn draw_chat_preview(f: &mut Frame, app: &App, chat: &ChatState, area: Rect) {
    let t = &app.cfg.theme;
    let target = chat.target();
    let (full, doc_label) = match target {
        Some(tg) => (
            match tg.doc {
                crate::stages::DocKind::Spec => app.spec_doc.clone(),
                crate::stages::DocKind::Plan => app.plan_doc.clone(),
            },
            tg.doc.label(),
        ),
        None => (app.spec_doc.clone(), "spec"),
    };
    let full = full.unwrap_or_default();
    // The FULL document renders; a section target becomes a focus range:
    // banded and auto-scrolled to, with the rest visible for context.
    let (header, focus) = match target {
        Some(tg) if tg.section.is_some() => (
            format!(
                " {} · § {}",
                doc_label,
                tg.section.clone().unwrap_or_default()
            ),
            Some(tg.range.clone()),
        ),
        _ => (format!(" {doc_label} · whole"), None),
    };
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(0)])
        .split(area);
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            header,
            Style::default()
                .fg(t.comment())
                .add_modifier(Modifier::BOLD),
        ))),
        rows[0],
    );
    let _ = draw_markdown_scrolled(f, t, rows[1], &full, 0, focus.as_ref());
}

/// The conversation: header + windowed transcript + a cursored input line.
fn draw_chat_panel(f: &mut Frame, app: &App, chat: &ChatState, area: Rect) {
    let t = &app.cfg.theme;
    // The input box grows with Alt+Enter newlines, up to 4 rows.
    let input_rows = (chat.input.iter().filter(|c| **c == '\n').count() as u16 + 1).clamp(1, 4);
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(input_rows),
            Constraint::Length(1), // persistent key footer (chat swallows `?`)
        ])
        .split(area);

    // Header: which target + the keys.
    let target_label = chat
        .target()
        .map(|tg| tg.label())
        .unwrap_or_else(|| "spec".into());
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                format!(" {} chat ", t.icon_agent()),
                Style::default().fg(t.accent()).add_modifier(Modifier::BOLD),
            ),
            Span::styled(format!("· {target_label}"), Style::default().fg(t.muted())),
        ])),
        rows[0],
    );

    // Transcript, windowed by scroll (offset from the bottom; 0 = tail).
    let width = rows[1].width;
    let mut lines: Vec<Line> = Vec::new();
    for turn in &chat.transcript {
        match turn {
            ChatTurn::User(text) => lines.push(Line::from(vec![
                Span::styled(
                    "› ",
                    Style::default()
                        .fg(t.highlight())
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(text.clone(), Style::default().fg(t.fg())),
            ])),
            ChatTurn::Assistant(evs) => {
                for ev in evs {
                    lines.push(event_line(t, ev, width));
                }
            }
            ChatTurn::System(s) => lines.push(Line::from(Span::styled(
                format!("  {s}"),
                Style::default()
                    .fg(t.comment())
                    .add_modifier(Modifier::ITALIC),
            ))),
        }
    }
    if chat.in_flight {
        let sp = SPINNER[app.spinner % SPINNER.len()];
        lines.push(Line::from(Span::styled(
            format!("  {sp} editing…"),
            Style::default().fg(t.info()),
        )));
    }
    if lines.is_empty() {
        lines.push(Line::from(Span::styled(
            "  ask me to write or refine this doc",
            Style::default().fg(t.comment()),
        )));
    }
    let h = rows[1].height as usize;
    let total = lines.len();
    let max_start = total.saturating_sub(h);
    let start = max_start.saturating_sub(chat.scroll.min(max_start));
    let visible: Vec<Line> = lines.into_iter().skip(start).take(h).collect();
    f.render_widget(Paragraph::new(visible).wrap(Wrap { trim: false }), rows[1]);

    // Input box (possibly multi-line via Alt+Enter): the caret sits on the
    // cursor's own line, split at its column. First line carries the prompt
    // glyph; continuation lines get a matching indent.
    let cursor_row = chat.input[..chat.cursor]
        .iter()
        .filter(|c| **c == '\n')
        .count();
    let text: String = chat.input.iter().collect();
    let mut consumed = 0usize; // chars consumed incl. the row's trailing '\n'
    let mut input_lines: Vec<Line> = Vec::new();
    let input_fg = if chat.input_focused {
        t.fg()
    } else {
        t.muted()
    };
    let prompt_fg = if chat.input_focused {
        t.highlight()
    } else {
        t.muted()
    };
    for (row, seg) in text.split('\n').enumerate() {
        let lead = if row == 0 {
            Span::styled(
                format!(" {} ", t.icon_prompt()),
                Style::default().fg(prompt_fg).bg(t.bg_row()),
            )
        } else {
            Span::styled("   ", Style::default().bg(t.bg_row()))
        };
        let mut spans = vec![lead];
        // The caret is drawn only while the input is focused; sidebar
        // mode renders the draft dimmed and caretless.
        if chat.input_focused && row == cursor_row {
            let col = chat.cursor - consumed;
            let before: String = seg.chars().take(col).collect();
            let after: String = seg.chars().skip(col).collect();
            spans.push(Span::styled(
                before,
                Style::default().fg(input_fg).bg(t.bg_row()),
            ));
            spans.push(Span::styled(
                "▏",
                Style::default().fg(t.info()).bg(t.bg_row()),
            ));
            spans.push(Span::styled(
                after,
                Style::default().fg(input_fg).bg(t.bg_row()),
            ));
        } else {
            spans.push(Span::styled(
                seg.to_string(),
                Style::default().fg(input_fg).bg(t.bg_row()),
            ));
        }
        input_lines.push(fill_row(spans, rows[2].width, t.bg_row()));
        consumed += seg.chars().count() + 1;
    }
    // Show the last input_rows lines so the caret's line stays visible.
    let skip = input_lines.len().saturating_sub(rows[2].height as usize);
    f.render_widget(
        Paragraph::new(input_lines.into_iter().skip(skip).collect::<Vec<_>>()),
        rows[2],
    );

    // Persistent key footer: the chat swallows `?` (it types), so this line
    // is the only in-context documentation. Two states: idle vs in-flight.
    let footer = if !chat.input_focused {
        // Below the sidebar threshold j/k scrolls the transcript instead of
        // driving a pipeline that isn't drawn - the footer must not lie.
        if sidebar_hidden(f.area().width, true) {
            " j/k scroll · l edit input · i stage · ctrl+x cancel · esc close"
        } else {
            " j/k pipeline · l edit input · i stage · ctrl+x cancel · esc close"
        }
    } else if chat.in_flight {
        " enter queue · ctrl+x cancel edit · alt+←/alt+h sidebar · ↑↓ scroll"
    } else {
        " enter send · alt+enter ⏎ · tab target · alt+←/alt+h sidebar · alt+z/ctrl+z undo · esc close"
    };
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            footer,
            Style::default().fg(t.comment()).add_modifier(Modifier::DIM),
        ))),
        rows[3],
    );
}

// ---------------------------------------------------------------------------
// live tab: greeter / stream / check pane
// ---------------------------------------------------------------------------

fn draw_greeter(f: &mut Frame, t: &Theme, area: Rect) {
    let banner = ["█▀█ █ ▀█▀ █ █ ▄▀█ █  ", "█▀▄ █  █  █▄█ █▀█ █▄▄"];
    let mut lines: Vec<Line> = Vec::new();
    for row in banner {
        lines.push(Line::from(Span::styled(
            row.to_string(),
            Style::default().fg(t.accent()).add_modifier(Modifier::BOLD),
        )));
    }
    lines.push(Line::from(Span::styled(
        "s u m m o n  ·  r e v i e w  ·  v e r i f y",
        Style::default().fg(t.comment()),
    )));
    lines.push(Line::default());

    // Super-concise feature map: fixed-width rows so the centered block
    // keeps a clean label column.
    let guide: [(&str, &str); 11] = [
        ("pipeline", "spec → plan → review → tests → impl → dual"),
        ("chat", "s: chat to write/edit the spec or plan live"),
        ("runs", "daemons: quit freely, reattach · a takeover"),
        (
            "findings",
            "enter details · F/m triage · d dismiss · / filter",
        ),
        ("gates", "mutants · secrets · lessons · invariants"),
        ("money", "daily budget · per-run caps · costs"),
        ("safety", "redaction · verify-log chain · repro"),
        ("ci", "--ci → JUnit · --json + exit codes"),
        ("parallel", "new --worktree · [ ] switch features"),
        ("more", "ps · attach · doctor · clean · pr-comment"),
        ("guide", "5: the detailed guide & tips, in-app"),
    ];
    let label_w = guide
        .iter()
        .map(|(l, _)| l.chars().count())
        .max()
        .unwrap_or(0);
    let desc_w = guide
        .iter()
        .map(|(_, d)| d.chars().count())
        .max()
        .unwrap_or(0);
    for (label, desc) in guide {
        let pad = desc_w - desc.chars().count();
        lines.push(Line::from(vec![
            Span::styled(
                format!("{label:>label_w$}  "),
                Style::default()
                    .fg(t.highlight())
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("{desc}{}", " ".repeat(pad)),
                Style::default().fg(t.muted()),
            ),
        ]));
    }
    lines.push(Line::default());
    lines.push(Line::from(vec![
        keycap(t, "enter"),
        Span::styled(" run stage   ", Style::default().fg(t.comment())),
        keycap(t, ":"),
        Span::styled(" palette   ", Style::default().fg(t.comment())),
        keycap(t, "?"),
        Span::styled(" keys   ", Style::default().fg(t.comment())),
        keycap(t, "q"),
        Span::styled(" quit", Style::default().fg(t.comment())),
    ]));

    let h = lines.len() as u16;
    let y = area.y + area.height.saturating_sub(h) / 2;
    let rect = Rect::new(area.x, y, area.width, h.min(area.height));
    f.render_widget(Paragraph::new(lines).alignment(Alignment::Center), rect);
}

fn event_line<'a>(t: &Theme, ev: &'a AgentEvent, width: u16) -> Line<'a> {
    match ev {
        AgentEvent::SessionStart { model, .. } => Line::from(vec![
            Span::styled(
                format!("{} ", t.icon_agent()),
                Style::default().fg(t.accent()),
            ),
            Span::styled(
                model.clone(),
                Style::default().fg(t.accent()).add_modifier(Modifier::BOLD),
            ),
        ]),
        AgentEvent::Text { text } => {
            Line::from(Span::styled(text.clone(), Style::default().fg(t.fg())))
        }
        AgentEvent::Thinking { text } => Line::from(Span::styled(
            text.clone(),
            Style::default()
                .fg(t.comment())
                .add_modifier(Modifier::ITALIC),
        )),
        AgentEvent::ToolUse { name, summary } => Line::from(vec![
            Span::styled(
                format!("{} ", t.icon_gutter()),
                Style::default().fg(t.info()),
            ),
            Span::styled(
                name.clone(),
                Style::default().fg(t.info()).add_modifier(Modifier::BOLD),
            ),
            Span::styled(format!(" {summary}"), Style::default().fg(t.muted())),
        ]),
        AgentEvent::ToolResult { is_error, summary } => {
            let color = if *is_error { t.error() } else { t.grey_fg() };
            Line::from(vec![
                Span::styled("  ↳ ", Style::default().fg(color)),
                Span::styled(summary.clone(), Style::default().fg(color)),
            ])
        }
        AgentEvent::RateLimit(info) => Line::from(Span::styled(
            format!("rate limit: {}", info.status.as_deref().unwrap_or("?")),
            Style::default().fg(t.warn()),
        )),
        AgentEvent::Completed {
            ok,
            total_cost_usd,
            duration_ms,
            ..
        } => {
            let (icon, color) = if *ok {
                (t.icon_done(), t.ok())
            } else {
                (t.icon_failed(), t.error())
            };
            let text = format!(
                "{icon} {} {}",
                total_cost_usd
                    .map(|c| format!("${c:.3}"))
                    .unwrap_or_default(),
                duration_ms
                    .map(|d| format!("{:.1}s", d as f64 / 1000.0))
                    .unwrap_or_default()
            );
            // Full-width band so the run boundary reads at a glance.
            fill_row(
                vec![Span::styled(
                    text,
                    Style::default()
                        .fg(color)
                        .bg(t.bg_row())
                        .add_modifier(Modifier::BOLD),
                )],
                width,
                t.bg_row(),
            )
        }
        AgentEvent::Stderr { line } => Line::from(Span::styled(
            line.clone(),
            Style::default().fg(t.comment()).add_modifier(Modifier::DIM),
        )),
        AgentEvent::Raw { value } => Line::from(Span::styled(
            format!(
                "· {}",
                value
                    .get("type")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("?")
            ),
            Style::default().fg(t.comment()).add_modifier(Modifier::DIM),
        )),
    }
}

fn draw_live(f: &mut Frame, app: &App, area: Rect) {
    let t = &app.cfg.theme;

    // A red check gets a dedicated pane at the bottom of the live tab.
    let (stream_area, check_area) = if let CheckState::Red { tail } = &app.check {
        let h = (tail.lines().count() as u16 + 2).min(area.height / 2);
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(1), Constraint::Length(h)])
            .split(area);
        (chunks[0], Some((chunks[1], tail.clone())))
    } else {
        (area, None)
    };

    if app.stream.is_empty() {
        if app.running.is_some() {
            f.render_widget(
                Paragraph::new(Span::styled(
                    "waiting for agent output…",
                    Style::default().fg(t.comment()),
                ))
                .alignment(Alignment::Center),
                Rect::new(
                    stream_area.x,
                    stream_area.y + stream_area.height / 2,
                    stream_area.width,
                    1,
                ),
            );
        } else {
            draw_greeter(f, t, stream_area);
        }
    } else {
        let height = stream_area.height as usize;
        let len = app.stream.len();
        // The scroll value is the EXCLUSIVE end index. Clamp low so
        // scroll-to-top (Some(0)) shows the first page instead of nothing,
        // and clamp high so a stale value left by the ring-buffer drain
        // (update() drops 1000 events without touching the scroll) can never
        // slice past the end and panic mid-run.
        let end = app
            .stream_scroll
            .map(|s| s.clamp(height.min(len), len))
            .unwrap_or(len);
        let start = end.saturating_sub(height);
        let lines: Vec<Line> = app.stream[start..end]
            .iter()
            .map(|e| event_line(t, e, stream_area.width))
            .collect();
        // Wrap long agent output instead of clipping it at the right edge, then
        // scroll so the newest wrapped rows stay pinned to the bottom (a wrapped
        // window can exceed `height` rows; scroll hides the overflow at the top,
        // preserving the follow-tail). `wrapped_rows` mirrors ratatui's wrapper.
        let inner_w = stream_area.width as usize;
        let total: u16 = lines.iter().map(|l| wrapped_rows(l, inner_w)).sum();
        let scroll_y = total.saturating_sub(stream_area.height);
        f.render_widget(
            Paragraph::new(lines)
                .wrap(Wrap { trim: false })
                .scroll((scroll_y, 0)),
            stream_area,
        );
    }

    if let Some((rect, tail)) = check_area {
        let mut lines = vec![Line::from(vec![
            Span::styled("▔▔▔", Style::default().fg(t.error())),
            Span::styled(
                " check.sh failed ",
                Style::default().fg(t.error()).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                "▔".repeat((rect.width as usize).saturating_sub(20)),
                Style::default().fg(t.error()),
            ),
        ])];
        for l in tail.lines() {
            lines.push(Line::from(Span::styled(
                l.to_string(),
                Style::default().fg(t.error()),
            )));
        }
        f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), rect);
    }
}

// ---------------------------------------------------------------------------
// findings / history / plan tabs
// ---------------------------------------------------------------------------

fn severity_pill<'a>(t: &Theme, sev: Severity) -> Vec<Span<'a>> {
    let (label, color) = match sev {
        Severity::Critical => ("CRIT", t.error()),
        Severity::Major => ("MAJR", t.warn()),
        Severity::Minor => ("minr", t.attention()),
    };
    pill(t, format!(" {label} "), color)
}

fn draw_findings(f: &mut Frame, app: &App, area: Rect) {
    let t = &app.cfg.theme;
    let agg = app.visible_findings();
    if agg.is_empty() {
        let msg = if app.filter_active() {
            "no findings match the filter"
        } else {
            "no findings; run plan-review or dual-review"
        };
        f.render_widget(
            Paragraph::new(Span::styled(msg, Style::default().fg(t.comment())))
                .alignment(Alignment::Center),
            Rect::new(area.x, area.y + area.height / 2, area.width, 1),
        );
        return;
    }
    let mut lines: Vec<Line> = vec![Line::default()];
    let per_finding = 3usize; // row + scenario + spacer
    // Reserve one line: the selected finding may add a snippet row.
    let visible = ((area.height as usize).saturating_sub(1) / per_finding).max(1);
    let first = app
        .selected_finding
        .saturating_sub(visible.saturating_sub(1));
    for (i, af) in agg.iter().enumerate().skip(first).take(visible) {
        let (src, finding) = (&af.file_idx, &af.finding);
        let selected = i == app.selected_finding;
        let resolved = finding.resolved();
        let row_bg = if selected { t.bg_row2() } else { t.bg() };
        // Resolved rows recede: dim text, no colored pills.
        let title_fg = if resolved { t.comment() } else { t.fg() };
        let mut spans: Vec<Span> = vec![Span::styled(" ", Style::default().bg(row_bg))];
        spans.extend(severity_pill(t, finding.severity));
        spans.push(Span::styled(" ", Style::default().bg(row_bg)));
        if finding.cross_confirmed() {
            if resolved {
                spans.push(Span::styled(
                    "◆both ",
                    Style::default().fg(t.comment()).bg(row_bg),
                ));
            } else {
                spans.extend(pill(t, " ◆both ".into(), t.ok()));
            }
        } else {
            spans.push(Span::styled(
                "◇single",
                Style::default()
                    .fg(if resolved { t.comment() } else { t.warn() })
                    .bg(row_bg),
            ));
        }
        // The plan step no longer locates: don't pretend the link holds.
        if app.is_anchor_lost(af) {
            spans.push(Span::styled(
                " ⚓",
                Style::default().fg(t.warn()).bg(row_bg),
            ));
        }
        spans.push(Span::styled(
            format!(" {} ", finding.location()),
            Style::default().fg(t.info()).bg(row_bg),
        ));
        spans.push(Span::styled(
            finding.title.clone(),
            Style::default()
                .fg(title_fg)
                .bg(row_bg)
                .add_modifier(if selected {
                    Modifier::BOLD
                } else {
                    Modifier::empty()
                }),
        ));
        // Right-aligned triage chip: current state, else a dim ghost of what
        // `t` (one-touch triage) would do with this finding.
        let chip: Vec<Span> = if resolved {
            if finding.action == "fixed" {
                vec![Span::styled(
                    "✓ fixed ",
                    Style::default().fg(t.ok()).bg(row_bg),
                )]
            } else {
                vec![Span::styled(
                    "∅ dismissed ",
                    Style::default().fg(t.comment()).bg(row_bg),
                )]
            }
        } else {
            match finding.answer.as_deref() {
                Some("auto") => vec![Span::styled(
                    "⚑A queued ",
                    Style::default().fg(t.accent()).bg(row_bg),
                )],
                Some("manual") => vec![Span::styled(
                    "⚑M manual ",
                    Style::default().fg(t.info()).bg(row_bg),
                )],
                _ if finding.reason.is_some() => vec![Span::styled(
                    "✗ declined ",
                    Style::default().fg(t.warn()).bg(row_bg),
                )],
                _ => match crate::findings::recommend(finding) {
                    Some(rec) => {
                        use crate::findings::Recommendation as R;
                        let label = match rec {
                            R::QueueAuto => "→⚑A",
                            R::QueueManual => "→⚑M",
                            R::Archive => "→✓",
                            R::Dismiss(_) => "→∅",
                            R::NeedsYou => "→you",
                        };
                        vec![Span::styled(
                            format!("{label} "),
                            Style::default()
                                .fg(t.comment())
                                .bg(row_bg)
                                .add_modifier(Modifier::DIM),
                        )]
                    }
                    None => vec![],
                },
            }
        };
        lines.push(fill_row_chip(spans, chip, area.width, row_bg));

        let stage = &app.findings[*src].file.stage;
        lines.push(Line::from(vec![
            Span::raw("   "),
            Span::styled(
                if finding.scenario.is_empty() {
                    format!("[{stage}:{}]", finding.verdict)
                } else {
                    format!("{} [{stage}:{}]", finding.scenario, finding.verdict)
                },
                Style::default().fg(t.comment()),
            ),
        ]));
        // The anchored source excerpt, selected finding only (first line).
        if selected && let Some(snippet) = &finding.snippet {
            let one = snippet.lines().next().unwrap_or_default().trim_end();
            let more = if snippet.lines().nth(1).is_some() {
                " …"
            } else {
                ""
            };
            lines.push(Line::from(vec![
                Span::raw("   "),
                Span::styled(
                    format!("▏ {one}{more}"),
                    Style::default().fg(t.comment()).add_modifier(Modifier::DIM),
                ),
            ]));
        }
        lines.push(Line::default());
    }
    f.render_widget(Paragraph::new(lines), area);
}

fn draw_history(f: &mut Frame, app: &App, area: Rect) {
    let t = &app.cfg.theme;
    let metas = app.visible_metas();
    if metas.is_empty() {
        let msg = if app.filter_active() {
            "no runs match the filter"
        } else {
            "no runs yet"
        };
        f.render_widget(
            Paragraph::new(Span::styled(msg, Style::default().fg(t.comment())))
                .alignment(Alignment::Center),
            Rect::new(area.x, area.y + area.height / 2, area.width, 1),
        );
        return;
    }
    // Scrollable: j/k move `history_scroll`; the renderer reports the extent
    // so the input path can clamp and implement G=last page.
    let rows = (area.height as usize).saturating_sub(1);
    let max_scroll = metas.len().saturating_sub(rows.max(1));
    app.view_max.history.set(max_scroll);
    let offset = app.history_scroll.min(max_scroll);
    let mut lines: Vec<Line> = vec![Line::default()];
    for (i, m) in metas.iter().skip(offset).take(rows).enumerate() {
        let row_bg = if i % 2 == 1 { t.bg_row() } else { t.bg() };
        let (icon, color) = if m.ok {
            (t.icon_done(), t.ok())
        } else {
            (t.icon_failed(), t.error())
        };
        let when = m
            .started_at
            .map(|d| d.format("%m-%d %H:%M").to_string())
            .unwrap_or_else(|| "?".into());
        let spans = vec![
            Span::styled(format!(" {icon} "), Style::default().fg(color).bg(row_bg)),
            Span::styled(
                format!("{when}  "),
                Style::default().fg(t.grey_fg()).bg(row_bg),
            ),
            Span::styled(
                format!("{:<13}", m.stage),
                Style::default().fg(t.fg()).bg(row_bg),
            ),
            Span::styled(
                format!("{:<8}", m.agent),
                Style::default().fg(t.info()).bg(row_bg),
            ),
            Span::styled(
                m.total_cost_usd
                    .map(|c| format!("{:>9}", format!("${c:.3}")))
                    .unwrap_or_else(|| format!("{:>9}", "-")),
                Style::default().fg(t.warn()).bg(row_bg),
            ),
            Span::styled(
                m.usage
                    .as_ref()
                    .map(|u| format!("  {}↑ {}↓", u.input_tokens, u.output_tokens))
                    .unwrap_or_default(),
                Style::default().fg(t.muted()).bg(row_bg),
            ),
        ];
        lines.push(fill_row(spans, area.width, row_bg));
    }
    f.render_widget(Paragraph::new(lines), area);
}

fn draw_plan(f: &mut Frame, app: &App, area: Rect) {
    let plan = app.plan_doc.clone();
    let spec = app.spec_doc.clone();
    let text = match (plan, spec) {
        (Some(p), _) => p,
        (None, Some(s)) => format!("*(no plan yet; spec below)*\n\n{s}"),
        (None, None) => "no spec or plan yet; press enter on the spec stage".into(),
    };
    let max = draw_markdown_scrolled(f, &app.cfg.theme, area, &text, app.plan_scroll, None);
    app.view_max.plan.set(max);
}

/// In-app manual: detailed guide + tips, embedded at compile time.
fn draw_guide(f: &mut Frame, app: &App, area: Rect) {
    let text = include_str!("../../docs/guide.md");
    let max = draw_markdown_scrolled(f, &app.cfg.theme, area, text, app.guide_scroll, None);
    app.view_max.guide.set(max);
}

/// Themed markdown (pulldown-cmark) with j/k scrolling and a top-right
/// position hint, shared by the plan/guide tabs and the chat preview.
/// `focus` is a SOURCE-line range: matching output lines get a subtle band,
/// and (when the caller isn't scrolling) the view auto-scrolls to it.
fn draw_markdown_scrolled(
    f: &mut Frame,
    t: &Theme,
    area: Rect,
    text: &str,
    scroll: usize,
    focus: Option<&std::ops::Range<usize>>,
) -> usize {
    let rendered = crate::ui::markdown::render(text, t, area.width);
    let total = rendered.lines.len();
    let max_scroll = total.saturating_sub(area.height as usize);
    let in_focus = |i: usize| {
        focus.is_some_and(|r| rendered.src[i].is_some_and(|s| s >= r.start && s < r.end))
    };
    let mut offset = scroll.min(max_scroll);
    if scroll == 0
        && let Some(first) = (0..total).find(|i| in_focus(*i))
    {
        // One line of context above the focused section.
        offset = first.saturating_sub(1).min(max_scroll);
    }
    let visible: Vec<Line> = rendered
        .lines
        .into_iter()
        .enumerate()
        .skip(offset)
        .take(area.height as usize)
        .map(|(i, line)| {
            if in_focus(i) {
                line.style(Style::default().bg(t.bg_row2()))
            } else {
                line
            }
        })
        .collect();
    f.render_widget(Paragraph::new(visible).wrap(Wrap { trim: false }), area);

    // Scroll hint in the top-right corner when there is more.
    if total > area.height as usize {
        let hint = format!(" {}/{} ", offset + 1, max_scroll + 1);
        let w = hint.chars().count() as u16;
        if w < area.width {
            f.render_widget(
                Paragraph::new(Span::styled(hint, Style::default().fg(t.grey_fg()))),
                Rect::new(area.right() - w, area.y, w, 1),
            );
        }
    }
    max_scroll
}

// ---------------------------------------------------------------------------
// powerline statusline
// ---------------------------------------------------------------------------

/// Spend sparkline over the most recent cost-bearing runs, oldest → newest.
fn cost_sparkline(app: &App) -> Option<String> {
    const WINDOW: usize = 12;
    // metas are newest-first; take the window, then flip to chronological.
    let mut costs: Vec<f64> = app
        .metas
        .iter()
        .filter_map(|m| m.total_cost_usd)
        .take(WINDOW)
        .collect();
    costs.reverse();
    crate::output::sparkline(&costs)
}

fn draw_statusline(f: &mut Frame, app: &App, area: Rect) {
    let t = &app.cfg.theme;
    let base = t.bg_statusline();

    let mut left: Vec<Span> = Vec::new();
    left.extend(pl_segment(t, " RITUAL ".into(), t.accent(), base));
    left.extend(pl_segment(
        t,
        format!(" {} {} ", t.icon_branch(), app.branch),
        t.highlight(),
        base,
    ));
    // Middle: status message (yellow) or feature title (grey).
    let middle = match &app.status_msg {
        Some(msg) => Span::styled(
            format!(" {msg}"),
            Style::default().fg(t.attention()).bg(base),
        ),
        None => Span::styled(
            format!(
                " {}",
                app.state
                    .features
                    .get(&app.slug)
                    .map(|f| f.title.clone())
                    .unwrap_or_default()
            ),
            Style::default().fg(t.grey_fg()).bg(base),
        ),
    };
    left.push(middle);

    let mut right: Vec<Span> = Vec::new();
    // A compact spend sparkline over recent runs (older → newer); the shape
    // of the burn at a glance. Suppressed on narrow bars where it'd crowd out
    // the budget/run segments.
    if area.width >= 80
        && let Some(spark) = cost_sparkline(app)
    {
        right.push(Span::styled(
            format!("{spark} "),
            Style::default().fg(t.info()).bg(base),
        ));
    }
    if let Some(budget) = app.cfg.budget_daily_usd {
        let spent = app.today_spend();
        let color = if spent >= budget {
            t.error()
        } else if spent >= 0.75 * budget {
            t.warn()
        } else {
            t.grey_fg()
        };
        right.push(Span::styled(
            format!("${spent:.2}/${budget:.0} "),
            Style::default().fg(color).bg(base),
        ));
    }
    if let Some(stage) = app.running {
        right.extend(pl_segment(
            t,
            format!(
                " {} {} ",
                SPINNER[app.spinner % SPINNER.len()],
                stage.label()
            ),
            t.info(),
            base,
        ));
    }
    // A claude plan fix runs on its own track (independent of `running`).
    if let Some(label) = app.fix_label() {
        right.extend(pl_segment(
            t,
            format!(" {} {} ", SPINNER[app.spinner % SPINNER.len()], label),
            t.attention(),
            base,
        ));
    } else {
        // Triage progress: answers queued for the claude batch.
        let queued = app.queued_auto().len();
        if queued > 0 {
            right.extend(pl_segment(t, format!(" ⚑{queued} "), t.accent(), base));
        }
    }
    let check_seg = match &app.check {
        CheckState::Green => Some((format!(" {} ok ", t.icon_check()), t.ok())),
        CheckState::Red { .. } => Some((format!(" {} RED ", t.icon_check()), t.error())),
        _ => None,
    };
    if let Some((text, bg)) = check_seg {
        right.extend(pl_segment(t, text, bg, base));
    }

    let used = visible_width(&left) + visible_width(&right);
    let mut spans = left;
    if (used as u16) < area.width {
        spans.push(Span::styled(
            " ".repeat(area.width as usize - used),
            Style::default().bg(base),
        ));
    }
    spans.extend(right);
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

// ---------------------------------------------------------------------------
// overlays
// ---------------------------------------------------------------------------

/// The `i` stage-detail overlay: status + when it finished, the last run's
/// cost/model, WHY it is stale, what blocks it, and what running it unlocks.
/// Reads only the cached guidance - the render path never computes.
fn draw_stage_detail(f: &mut Frame, app: &App) {
    let t = &app.cfg.theme;
    let id = app.selected_stage();
    let stage = app
        .state
        .features
        .get(&app.slug)
        .map(|feat| feat.stage(id))
        .unwrap_or_default();
    let (icon, icon_color) = stage_icon_color(t, stage.status);
    let status_word = match stage.status {
        StageStatus::Pending => "pending",
        StageStatus::Running => "running",
        StageStatus::Done => "done",
        StageStatus::Failed => "failed",
        StageStatus::NeedsAttention => "needs attention",
        StageStatus::Skipped => "skipped",
    };

    let mut lines: Vec<Line> = vec![Line::default()];
    let mut head = vec![
        Span::styled(format!(" {icon} "), Style::default().fg(icon_color)),
        Span::styled(
            status_word,
            Style::default().fg(icon_color).add_modifier(Modifier::BOLD),
        ),
    ];
    if let Some(fin) = stage.finished_at {
        head.push(Span::styled(
            format!(
                "  finished {}",
                crate::guidance::rel_time(fin, chrono::Utc::now())
            ),
            Style::default().fg(t.muted()),
        ));
    }
    if !stage.runs.is_empty() {
        head.push(Span::styled(
            format!("  x{} run(s)", stage.runs.len()),
            Style::default().fg(t.comment()),
        ));
    }
    lines.push(Line::from(head));

    // The last run's receipts, straight from its meta.
    if let Some(meta) = stage
        .runs
        .last()
        .and_then(|rid| app.metas.iter().find(|m| &m.run_id == rid))
    {
        let cost = meta
            .total_cost_usd
            .map(|c| format!("${c:.2}"))
            .unwrap_or_else(|| "$-".into());
        let model = meta.model.as_deref().unwrap_or("?");
        lines.push(Line::from(Span::styled(
            format!("   last run: {cost} · {model}"),
            Style::default().fg(t.comment()),
        )));
    }

    let guidance = app.guidance.as_ref().and_then(|g| g.stages.get(&id));
    let stale = guidance.and_then(|g| g.stale);
    let blockers: &[String] = guidance.map(|g| g.blockers.as_slice()).unwrap_or(&[]);
    if let Some(reason) = stale {
        lines.push(Line::default());
        lines.push(Line::from(Span::styled(
            format!(" {} {} - re-run to refresh", t.icon_stale(), reason.text()),
            Style::default().fg(t.warn()),
        )));
    } else if stage.status == StageStatus::Done
        && matches!(
            id,
            crate::state::StageId::DualReview | crate::state::StageId::Coverage
        )
        && stage.fingerprint.is_none()
    {
        lines.push(Line::default());
        lines.push(Line::from(Span::styled(
            " (no tree fingerprint recorded - code staleness unknown)",
            Style::default().fg(t.comment()).add_modifier(Modifier::DIM),
        )));
    }
    if !blockers.is_empty() {
        lines.push(Line::default());
        for b in blockers {
            lines.push(Line::from(Span::styled(
                format!(" ! {b}"),
                Style::default().fg(t.error()),
            )));
        }
    }
    // What the plan will (not) be grounded in - reads only cached guidance.
    if id == crate::state::StageId::Plan
        && let Some(arch) = app.guidance.as_ref().and_then(|g| g.arch)
    {
        use crate::architect::ArchStatus;
        let (text, warn) = match arch {
            ArchStatus::Missing => ("missing - run `ritual architect`", true),
            ArchStatus::Stale => ("stale - run `ritual architect`", true),
            ArchStatus::Fresh => ("fresh", false),
            ArchStatus::Unknown => ("unknown (no fingerprint recorded)", false),
        };
        lines.push(Line::default());
        lines.push(Line::from(Span::styled(
            format!(" architecture map: {text}"),
            Style::default().fg(if warn { t.warn() } else { t.comment() }),
        )));
    }

    lines.push(Line::default());
    let unlocks = PIPELINE
        .iter()
        .skip_while(|s| **s != id)
        .nth(1)
        .map(|s| s.label());
    if let Some(next_label) = unlocks {
        lines.push(Line::from(Span::styled(
            format!(" unlocks: {next_label}"),
            Style::default().fg(t.muted()),
        )));
    }
    let suggested = if !blockers.is_empty() {
        "resolve the blocker first".to_string()
    } else if app.guidance.as_ref().and_then(|g| g.next) == Some(id) {
        "run it now (enter)".to_string()
    } else if stale.is_some() {
        "re-run to refresh (enter)".to_string()
    } else if stage.status == StageStatus::Done {
        "nothing to do here".to_string()
    } else {
        "run when its turn comes (enter runs it anyway)".to_string()
    };
    lines.push(Line::from(Span::styled(
        format!(" suggested: {suggested}"),
        Style::default().fg(t.info()),
    )));

    lines.push(Line::default());
    lines.push(Line::from(vec![
        keycap(t, "enter"),
        Span::styled(" run  ", Style::default().fg(t.comment())),
        keycap(t, "j/k"),
        Span::styled(" stage  ", Style::default().fg(t.comment())),
        keycap(t, "esc/q/i"),
        Span::styled(" close", Style::default().fg(t.comment())),
    ]));

    let area = content_sized_rect(f.area(), &lines, 46);
    let inner = float_panel(f, t, area, &format!("stage · {}", id.label()));
    if inner.height < 2 {
        return;
    }
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
}

/// The finding detail overlay (Enter on the findings tab): everything the
/// findings JSON records about the cursor's finding, wrapped to the panel.
fn draw_finding_detail(f: &mut Frame, app: &App) {
    let t = &app.cfg.theme;
    let Some(af) = app.selected_finding_af() else {
        return; // the list emptied under the overlay; nothing to show
    };
    let finding = &af.finding;
    let area = centered_rect(
        f.area(),
        84.min(f.area().width.saturating_sub(2)).max(1),
        18.min(f.area().height.saturating_sub(2)).max(1),
    );
    let inner = float_panel(f, t, area, "finding");
    if inner.height < 2 {
        return;
    }
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(inner);

    let stage = app
        .findings
        .get(af.file_idx)
        .map(|lf| lf.file.stage.as_str())
        .unwrap_or_default();
    let mut lines: Vec<Line> = vec![Line::from(Span::styled(
        finding.title.clone(),
        Style::default().fg(t.fg()).add_modifier(Modifier::BOLD),
    ))];
    let mut pills: Vec<Span> = Vec::new();
    pills.extend(severity_pill(t, finding.severity));
    pills.push(Span::raw(" "));
    if finding.cross_confirmed() {
        pills.extend(pill(t, " ◆both ".into(), t.ok()));
    } else {
        pills.push(Span::styled("◇single", Style::default().fg(t.warn())));
    }
    pills.push(Span::styled(
        format!("  {}", finding.sources.join("+")),
        Style::default().fg(t.comment()),
    ));
    pills.push(Span::styled(
        format!("  {} ", finding.verdict),
        Style::default().fg(t.info()),
    ));
    pills.push(Span::styled(
        format!("[{stage}]"),
        Style::default().fg(t.comment()),
    ));
    if finding.resolved() {
        let (mark, color) = if finding.action == "fixed" {
            (" ✓fixed", t.ok())
        } else {
            (" ∅dismissed", t.comment())
        };
        pills.push(Span::styled(mark, Style::default().fg(color)));
    } else {
        match finding.answer.as_deref() {
            Some("auto") => pills.push(Span::styled(
                " ⚑A queued for claude",
                Style::default().fg(t.accent()),
            )),
            Some("manual") => pills.push(Span::styled(
                " ⚑M yours to fix",
                Style::default().fg(t.info()),
            )),
            _ => {}
        }
    }
    lines.push(Line::from(pills));
    let location = match (&finding.file, &finding.plan_step) {
        (Some(_), _) => finding.location(),
        (None, Some(step)) => format!("plan step: {step}"),
        _ => String::new(),
    };
    if !location.is_empty() {
        lines.push(Line::from(Span::styled(
            location,
            Style::default().fg(t.info()),
        )));
    }
    if let Some(reason) = &finding.reason {
        lines.push(Line::from(Span::styled(
            format!("reason: {reason}"),
            Style::default().fg(t.warn()),
        )));
    }
    if app.is_anchor_lost(&af) {
        lines.push(Line::from(Span::styled(
            "⚓ anchor lost - step no longer found in plan.md",
            Style::default().fg(t.warn()),
        )));
    }
    if finding.answer.is_none()
        && !finding.resolved()
        && let Some(rec) = crate::findings::recommend(finding)
    {
        use crate::findings::Recommendation as R;
        let text = match rec {
            R::QueueAuto => "recommended: queue for claude - confirmed plan finding",
            R::QueueManual => "recommended: fix manually - confirmed code finding",
            R::Archive => "recommended: archive - resolution already recorded in action",
            R::Dismiss(_) => "recommended: dismiss - retracted by the review itself",
            R::NeedsYou => "recommended: your judgment - no safe default",
        };
        lines.push(Line::from(Span::styled(
            text,
            Style::default().fg(t.comment()),
        )));
    }
    lines.push(Line::default());
    if !finding.scenario.is_empty() {
        lines.push(Line::from(Span::styled(
            finding.scenario.clone(),
            Style::default().fg(t.fg()),
        )));
    }
    if let Some(snippet) = &finding.snippet {
        lines.push(Line::default());
        for l in snippet.lines() {
            lines.push(Line::from(Span::styled(
                format!("▏ {l}"),
                Style::default().fg(t.comment()).add_modifier(Modifier::DIM),
            )));
        }
    }
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), rows[0]);

    let mut footer: Vec<Span> = Vec::new();
    let cap = |footer: &mut Vec<Span<'_>>, key: &'static str, label: String| {
        footer.push(keycap(t, key));
        footer.push(Span::styled(
            format!(" {label}  "),
            Style::default().fg(t.comment()),
        ));
    };
    for (key, label) in [("f", "fix"), ("d", "dismiss…")] {
        cap(&mut footer, key, label.into());
    }
    // F queues BOTH kinds: plan findings for the plan-fix batch, code
    // findings (file-anchored) for the code-fix batch.
    let plan_finding = finding.file.is_none() && finding.plan_step.is_some();
    if let Some(label) = app.fix_label() {
        footer.push(Span::styled(
            format!("{} {label}  ", SPINNER[app.spinner % SPINNER.len()]),
            Style::default().fg(t.attention()),
        ));
    } else {
        let f_label = if finding.answer.as_deref() == Some("auto") {
            "apply/unqueue"
        } else if plan_finding {
            "queue claude"
        } else {
            "queue code-fix"
        };
        cap(&mut footer, "F", f_label.into());
    }
    cap(&mut footer, "m", "manual".into());
    if app.fix_revertable() {
        cap(&mut footer, "u", "revert".into());
    }
    for (key, label) in [("o", "nvim"), ("e", "editor"), ("j/k", "next")] {
        cap(&mut footer, key, label.into());
    }
    cap(&mut footer, "esc/q/enter", "close".into());
    f.render_widget(Paragraph::new(Line::from(footer)), rows[1]);
}

fn draw_palette(f: &mut Frame, app: &App) {
    let t = &app.cfg.theme;
    let Some(p) = &app.palette else { return };
    let matches = app.palette_filtered();
    let height = (matches.len() as u16 + 4).clamp(5, 16);
    let area = centered_rect(f.area(), 56, height);
    let inner = float_panel(f, t, area, "command");

    let mut lines = Vec::new();
    // Telescope prompt row on one_bg.
    let prompt = vec![
        Span::styled(
            format!(" {} ", t.icon_prompt()),
            Style::default().fg(t.highlight()).bg(t.bg_row()),
        ),
        Span::styled(p.input.clone(), Style::default().fg(t.fg()).bg(t.bg_row())),
        Span::styled("▏", Style::default().fg(t.info()).bg(t.bg_row())),
    ];
    lines.push(fill_row(prompt, inner.width, t.bg_row()));
    lines.push(Line::default());

    let visible = (inner.height as usize).saturating_sub(2);
    let first = p.selected.saturating_sub(visible.saturating_sub(1));
    for (i, (label, _)) in matches.iter().enumerate().skip(first).take(visible) {
        if i == p.selected {
            // PmenuSel.
            let spans = vec![Span::styled(
                format!("  {label}"),
                Style::default()
                    .fg(t.on_accent())
                    .bg(t.bg_selection())
                    .add_modifier(Modifier::BOLD),
            )];
            lines.push(fill_row(spans, inner.width, t.bg_selection()));
        } else {
            lines.push(Line::from(Span::styled(
                format!("  {label}"),
                Style::default().fg(t.muted()),
            )));
        }
    }
    if matches.is_empty() {
        lines.push(Line::from(Span::styled(
            "  no matching command",
            Style::default().fg(t.comment()),
        )));
    }
    f.render_widget(Paragraph::new(lines), inner);
}

/// One row in a which-key section: a real Action (keycaps resolved from the
/// live keymap; unbound-but-labeled actions render with a `:` keycap so
/// palette-only entries are never silently dropped) or a literal hint for
/// modal keys that aren't Actions (esc/enter/q).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WkEntry {
    Act(Action),
    Lit {
        keys: &'static str,
        desc: &'static str,
    },
}

/// The ordered key sections to show in the which-key overlay for the CURRENT
/// context: the actions specific to the active tab (or the modal overlay)
/// FIRST, then the tab-agnostic `actions`, `global` and `move` sections.
/// Mirrors what `dispatch`/`nav`/the modal handlers actually honor per
/// context - the honesty test in tests/ui_snapshots.rs pins these tables.
pub fn whichkey_sections(app: &App) -> Vec<(&'static str, Vec<WkEntry>)> {
    use Action::*;
    use WkEntry::{Act, Lit};
    // The finding-detail overlay is its own modal: `detail_input` honors only a
    // subset (the finding actions + up/down + help/close), swallowing tabs,
    // palette, settings, scrolling and feature-nav - so advertise ONLY what
    // works here, not the base-layer global/move sections.
    if app.stage_detail {
        return vec![
            (
                "stage",
                vec![
                    Lit {
                        keys: "enter",
                        desc: "run this stage",
                    },
                    Lit {
                        keys: "esc/q/i",
                        desc: "close",
                    },
                ],
            ),
            ("move", vec![Act(Up), Act(Down)]),
        ];
    }
    if app.finding_detail {
        return vec![
            (
                "finding",
                vec![
                    Act(FindingFix),
                    Act(FindingDismiss),
                    Act(FindingClaudeFix),
                    Act(FindingManual),
                    Act(DocUndo),
                    Act(NvimOpen),
                    Act(OpenEditor),
                    Lit {
                        keys: "esc/q/enter",
                        desc: "close",
                    },
                ],
            ),
            ("move", vec![Act(Up), Act(Down)]),
        ];
    }
    // Chat sidebar mode (input unfocused): `chat_unfocused_input` honors
    // exactly these. A FOCUSED chat never reaches help - `?` types.
    if app.chat.as_ref().is_some_and(|c| !c.input_focused) {
        return vec![(
            "chat",
            vec![
                Act(Up),
                Act(Down),
                Act(FocusRight),
                Act(SpecChat),
                Act(StageDetail),
                Act(Help),
                Lit {
                    keys: "ctrl+x",
                    desc: "cancel edit",
                },
                Lit {
                    keys: "esc/q",
                    desc: "close chat",
                },
            ],
        )];
    }
    // Per-tab context: ONLY what is specific to this tab. Finding-targeted
    // keys (o/e and the triage set) live on Findings; Confirm appears
    // everywhere because Enter launches the selected stage from every tab
    // (and opens the finding on Findings).
    let ctx: (&'static str, Vec<WkEntry>) = match app.tab {
        Tab::Live => ("run", vec![Act(Confirm)]),
        Tab::Findings => (
            "findings",
            vec![
                Act(Confirm),
                Act(FindingFix),
                Act(FindingDismiss),
                Act(FindingClaudeFix),
                Act(FindingManual),
                Act(QueueAllCode),
                Act(TriageAll),
                Act(FindingsApply),
                Act(ToggleResolved),
                Act(Filter),
                Act(NvimOpen),
                Act(OpenEditor),
            ],
        ),
        Tab::Plan => ("plan", vec![Act(Confirm), Act(ResetPlan)]),
        Tab::History => ("history", vec![Act(Confirm), Act(Filter)]),
        Tab::Guide => ("guide", vec![Act(Confirm)]),
    };
    // Tab-agnostic actions: verified to work identically on every tab
    // (no tab guard in dispatch). Architect is chordless: renders as `:`.
    let shared: Vec<WkEntry> = vec![
        Act(StageDetail),
        Act(Cancel),
        Act(CheckFast),
        Act(CheckFull),
        Act(Takeover),
        Act(SpecChat),
        Act(Architect),
        Act(DocUndo),
        Act(NvimQuickfix),
    ];
    vec![
        ctx,
        ("actions", shared),
        (
            "global",
            vec![
                Act(Quit),
                Act(Help),
                Act(Palette),
                Act(NextTab),
                // The five tab jumps compress to one row (they'd crowd out
                // real context on narrow frames).
                Lit {
                    keys: "1-5",
                    desc: "jump to tab",
                },
                Act(Refresh),
                Act(Settings),
            ],
        ),
        (
            "move",
            vec![
                Act(Up),
                Act(Down),
                Act(FocusLeft),
                Act(FocusRight),
                Act(ScrollTop),
                Act(Follow),
                Act(FeaturePrev),
                Act(FeatureNext),
            ],
        ),
    ]
}

/// The which-key help overlay: the keys/actions live in the CURRENT view,
/// generated from the RESOLVED keymap (so it reflects `[keys]` rebinds) with
/// each action's human label. Packs sections into a which-key grid: one column
/// when it fits the frame height, wrapping into more columns when it doesn't, so
/// the context section is never clipped. Press the help key again or Esc to close.
fn draw_help(f: &mut Frame, app: &App) {
    let t = &app.cfg.theme;
    let header_style = Style::default()
        .fg(t.grey_fg())
        .add_modifier(Modifier::BOLD);
    let desc_style = Style::default().fg(t.muted());

    // One rendered block per non-empty section: a header line + one row per
    // entry. An Action with a bound chord shows its keycaps; an UNBOUND one
    // with a label shows the `:` keycap + "(palette)" instead of vanishing
    // (the old silent drop hid FindingsApply entirely); Lit rows render
    // their literal keys (modal esc/enter hints that aren't Actions).
    let mut blocks: Vec<(Vec<Line>, usize)> = Vec::new();
    for (title, entries) in whichkey_sections(app) {
        let mut ls: Vec<Line> = Vec::new();
        let mut w = title.chars().count() + 1;
        for entry in entries {
            let (keys, desc) = match entry {
                WkEntry::Act(a) => {
                    let mut caps: Vec<String> = app
                        .cfg
                        .keymap
                        .chords_for(a)
                        .iter()
                        .map(|c| c.caption())
                        .collect();
                    let desc = describe(a);
                    if caps.is_empty() {
                        if desc.is_empty() {
                            continue; // dynamic action with no label: nothing to show
                        }
                        // Unbound-but-labeled: the `:` keycap says "via the
                        // palette" - the row must never vanish silently.
                        (":".to_string(), desc.to_string())
                    } else {
                        // Show the shortest keycaps (the modifier-less
                        // primaries) and elide further aliases: an action
                        // with four chords (focus-left) would otherwise blow
                        // the column budget and clip its description.
                        caps.sort_by_key(|c| c.chars().count());
                        let shown = if caps.len() > 2 {
                            format!("{} / {} …", caps[0], caps[1])
                        } else {
                            caps.join(" / ")
                        };
                        (shown, desc.to_string())
                    }
                }
                WkEntry::Lit { keys, desc } => (keys.to_string(), desc.to_string()),
            };
            w = w.max(keys.chars().count() + desc.chars().count() + 5);
            ls.push(Line::from(vec![
                Span::raw(" "),
                keycap(t, &keys),
                Span::raw(" "),
                Span::styled(desc, desc_style),
            ]));
        }
        if ls.is_empty() {
            continue;
        }
        ls.insert(
            0,
            Line::from(Span::styled(format!(" {title}"), header_style)),
        );
        blocks.push((ls, w));
    }

    // Pack blocks top-to-bottom into columns; start a new column when the
    // current one would overflow the frame's inner height.
    let avail_h = (f.area().height.saturating_sub(2) as usize).max(1);
    let gap = 2usize;
    let mut cols: Vec<Vec<Line>> = vec![Vec::new()];
    let mut cols_w: Vec<usize> = vec![0];
    for (ls, w) in blocks {
        let ci = cols.len() - 1;
        let sep = usize::from(!cols[ci].is_empty());
        if !cols[ci].is_empty() && cols[ci].len() + sep + ls.len() > avail_h {
            cols.push(Vec::new());
            cols_w.push(0);
        }
        let ci = cols.len() - 1;
        if !cols[ci].is_empty() {
            cols[ci].push(Line::default());
        }
        cols[ci].extend(ls);
        cols_w[ci] = cols_w[ci].max(w);
    }

    let total_w: usize = cols_w.iter().sum::<usize>() + gap * cols.len().saturating_sub(1);
    let max_h = cols.iter().map(|c| c.len()).max().unwrap_or(1);
    let width = (total_w as u16 + 4).min(f.area().width);
    let height = (max_h as u16 + 2).min(f.area().height);
    let area = centered_rect(f.area(), width, height);
    let inner = float_panel(f, t, area, "keys");

    // Render each column into its own slice of the inner rect.
    let mut x = inner.x + 1;
    for (i, col) in cols.into_iter().enumerate() {
        if x >= inner.x + inner.width {
            break;
        }
        let remaining = inner.x + inner.width - x;
        let cw = (cols_w[i] as u16).min(remaining);
        f.render_widget(Paragraph::new(col), Rect::new(x, inner.y, cw, inner.height));
        x += cw + gap as u16;
    }
}

/// The `S` settings editor: grouped catalog rows with effective values and
/// dim source tags. Toggles/cycles apply on Enter; numeric/text rows open an
/// inline edit line at the bottom of the panel.
fn draw_settings(f: &mut Frame, app: &App) {
    let t = &app.cfg.theme;
    let Some(s) = &app.settings else { return };
    let catalog = crate::settings::CATALOG;

    // Display rows: a group header line wherever the group changes; remember
    // which display row each catalog entry lands on for the scroll window.
    let mut rows: Vec<(Option<usize>, &'static str)> = Vec::new();
    let mut last_group = "";
    for (i, def) in catalog.iter().enumerate() {
        if def.group != last_group {
            rows.push((None, def.group));
            last_group = def.group;
        }
        rows.push((Some(i), def.group));
    }

    let want_h = rows.len() as u16 + 4; // top pad + rows + key footer + borders
    let area = centered_rect(
        f.area(),
        66.min(f.area().width.saturating_sub(2)).max(1),
        want_h.min(f.area().height.saturating_sub(2)).max(1),
    );
    let inner = float_panel(f, t, area, "settings - project config");
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    let sel_row = rows
        .iter()
        .position(|(ci, _)| *ci == Some(s.selected))
        .unwrap_or(0);
    // The inline edit block pins to the panel bottom: prompt, hint, error.
    // One more line is always reserved for the key footer (the settings
    // overlay swallows `?`, so this is its only key documentation).
    let reserved = if s.edit.is_some() { 3 } else { 0 };
    let visible = (inner.height as usize).saturating_sub(2 + reserved);
    let first = sel_row.saturating_sub(visible.saturating_sub(1));

    let mut lines = vec![Line::default()];
    for (ci, group) in rows.iter().skip(first).take(visible) {
        match ci {
            None => {
                lines.push(Line::from(Span::styled(
                    format!(" {group}"),
                    Style::default()
                        .fg(t.grey_fg())
                        .add_modifier(Modifier::BOLD),
                )));
            }
            Some(i) => {
                let def = &catalog[*i];
                let value = (def.get)(&app.cfg);
                let tag = s.sources.get(*i).copied().unwrap_or("default");
                let selected = *i == s.selected;
                let bg = if selected {
                    t.bg_selection()
                } else {
                    t.bg_float()
                };
                let label_style = if selected {
                    Style::default()
                        .fg(t.on_accent())
                        .bg(bg)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(t.muted())
                };
                let value_style = if value.is_some() {
                    Style::default().fg(t.fg()).bg(bg)
                } else {
                    Style::default().fg(t.comment()).bg(bg)
                };
                let left = vec![Span::styled(format!("  {}", def.label), label_style)];
                let chip = vec![
                    Span::styled(value.unwrap_or_else(|| "-".into()), value_style),
                    Span::styled(
                        format!(" ({tag}) "),
                        Style::default()
                            .fg(t.comment())
                            .bg(bg)
                            .add_modifier(Modifier::DIM),
                    ),
                ];
                lines.push(fill_row_chip(left, chip, inner.width, bg));
            }
        }
    }
    if let Some(e) = &s.edit {
        let def = &catalog[s.selected.min(catalog.len().saturating_sub(1))];
        let target = (inner.height as usize).saturating_sub(reserved);
        while lines.len() < target {
            lines.push(Line::default());
        }
        let prompt = vec![
            Span::styled(
                format!(" {} {}: ", t.icon_prompt(), def.label),
                Style::default()
                    .fg(t.highlight())
                    .bg(t.bg_row())
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(e.input.clone(), Style::default().fg(t.fg()).bg(t.bg_row())),
            Span::styled("▏", Style::default().fg(t.info()).bg(t.bg_row())),
        ];
        lines.push(fill_row(prompt, inner.width, t.bg_row()));
        lines.push(Line::from(Span::styled(
            format!("  {}", setting_hint(def)),
            Style::default().fg(t.comment()).add_modifier(Modifier::DIM),
        )));
        if let Some(err) = &e.error {
            lines.push(Line::from(Span::styled(
                format!("  ✗ {err}"),
                Style::default().fg(t.error()),
            )));
        }
    }
    // Key footer, pinned to the bottom row.
    while (lines.len() as u16) < inner.height.saturating_sub(1) {
        lines.push(Line::default());
    }
    lines.truncate(inner.height.saturating_sub(1) as usize);
    lines.push(Line::from(Span::styled(
        " j/k move · enter toggle/edit · g/G first/last · S/esc close",
        Style::default().fg(t.comment()).add_modifier(Modifier::DIM),
    )));
    f.render_widget(Paragraph::new(lines), inner);
}

/// One-line allowed-values hint under the settings edit prompt.
fn setting_hint(def: &crate::settings::SettingDef) -> String {
    use crate::settings::SettingKind::*;
    match def.kind {
        F64 => format!("{} - number > 0", def.doc),
        OptF64 => format!("{} - number > 0, empty = unset", def.doc),
        U64 => format!("{} - whole number ≥ 1", def.doc),
        Bool | Text => def.doc.to_string(),
        OptText => format!("{} - empty = unset", def.doc),
        Enum(vs) => format!("{} - {}", def.doc, vs.join("/")),
        OptEnum(vs) => format!("{} - {}, empty = unset", def.doc, vs.join("/")),
    }
}

/// The `implement` launch overlay: an interactive `claude --resume` can't be
/// handed an opening message, so surface the suggested prompt for the user to
/// copy/paste once the session opens. `enter` commits the handover.
fn draw_implement_hint(f: &mut Frame, app: &App) {
    let t = &app.cfg.theme;
    let Some(hint) = &app.implement_hint else {
        return;
    };
    let area = centered_rect(
        f.area(),
        66.min(f.area().width.saturating_sub(2)).max(1),
        13.min(f.area().height.saturating_sub(2)).max(1),
    );
    let inner = float_panel(f, t, area, "implement - resume + paste to start");
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    let lead = if hint.resuming {
        "The tests-red session reopens with its full context."
    } else {
        "Pick the tests-red session from the list that opens."
    };
    let mut lines = vec![
        Line::default(),
        Line::from(Span::styled(
            format!("  {lead}"),
            Style::default().fg(t.muted()),
        )),
    ];
    // Keys line, assembled to match whichever branch we render.
    let key = |k: &str| {
        Span::styled(
            k.to_string(),
            Style::default().fg(t.accent()).add_modifier(Modifier::BOLD),
        )
    };
    let dim = |s: &str| Span::styled(s.to_string(), Style::default().fg(t.muted()));
    if hint.copied {
        // Copied - DON'T render the prompt: a mouse-drag over it grabs the
        // float border + sidebar behind it, and copy-on-select would clobber
        // the clean clipboard. Nothing to select, so paste just works.
        lines.push(Line::from(Span::styled(
            "  ✓ the implement instruction is on your clipboard.",
            Style::default().fg(t.ok()),
        )));
        lines.push(Line::default());
        lines.push(Line::from(vec![
            dim("  Press "),
            key("[enter]"),
            dim(" to open it, then paste ("),
            Span::styled("Ctrl+Shift+V", Style::default().fg(t.highlight())),
            dim(" / middle-click)."),
        ]));
        lines.push(Line::default());
        lines.push(Line::from(vec![
            key("  [enter]"),
            dim(" open    "),
            key("[c]"),
            dim(" copy again    "),
            key("[esc]"),
            dim(" cancel"),
        ]));
    } else {
        // Clipboard unreachable - fall back to showing the prompt to copy by
        // hand (press c to retry the clipboard).
        lines.push(Line::from(dim(
            "  Couldn't reach a clipboard. Copy this and paste it in:",
        )));
        lines.push(Line::default());
        lines.push(Line::from(Span::styled(
            crate::stages::IMPLEMENT_PROMPT,
            Style::default().fg(t.highlight()),
        )));
        lines.push(Line::default());
        lines.push(Line::from(vec![
            key("  [enter]"),
            dim(" open    "),
            key("[c]"),
            dim(" retry copy    "),
            key("[esc]"),
            dim(" cancel"),
        ]));
    }
    f.render_widget(
        Paragraph::new(lines).wrap(ratatui::widgets::Wrap { trim: false }),
        inner,
    );
}

/// Size a confirm float to its content so the bottom key-hint row is never
/// clipped: wide enough for the longest line when the frame allows (no
/// wrapping), and when the frame is narrower, tall enough for the
/// word-wrapped rows.
fn content_sized_rect(frame: Rect, lines: &[Line], min_width: u16) -> Rect {
    let max_w = frame.width.saturating_sub(2).max(3);
    let content_w = lines.iter().map(|l| l.width()).max().unwrap_or(1) as u16 + 2;
    let width = content_w.clamp(min_width.min(max_w), max_w);
    let inner_w = usize::from(width.saturating_sub(2).max(1));
    let rows: u16 = lines.iter().map(|l| wrapped_rows(l, inner_w)).sum();
    centered_rect(
        frame,
        width,
        (rows + 2).min(frame.height.saturating_sub(2)).max(1),
    )
}

/// Greedy word-wrap `text` into rows no wider than `width` columns. The first
/// row starts at column 0; every continuation row is prefixed with `indent`
/// spaces (a hanging indent so a wrapped sentence reads as one block). A word
/// longer than the row budget hard-splits. Used for the narrow sidebar
/// guidance strip, where a single `fill_row` clips the sentence mid-word.
fn wrap_plain(text: &str, width: usize, indent: usize) -> Vec<String> {
    let width = width.max(1);
    let indent = indent.min(width - 1);
    let cont_budget = width - indent; // usable columns on a continuation row
    // Break into chunks that fit any row (hard-split words wider than a
    // continuation row) so a chunk placed on a fresh line always fits.
    let mut chunks: Vec<String> = Vec::new();
    for word in text.split_whitespace() {
        let chars: Vec<char> = word.chars().collect();
        let mut i = 0;
        while chars.len() - i > cont_budget {
            chunks.push(chars[i..i + cont_budget].iter().collect());
            i += cont_budget;
        }
        chunks.push(chars[i..].iter().collect());
    }
    let mut rows: Vec<String> = Vec::new();
    let mut line = String::new();
    for chunk in chunks {
        let budget = if rows.is_empty() { width } else { cont_budget };
        let projected = if line.is_empty() {
            chunk.chars().count()
        } else {
            line.chars().count() + 1 + chunk.chars().count()
        };
        if !line.is_empty() && projected > budget {
            rows.push(std::mem::take(&mut line));
        }
        if line.is_empty() {
            line = chunk;
        } else {
            line.push(' ');
            line.push_str(&chunk);
        }
    }
    rows.push(line);
    for (i, row) in rows.iter_mut().enumerate() {
        if i > 0 {
            *row = format!("{}{row}", " ".repeat(indent));
        }
    }
    rows
}

/// Rows one Line occupies under greedy word wrap at `width` columns (mirrors
/// ratatui's WordWrapper for the plain prose these floats hold; a word wider
/// than the panel char-wraps across rows).
fn wrapped_rows(line: &Line, width: usize) -> u16 {
    if width == 0 {
        return 1;
    }
    let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
    let mut rows: u16 = 1;
    let mut cur = 0usize;
    // Tokens keep their REAL widths, including whitespace runs: keycap hint
    // lines render with multi-space gaps and ratatui wraps with
    // `Wrap { trim: false }`, so collapsing runs to one space undercounts and
    // the float clips its last row on narrow frames.
    let mut iter = text.chars().peekable();
    while let Some(&first) = iter.peek() {
        let ws = first.is_whitespace();
        let mut w = 0usize;
        while iter.peek().is_some_and(|c| c.is_whitespace() == ws) {
            iter.next();
            w += 1;
        }
        if ws {
            // Spaces fill (and can end) the current row but never open one.
            cur = (cur + w).min(width);
        } else if w > width {
            // A word longer than the row hard-wraps mid-word.
            if cur > 0 {
                rows += 1;
            }
            let full = (w - 1) / width;
            rows += full as u16;
            cur = w - full * width;
        } else if cur + w > width {
            rows += 1;
            cur = w;
        } else {
            cur += w;
        }
    }
    rows
}

/// The `t` triage confirm: what one `y` stages - dispositions only, the
/// plan still mutates exclusively through F-apply.
fn draw_triage_confirm(f: &mut Frame, app: &App) {
    let t = &app.cfg.theme;
    let Some(c) = &app.triage_confirm else { return };
    let total = c.archive + c.queue_auto + c.queue_manual + c.dismiss;
    let mut lines = vec![Line::from(Span::styled(
        format!("apply recommended triage to {total} finding(s)?"),
        Style::default().fg(t.fg()).add_modifier(Modifier::BOLD),
    ))];
    lines.push(Line::from(Span::styled(
        format!(
            "✓ archive {} (prose→reason) · ⚑A {} · ⚑M {} · ∅ dismiss {}",
            c.archive, c.queue_auto, c.queue_manual, c.dismiss
        ),
        Style::default().fg(t.comment()),
    )));
    if c.needs_you > 0 {
        lines.push(Line::from(Span::styled(
            format!("{} need your judgment - untouched", c.needs_you),
            Style::default().fg(t.warn()),
        )));
    }
    lines.push(Line::from(vec![
        keycap(t, "y"),
        Span::raw(" apply  "),
        keycap(t, "esc"),
        Span::raw(" cancel"),
    ]));
    // Size the panel to the content: a fixed height clips the y/esc key
    // line off the bottom whenever the optional warning line is present.
    let area = content_sized_rect(f.area(), &lines, 62);
    let inner = float_panel(f, t, area, "apply recommended triage");
    if inner.height == 0 {
        return;
    }
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
}

/// The F-apply confirm: what one `y` will send to claude, and how degraded
/// the mechanical gate would be.
fn draw_apply_confirm(f: &mut Frame, app: &App) {
    let t = &app.cfg.theme;
    let Some(c) = &app.apply_confirm else { return };
    // One type per apply: plan findings (section-gated, u-revertable) run
    // first; code findings run on the next apply.
    let headline = if c.plan_count > 0 {
        format!(
            "apply {} plan answer(s) via claude - ONE run?",
            c.plan_count
        )
    } else {
        format!(
            "fix {} code finding(s) - one run + check.sh + re-review?",
            c.code_count
        )
    };
    let mut lines = vec![Line::from(Span::styled(
        headline,
        Style::default().fg(t.fg()).add_modifier(Modifier::BOLD),
    ))];
    if c.plan_count == 0 && c.code_count > 0 {
        lines.push(Line::from(Span::styled(
            "a passing fix stays in your working tree - review with git",
            Style::default().fg(t.comment()),
        )));
    }
    let mut notes = Vec::new();
    if c.plan_count > 0 && c.code_count > 0 {
        notes.push(format!("{} code finding(s) apply next", c.code_count));
    }
    if c.skipped_other_features > 0 {
        notes.push(format!(
            "{} on other features skipped",
            c.skipped_other_features
        ));
    }
    if c.anchor_lost > 0 {
        notes.push(format!(
            "{} anchor(s) lost → whole-plan scope, gate off",
            c.anchor_lost
        ));
    }
    if !notes.is_empty() {
        lines.push(Line::from(Span::styled(
            notes.join(" · "),
            Style::default().fg(t.warn()),
        )));
    }
    let mut keys: Vec<Span> = vec![keycap(t, "y"), Span::raw(" apply  ")];
    keys.push(keycap(t, "u"));
    keys.push(Span::raw(" unqueue this  "));
    keys.push(keycap(t, "esc"));
    keys.push(Span::raw(" cancel"));
    lines.push(Line::from(keys));
    // Size the panel to the content: a fixed height clips the y/u/esc key
    // line off the bottom whenever an optional note line is present.
    let area = content_sized_rect(f.area(), &lines, 58);
    let inner = float_panel(f, t, area, "apply answers");
    if inner.height == 0 {
        return;
    }
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
}

/// `d`'s one-line reason input (enter = commit, empty ok; esc = cancel).
fn draw_dismiss_prompt(f: &mut Frame, app: &App) {
    let t = &app.cfg.theme;
    let Some(p) = &app.dismiss_prompt else { return };
    let area = centered_rect(
        f.area(),
        64.min(f.area().width.saturating_sub(2)).max(1),
        4.min(f.area().height.saturating_sub(2)).max(1),
    );
    let inner = float_panel(f, t, area, "dismiss - reason (enter = none · esc = cancel)");
    if inner.height == 0 {
        return;
    }
    let mut lines = vec![Line::from(Span::styled(
        first_words_pane(&p.title, inner.width),
        Style::default().fg(t.comment()),
    ))];
    lines.push(Line::from(vec![
        Span::styled(
            format!(" {} ", t.icon_prompt()),
            Style::default().fg(t.highlight()).bg(t.bg_row()),
        ),
        Span::styled(
            format!("{} ", p.input),
            Style::default().fg(t.fg()).bg(t.bg_row()),
        ),
        Span::styled("▏", Style::default().fg(t.accent()).bg(t.bg_row())),
    ]));
    f.render_widget(Paragraph::new(lines), inner);
}

/// Clip a title to the panel width (…-terminated), char-safe.
fn first_words_pane(s: &str, width: u16) -> String {
    let max = (width as usize).saturating_sub(2).max(1);
    if s.chars().count() <= max {
        return s.to_string();
    }
    let clipped: String = s.chars().take(max.saturating_sub(1)).collect();
    format!("{clipped}…")
}

fn draw_confirm_quit(f: &mut Frame, t: &Theme) {
    let area = centered_rect(f.area(), 44, 3);
    let inner = float_panel(f, t, area, "confirm");
    f.render_widget(
        Paragraph::new(Span::styled(
            "a run is active; quit anyway? (y/n)",
            Style::default().fg(t.attention()),
        ))
        .alignment(Alignment::Center),
        inner,
    );
}

fn draw_reset_confirm(f: &mut Frame, t: &Theme) {
    let area = centered_rect(f.area(), 64, 5);
    let inner = float_panel(f, t, area, "reset plan");
    f.render_widget(
        Paragraph::new(vec![
            Line::from(Span::styled(
                "reset the plan back to the spec? (y/n)",
                Style::default().fg(t.attention()),
            )),
            Line::from("deletes plan.md, resets plan..coverage, clears plan findings"),
            Line::from("your code and git history are untouched"),
        ])
        .alignment(Alignment::Center),
        inner,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::Color;

    #[test]
    fn fill_row_chip_pads_truncates_and_degrades() {
        let spans = vec![Span::raw("abcde")]; // 5 wide
        let chip = vec![Span::raw("XY")]; // 2 wide

        // Fits: middle padded so the chip ends at `width`.
        let line = fill_row_chip(spans.clone(), chip.clone(), 10, Color::Reset);
        assert_eq!(visible_width(&line.spans), 10);
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "abcde   XY");

        // Minimum one-space gap still fits untruncated.
        let line = fill_row_chip(spans.clone(), chip.clone(), 8, Color::Reset);
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "abcde XY");

        // Tight: the CHIP wins - content truncates with an ellipsis.
        let line = fill_row_chip(spans.clone(), chip.clone(), 7, Color::Reset);
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "abc… XY");
        assert_eq!(visible_width(&line.spans), 7);

        // Degenerate width: nothing left for content -> plain fill.
        let line = fill_row_chip(spans.clone(), chip, 5, Color::Reset);
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "abcde");

        // Empty chip degrades to plain fill.
        let line = fill_row_chip(spans, vec![], 9, Color::Reset);
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "abcde    ");
    }

    #[test]
    fn fill_row_chip_survives_an_exact_fit_multi_span_row() {
        // Previous spans consuming EXACTLY the budget used to underflow
        // `budget - used - 1` (debug panic / release row overflow) when the
        // next span arrived. width 10, chip 2 -> budget 7.
        let spans = vec![Span::raw("abc"), Span::raw("defg"), Span::raw("h")];
        let chip = vec![Span::raw("XY")];
        let line = fill_row_chip(spans, chip, 10, Color::Reset);
        assert!(
            visible_width(&line.spans) <= 10,
            "row must never overflow its width"
        );
    }

    #[test]
    fn wrapped_rows_counts_multi_space_runs_like_the_renderer() {
        // Keycap hint lines render with multi-space gaps and ratatui wraps
        // with trim:false; collapsing runs undercounted rows and clipped the
        // last hint line on narrow frames.
        // 22 chars once the double gaps are counted; the collapse-to-one-
        // space math saw 17 and predicted ONE row at width 20 - the real
        // renderer wraps to two, clipping the float's last line.
        let line = Line::from("aa  bb  cc  dd  ee  ff");
        assert_eq!(wrapped_rows(&line, 20), 2);
        // Zero width must not divide-by-zero.
        assert_eq!(wrapped_rows(&line, 0), 1);
        // Plain single-space text is unchanged by the rewrite.
        assert_eq!(wrapped_rows(&Line::from("ab cd"), 5), 1);
        assert_eq!(wrapped_rows(&Line::from("ab cd"), 4), 2);
    }

    #[test]
    fn wrap_plain_wraps_the_sidebar_note_without_clipping() {
        // The real warning that was clipped to "62 confirmed finding(s) op":
        // 26-col content budget (SIDEBAR_W 28 minus the 2-space lead).
        let rows = wrap_plain("62 confirmed finding(s) open (tab 2)", 26, 2);
        assert!(rows.len() >= 2, "must wrap, got {rows:?}");
        // No row exceeds the budget, so nothing is clipped by the render.
        assert!(rows.iter().all(|r| r.chars().count() <= 26), "{rows:?}");
        // The whole message survives once the hanging indent is stripped.
        let joined: String = rows
            .iter()
            .map(|r| r.trim_start())
            .collect::<Vec<_>>()
            .join(" ");
        assert_eq!(joined, "62 confirmed finding(s) open (tab 2)");
        // First row carries no indent; continuations hang under it.
        assert!(!rows[0].starts_with(' '));
        assert!(rows[1].starts_with("  "));
    }

    #[test]
    fn wrap_plain_hard_splits_an_overlong_word_and_survives_degenerate_width() {
        // A word wider than the row hard-splits rather than overflowing.
        let rows = wrap_plain("supercalifragilistic", 8, 2);
        assert!(rows.iter().all(|r| r.chars().count() <= 8), "{rows:?}");
        assert_eq!(rows.concat().replace(' ', ""), "supercalifragilistic");
        // Degenerate widths never panic (indent clamps below width).
        assert_eq!(wrap_plain("x", 1, 4), vec!["x".to_string()]);
        assert_eq!(wrap_plain("", 10, 2), vec![String::new()]);
    }
}
