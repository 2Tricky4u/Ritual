//! All drawing. Pure: reads App, writes the frame. No state mutations.
//!
//! Visual language: NvChad/base46 — borderless panels separated by background
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
use crate::ui::app::{App, CheckState, TABS, Tab};

const SPINNER: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
const SIDEBAR_W: u16 = 28;
const MIN_SIDEBAR_TERM_W: u16 = 70;

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
    if content.width >= MIN_SIDEBAR_TERM_W {
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
        if selected {
            // PmenuSel: purple row, dark text.
            let spans = vec![Span::styled(
                format!("  {icon} {}", id.label()),
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
                    id.label().to_string(),
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
        None => ("nvim —".to_string(), t.comment()),
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
    match app.tab {
        Tab::Live => draw_live(f, app, content),
        Tab::Findings => draw_findings(f, app, content),
        Tab::History => draw_history(f, app, content),
        Tab::Plan => draw_plan(f, app, content),
    }
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
    let guide: [(&str, &str); 8] = [
        ("pipeline", "spec → plan → review → tests → impl → dual"),
        ("runs", "daemons: quit freely, reattach · a takeover"),
        ("findings", "Q → nvim quickfix · o open · e $EDITOR"),
        ("money", "daily budget · per-run caps · --force"),
        ("safety", "redaction · verify-log chain · repro"),
        ("ci", "--ci → JUnit · --json + exit codes"),
        ("parallel", "new --worktree · [ ] switch features"),
        ("more", "report · bench · export · offline · cmds"),
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
    let agg = crate::findings::aggregate(&app.findings);
    if agg.is_empty() {
        f.render_widget(
            Paragraph::new(Span::styled(
                "no findings — run plan-review or dual-review",
                Style::default().fg(t.comment()),
            ))
            .alignment(Alignment::Center),
            Rect::new(area.x, area.y + area.height / 2, area.width, 1),
        );
        return;
    }
    let mut lines: Vec<Line> = vec![Line::default()];
    let per_finding = 3usize; // row + scenario + spacer
    let visible = (area.height as usize / per_finding).max(1);
    let first = app
        .selected_finding
        .saturating_sub(visible.saturating_sub(1));
    for (i, (src, finding)) in agg.iter().enumerate().skip(first).take(visible) {
        let selected = i == app.selected_finding;
        let row_bg = if selected { t.bg_row2() } else { t.bg() };
        let mut spans: Vec<Span> = vec![Span::styled(" ", Style::default().bg(row_bg))];
        spans.extend(severity_pill(t, finding.severity));
        spans.push(Span::styled(" ", Style::default().bg(row_bg)));
        if finding.cross_confirmed() {
            spans.extend(pill(t, " ◆both ".into(), t.ok()));
        } else {
            spans.push(Span::styled(
                "◇single",
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
                .fg(t.fg())
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
        lines.push(Line::default());
    }
    f.render_widget(Paragraph::new(lines), area);
}

fn draw_history(f: &mut Frame, app: &App, area: Rect) {
    let t = &app.cfg.theme;
    if app.metas.is_empty() {
        f.render_widget(
            Paragraph::new(Span::styled(
                "no runs yet",
                Style::default().fg(t.comment()),
            ))
            .alignment(Alignment::Center),
            Rect::new(area.x, area.y + area.height / 2, area.width, 1),
        );
        return;
    }
    let mut lines: Vec<Line> = vec![Line::default()];
    for (i, m) in app
        .metas
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
                    .unwrap_or_else(|| format!("{:>9}", "—")),
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
    let t = &app.cfg.theme;
    let plan = std::fs::read_to_string(app.dirs.plan_file(&app.slug)).ok();
    let spec = std::fs::read_to_string(app.dirs.spec_file(&app.slug)).ok();
    let text = match (plan, spec) {
        (Some(p), _) => p,
        (None, Some(s)) => format!("*(no plan yet — spec below)*\n\n{s}"),
        (None, None) => "no spec or plan yet — press enter on the spec stage".into(),
    };

    // Real markdown rendering (pulldown-cmark), themed; j/k scrolls.
    let lines = crate::ui::markdown::render(&text, t, area.width);
    let total = lines.len();
    let max_scroll = total.saturating_sub(area.height as usize);
    let offset = app.plan_scroll.min(max_scroll);
    let visible: Vec<Line> = lines
        .into_iter()
        .skip(offset)
        .take(area.height as usize)
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
    let groups: [(&str, &[(&str, &str)]); 4] = [
        (
            "navigate",
            &[
                ("j/k", "move"),
                ("[ ]", "cycle features"),
                ("tab 1-4", "panes"),
                ("g/G", "top / follow"),
            ],
        ),
        (
            "run",
            &[
                ("enter", "run stage / open"),
                ("a", "take over session"),
                ("x", "cancel run"),
                ("c/C", "check fast / full"),
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

fn draw_confirm_quit(f: &mut Frame, t: &Theme) {
    let area = centered_rect(f.area(), 44, 3);
    let inner = float_panel(f, t, area, "confirm");
    f.render_widget(
        Paragraph::new(Span::styled(
            "a run is active — quit anyway? (y/n)",
            Style::default().fg(t.attention()),
        ))
        .alignment(Alignment::Center),
        inner,
    );
}
