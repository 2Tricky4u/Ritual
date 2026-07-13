//! All drawing. Pure: reads App, writes the frame. No state mutations.
//!
//! Visual language: NvChad/base46, borderless panels separated by background
//! shades (darker sidebar, statusline_bg bottom bar), powerline statusline
//! with the user's live separator glyphs ( / ), PmenuSel purple selection,
//! tabufline pills, nvdash greeter, telescope-style palette, which-key help.

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph, Wrap};

use crate::findings::Severity;
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

/// A which-key style keycap chip.
fn keycap<'a>(t: &Theme, key: &'a str) -> Span<'a> {
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
    // Chat mode needs the width for its own preview|chat split, so the sidebar
    // only survives on wide terminals.
    let sidebar_min = if app.chat.is_some() {
        CHAT_SIDEBAR_MIN_TERM_W
    } else {
        MIN_SIDEBAR_TERM_W
    };
    if content.width >= sidebar_min {
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
    if app.dismiss_prompt.is_some() {
        draw_dismiss_prompt(f, app);
    }
    if app.apply_confirm.is_some() {
        draw_apply_confirm(f, app);
    }
    if app.show_help {
        draw_help(f, t);
    }
    if app.palette.is_some() {
        draw_palette(f, app);
    }
    if app.confirm_quit {
        draw_confirm_quit(f, t);
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

    // PIPELINE
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
        let suffix = if attempts > 1 {
            format!(" ×{attempts}")
        } else {
            String::new()
        };
        if selected {
            // PmenuSel: purple row, dark text.
            let spans = vec![Span::styled(
                format!("  {icon} {}{suffix}", id.label()),
                Style::default().fg(t.on_accent()).bg(t.bg_selection()),
            )];
            lines.push(fill_row(spans, w, t.bg_selection()));
        } else {
            let label_color = match status {
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

/// The live document (re-read every frame, so edits appear as they happen),
/// focused on the current target: the whole doc, or one section's raw slice.
fn draw_chat_preview(f: &mut Frame, app: &App, chat: &ChatState, area: Rect) {
    let t = &app.cfg.theme;
    let target = chat.target();
    let (path, doc_label) = match target {
        Some(tg) => (
            match tg.doc {
                crate::stages::DocKind::Spec => app.dirs.spec_file(&app.slug),
                crate::stages::DocKind::Plan => app.dirs.plan_file(&app.slug),
            },
            tg.doc.label(),
        ),
        None => (app.dirs.spec_file(&app.slug), "spec"),
    };
    let full = std::fs::read_to_string(&path).unwrap_or_default();
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
    draw_markdown_scrolled(f, t, rows[1], &full, 0, focus.as_ref());
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
        lines.push(Line::from(Span::styled(
            "  Tab: target · Enter: send · Esc: close",
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
    for (row, seg) in text.split('\n').enumerate() {
        let lead = if row == 0 {
            Span::styled(
                format!(" {} ", t.icon_prompt()),
                Style::default().fg(t.highlight()).bg(t.bg_row()),
            )
        } else {
            Span::styled("   ", Style::default().bg(t.bg_row()))
        };
        let mut spans = vec![lead];
        if row == cursor_row {
            let col = chat.cursor - consumed;
            let before: String = seg.chars().take(col).collect();
            let after: String = seg.chars().skip(col).collect();
            spans.push(Span::styled(
                before,
                Style::default().fg(t.fg()).bg(t.bg_row()),
            ));
            spans.push(Span::styled(
                "▏",
                Style::default().fg(t.info()).bg(t.bg_row()),
            ));
            spans.push(Span::styled(
                after,
                Style::default().fg(t.fg()).bg(t.bg_row()),
            ));
        } else {
            spans.push(Span::styled(
                seg.to_string(),
                Style::default().fg(t.fg()).bg(t.bg_row()),
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
        let end = app.stream_scroll.unwrap_or(app.stream.len());
        let start = end.saturating_sub(height);
        let lines: Vec<Line> = app.stream[start..end]
            .iter()
            .map(|e| event_line(t, e, stream_area.width))
            .collect();
        f.render_widget(Paragraph::new(lines), stream_area);
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
        if resolved {
            let (mark, color) = if finding.action == "fixed" {
                ("✓fixed ", t.ok())
            } else {
                ("∅dismissed", t.comment())
            };
            spans.push(Span::styled(mark, Style::default().fg(color).bg(row_bg)));
        } else if finding.cross_confirmed() {
            spans.extend(pill(t, " ◆both ".into(), t.ok()));
        } else {
            spans.push(Span::styled(
                "◇single",
                Style::default().fg(t.warn()).bg(row_bg),
            ));
        }
        // Triage answer marker: queued for the claude batch / yours to fix.
        match finding.answer.as_deref() {
            Some("auto") if !resolved => spans.push(Span::styled(
                " ⚑A",
                Style::default().fg(t.accent()).bg(row_bg),
            )),
            Some("manual") if !resolved => spans.push(Span::styled(
                " ⚑M",
                Style::default().fg(t.info()).bg(row_bg),
            )),
            _ => {}
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
        lines.push(fill_row(spans, area.width, row_bg));

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
    let mut lines: Vec<Line> = vec![Line::default()];
    for (i, m) in metas
        .iter()
        .take((area.height as usize).saturating_sub(1))
        .enumerate()
    {
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
    let plan = std::fs::read_to_string(app.dirs.plan_file(&app.slug)).ok();
    let spec = std::fs::read_to_string(app.dirs.spec_file(&app.slug)).ok();
    let text = match (plan, spec) {
        (Some(p), _) => p,
        (None, Some(s)) => format!("*(no plan yet; spec below)*\n\n{s}"),
        (None, None) => "no spec or plan yet; press enter on the spec stage".into(),
    };
    draw_markdown_scrolled(f, &app.cfg.theme, area, &text, app.plan_scroll, None);
}

/// In-app manual: detailed guide + tips, embedded at compile time.
fn draw_guide(f: &mut Frame, app: &App, area: Rect) {
    let text = include_str!("../../docs/guide.md");
    draw_markdown_scrolled(f, &app.cfg.theme, area, text, app.guide_scroll, None);
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
) {
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
            "⚓ anchor lost — step no longer found in plan.md",
            Style::default().fg(t.warn()),
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
    // F answers plan findings; the footer tracks the triage state.
    let plan_finding = finding.file.is_none() && finding.plan_step.is_some();
    if let Some(label) = app.fix_label() {
        footer.push(Span::styled(
            format!("{} {label}  ", SPINNER[app.spinner % SPINNER.len()]),
            Style::default().fg(t.attention()),
        ));
    } else if plan_finding {
        let f_label = if finding.answer.as_deref() == Some("auto") {
            "apply/unqueue"
        } else {
            "queue claude"
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
    cap(&mut footer, "esc", "close".into());
    if !plan_finding && app.fix_label().is_none() {
        footer.push(Span::styled(
            "F answers plan findings · m queues manual",
            Style::default().fg(t.comment()).add_modifier(Modifier::DIM),
        ));
    }
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

fn draw_help(f: &mut Frame, t: &Theme) {
    let groups: [(&str, &[(&str, &str)]); 5] = [
        (
            "navigate",
            &[
                ("j/k", "move"),
                ("[ ]", "cycle features"),
                ("tab 1-5", "panes"),
                ("g/G", "top / follow"),
            ],
        ),
        (
            "run",
            &[
                ("enter", "run stage / open"),
                ("s", "chat: edit spec/plan"),
                ("a", "take over session"),
                ("x", "cancel run"),
                ("c/C", "check fast / full"),
            ],
        ),
        (
            "findings",
            &[
                ("enter", "details"),
                ("f", "mark fixed (toggle)"),
                ("d", "dismiss (+reason)"),
                ("F", "queue/apply claude answers"),
                ("m", "queue manual fix"),
                ("u", "revert applied batch"),
                ("v", "show/hide resolved"),
                ("/", "filter list (2/3)"),
            ],
        ),
        (
            "tools",
            &[
                (":", "command palette"),
                ("o", "open in running nvim"),
                ("Q", "findings → nvim quickfix"),
                ("e", "open in $EDITOR"),
                ("r", "refresh"),
            ],
        ),
        ("misc", &[("?", "this help"), ("q", "quit")]),
    ];
    let height = 3 + groups.iter().map(|(_, ks)| ks.len() + 2).sum::<usize>() as u16;
    let area = centered_rect(f.area(), 46, height.min(f.area().height));
    let inner = float_panel(f, t, area, "keys");

    let mut lines = vec![Line::default()];
    for (group, keys) in groups {
        lines.push(Line::from(Span::styled(
            format!(" {group}"),
            Style::default()
                .fg(t.grey_fg())
                .add_modifier(Modifier::BOLD),
        )));
        for (key, desc) in keys {
            lines.push(Line::from(vec![
                Span::raw("  "),
                keycap(t, key),
                Span::raw(" "),
                Span::styled(desc.to_string(), Style::default().fg(t.muted())),
            ]));
        }
        lines.push(Line::default());
    }
    f.render_widget(Paragraph::new(lines), inner);
}

/// The F-apply confirm: what one `y` will send to claude, and how degraded
/// the mechanical gate would be.
fn draw_apply_confirm(f: &mut Frame, app: &App) {
    let t = &app.cfg.theme;
    let Some(c) = &app.apply_confirm else { return };
    let area = centered_rect(
        f.area(),
        58.min(f.area().width.saturating_sub(2)).max(1),
        5.min(f.area().height.saturating_sub(2)).max(1),
    );
    let inner = float_panel(f, t, area, "apply answers");
    if inner.height == 0 {
        return;
    }
    let mut lines = vec![Line::from(Span::styled(
        format!("apply {} answer(s) via claude — ONE run?", c.count),
        Style::default().fg(t.fg()).add_modifier(Modifier::BOLD),
    ))];
    if c.skipped_other_features > 0 || c.anchor_lost > 0 {
        let mut notes = Vec::new();
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
    let inner = float_panel(f, t, area, "dismiss — reason (enter = none · esc = cancel)");
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
