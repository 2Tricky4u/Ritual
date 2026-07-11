//! All drawing. Pure: reads App, writes the frame. No state mutations.

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, List, ListItem, Paragraph, Tabs, Wrap};

use crate::findings::Severity;
use crate::runner::events::AgentEvent;
use crate::state::{PIPELINE, StageStatus};
use crate::theme::Theme;
use crate::ui::app::{App, CheckState, TABS, Tab};

const SPINNER: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

pub fn draw(f: &mut Frame, app: &App) {
    let t = &app.cfg.theme;
    f.render_widget(
        Block::default().style(Style::default().bg(t.bg())),
        f.area(),
    );

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(5), Constraint::Length(1)])
        .split(f.area());

    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(30), Constraint::Min(20)])
        .split(rows[0]);

    draw_sidebar(f, app, cols[0]);
    draw_main(f, app, cols[1]);
    draw_status_bar(f, app, rows[1]);

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

/// Command palette overlay: filter line + fuzzy-matched actions.
fn draw_palette(f: &mut Frame, app: &App) {
    let t = &app.cfg.theme;
    let Some(p) = &app.palette else { return };
    let matches = app.palette_filtered();
    let height = (matches.len() as u16 + 3).clamp(4, 14);
    let area = centered_rect(f.area(), 52, height);
    f.render_widget(Clear, area);

    let mut lines = vec![Line::from(vec![
        Span::styled("› ", Style::default().fg(t.highlight())),
        Span::styled(p.input.clone(), Style::default().fg(t.fg())),
        Span::styled("▏", Style::default().fg(t.info())),
    ])];
    let visible = (height as usize).saturating_sub(3);
    let first = p.selected.saturating_sub(visible.saturating_sub(1));
    for (i, (label, _)) in matches.iter().enumerate().skip(first).take(visible) {
        let (marker, style) = if i == p.selected {
            (
                "▸ ",
                Style::default()
                    .fg(t.highlight())
                    .add_modifier(Modifier::BOLD),
            )
        } else {
            ("  ", Style::default().fg(t.fg()))
        };
        lines.push(Line::from(vec![
            Span::styled(marker, Style::default().fg(t.highlight())),
            Span::styled(label.clone(), style),
        ]));
    }
    if matches.is_empty() {
        lines.push(Line::from(Span::styled(
            "  no matching command",
            Style::default().fg(t.muted()),
        )));
    }
    f.render_widget(
        Paragraph::new(lines)
            .block(titled_block(t, "command", true).style(Style::default().bg(t.bg_dark()))),
        area,
    );
}

fn titled_block<'a>(t: &Theme, title: &'a str, focused: bool) -> Block<'a> {
    let border = if focused { t.accent() } else { t.muted() };
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(border))
        .title(Span::styled(
            format!(" {title} "),
            Style::default().fg(t.accent()).add_modifier(Modifier::BOLD),
        ))
}

fn stage_style(t: &Theme, status: StageStatus) -> (String, Style) {
    match status {
        StageStatus::Pending => (t.icon_pending().into(), Style::default().fg(t.muted())),
        StageStatus::Running => (t.icon_running().into(), Style::default().fg(t.info())),
        StageStatus::Done => (t.icon_done().into(), Style::default().fg(t.ok())),
        StageStatus::Failed => (t.icon_failed().into(), Style::default().fg(t.error())),
        StageStatus::NeedsAttention => (
            t.icon_attention().into(),
            Style::default().fg(t.attention()),
        ),
        StageStatus::Skipped => (t.icon_skipped().into(), Style::default().fg(t.muted())),
    }
}

fn draw_sidebar(f: &mut Frame, app: &App, area: Rect) {
    let t = &app.cfg.theme;
    let title = app
        .state
        .features
        .get(&app.slug)
        .map(|feat| feat.title.clone())
        .unwrap_or_else(|| "ritual".into());
    let block = titled_block(t, &title, true);
    let inner = block.inner(area);
    f.render_widget(block, area);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(PIPELINE.len() as u16 + 1),
            Constraint::Min(0),
        ])
        .split(inner);

    // Pipeline list.
    let items: Vec<ListItem> = PIPELINE
        .iter()
        .enumerate()
        .map(|(i, id)| {
            let status = app.stage_status(*id);
            let (mut icon, style) = stage_style(t, status);
            if status == StageStatus::Running {
                icon = SPINNER[app.spinner % SPINNER.len()].to_string();
            }
            let selected = i == app.selected;
            let marker = if selected { "▸" } else { " " };
            let label_style = if selected {
                Style::default().fg(t.fg()).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(t.muted())
            };
            ListItem::new(Line::from(vec![
                Span::styled(format!(" {marker} "), Style::default().fg(t.highlight())),
                Span::styled(format!("{icon} "), style),
                Span::styled(id.label().to_string(), label_style),
            ]))
        })
        .collect();
    f.render_widget(List::new(items), chunks[0]);

    // Widgets: branch, agent auth, mcp bridge, check state.
    let claude = match &app.agents.claude {
        Some(a) if a.logged_in => Span::styled(
            format!(
                "claude ok{}",
                a.subscription_type
                    .as_deref()
                    .map(|s| format!(" ({s})"))
                    .unwrap_or_default()
            ),
            Style::default().fg(t.ok()),
        ),
        Some(_) => Span::styled("claude: not logged in", Style::default().fg(t.error())),
        None => Span::styled("claude …", Style::default().fg(t.muted())),
    };
    let codex = match app.agents.codex_cli_ok {
        Some(true) => Span::styled("codex ok", Style::default().fg(t.ok())),
        Some(false) => Span::styled("codex: run `codex login`", Style::default().fg(t.error())),
        None => Span::styled("codex …", Style::default().fg(t.muted())),
    };
    let mcp = match app.agents.mcp_codex_connected {
        Some(true) => Span::styled("bridge ok", Style::default().fg(t.ok())),
        Some(false) => Span::styled("bridge down", Style::default().fg(t.error())),
        None => Span::styled("bridge …", Style::default().fg(t.muted())),
    };
    let check = match &app.check {
        CheckState::Unknown => Span::styled("check ?", Style::default().fg(t.muted())),
        CheckState::Running => Span::styled(
            format!("check {}", SPINNER[app.spinner % SPINNER.len()]),
            Style::default().fg(t.info()),
        ),
        CheckState::Green => Span::styled("check green", Style::default().fg(t.ok())),
        CheckState::Red { .. } => Span::styled("check RED", Style::default().fg(t.error())),
    };
    let widget_line = |icon: &'static str, content: Span<'static>| {
        Line::from(vec![
            Span::raw(" "),
            Span::styled(icon, Style::default().fg(t.accent())),
            Span::raw(" "),
            content,
        ])
    };
    let lines = vec![
        Line::default(),
        Line::from(vec![
            Span::styled(
                format!(" {} ", t.icon_branch()),
                Style::default().fg(t.highlight()),
            ),
            Span::styled(app.branch.clone(), Style::default().fg(t.fg())),
        ]),
        widget_line(t.icon_agent(), claude),
        widget_line(t.icon_agent(), codex),
        widget_line(t.icon_agent(), mcp),
        widget_line(t.icon_check(), check),
    ];
    f.render_widget(Paragraph::new(lines), chunks[1]);
}

fn draw_main(f: &mut Frame, app: &App, area: Rect) {
    let t = &app.cfg.theme;
    let block = titled_block(t, "ritual", false);
    let inner = block.inner(area);
    f.render_widget(block, area);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(0)])
        .split(inner);

    let titles: Vec<Line> = TABS
        .iter()
        .map(|(_, name)| Line::from(name.to_string()))
        .collect();
    let selected = TABS
        .iter()
        .position(|(tab, _)| *tab == app.tab)
        .unwrap_or(0);
    let tabs = Tabs::new(titles)
        .select(selected)
        .style(Style::default().fg(t.muted()))
        .highlight_style(
            Style::default()
                .fg(t.highlight())
                .add_modifier(Modifier::BOLD),
        )
        .divider(Span::styled("·", Style::default().fg(t.muted())));
    f.render_widget(tabs, chunks[0]);

    match app.tab {
        Tab::Live => draw_live(f, app, chunks[1]),
        Tab::Findings => draw_findings(f, app, chunks[1]),
        Tab::History => draw_history(f, app, chunks[1]),
        Tab::Plan => draw_plan(f, app, chunks[1]),
    }
}

fn event_line<'a>(t: &Theme, ev: &'a AgentEvent) -> Line<'a> {
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
                .fg(t.muted())
                .add_modifier(Modifier::ITALIC),
        )),
        AgentEvent::ToolUse { name, summary } => Line::from(vec![
            Span::styled("▸ ", Style::default().fg(t.info())),
            Span::styled(
                name.clone(),
                Style::default().fg(t.info()).add_modifier(Modifier::BOLD),
            ),
            Span::styled(format!(" {summary}"), Style::default().fg(t.muted())),
        ]),
        AgentEvent::ToolResult { is_error, summary } => {
            let color = if *is_error { t.error() } else { t.muted() };
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
            Line::from(Span::styled(
                format!(
                    "{icon} {} {}",
                    total_cost_usd
                        .map(|c| format!("${c:.3}"))
                        .unwrap_or_default(),
                    duration_ms
                        .map(|d| format!("{:.1}s", d as f64 / 1000.0))
                        .unwrap_or_default()
                ),
                Style::default().fg(color).add_modifier(Modifier::BOLD),
            ))
        }
        AgentEvent::Stderr { line } => Line::from(Span::styled(
            line.clone(),
            Style::default().fg(t.muted()).add_modifier(Modifier::DIM),
        )),
        AgentEvent::Raw { value } => Line::from(Span::styled(
            format!(
                "· {}",
                value
                    .get("type")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("?")
            ),
            Style::default().fg(t.muted()).add_modifier(Modifier::DIM),
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
        let hint = if app.running.is_some() {
            "waiting for agent output…"
        } else {
            "select a stage and press Enter to run it — headless output streams here"
        };
        f.render_widget(
            Paragraph::new(hint)
                .style(Style::default().fg(t.muted()))
                .alignment(Alignment::Center)
                .wrap(Wrap { trim: true }),
            centered_vertically(stream_area, 1),
        );
    } else {
        let height = stream_area.height as usize;
        let end = app.stream_scroll.unwrap_or(app.stream.len());
        let start = end.saturating_sub(height);
        let lines: Vec<Line> = app.stream[start..end]
            .iter()
            .map(|e| event_line(t, e))
            .collect();
        f.render_widget(Paragraph::new(lines), stream_area);
    }

    if let Some((rect, tail)) = check_area {
        f.render_widget(
            Paragraph::new(tail)
                .style(Style::default().fg(t.error()))
                .wrap(Wrap { trim: false })
                .block(
                    Block::default()
                        .borders(Borders::TOP)
                        .border_style(Style::default().fg(t.error()))
                        .title(Span::styled(
                            " check.sh failed ",
                            Style::default().fg(t.error()).add_modifier(Modifier::BOLD),
                        )),
                ),
            rect,
        );
    }
}

fn draw_findings(f: &mut Frame, app: &App, area: Rect) {
    let t = &app.cfg.theme;
    let agg = crate::findings::aggregate(&app.findings);
    if agg.is_empty() {
        f.render_widget(
            Paragraph::new("no findings — run plan-review or dual-review")
                .style(Style::default().fg(t.muted()))
                .alignment(Alignment::Center),
            centered_vertically(area, 1),
        );
        return;
    }
    let items: Vec<ListItem> = agg
        .iter()
        .enumerate()
        .map(|(i, (src, finding))| {
            let sev_color = match finding.severity {
                Severity::Critical => t.error(),
                Severity::Major => t.warn(),
                Severity::Minor => t.attention(),
            };
            let badge = if finding.cross_confirmed() {
                Span::styled("◆both", Style::default().fg(t.ok()))
            } else {
                Span::styled("◇single", Style::default().fg(t.warn()))
            };
            let marker = if i == app.selected_finding {
                "▸"
            } else {
                " "
            };
            let stage = &app.findings[*src].file.stage;
            ListItem::new(Line::from(vec![
                Span::styled(format!("{marker} "), Style::default().fg(t.highlight())),
                Span::styled(
                    format!("{:<8} ", finding.severity.label()),
                    Style::default().fg(sev_color).add_modifier(Modifier::BOLD),
                ),
                badge,
                Span::styled(
                    format!(" {} ", finding.location()),
                    Style::default().fg(t.info()),
                ),
                Span::styled(finding.title.clone(), Style::default().fg(t.fg())),
                Span::styled(
                    format!("  [{stage}:{}]", finding.verdict),
                    Style::default().fg(t.muted()),
                ),
            ]))
        })
        .collect();
    f.render_widget(List::new(items), area);
}

fn draw_history(f: &mut Frame, app: &App, area: Rect) {
    let t = &app.cfg.theme;
    if app.metas.is_empty() {
        f.render_widget(
            Paragraph::new("no runs yet")
                .style(Style::default().fg(t.muted()))
                .alignment(Alignment::Center),
            centered_vertically(area, 1),
        );
        return;
    }
    let lines: Vec<Line> = app
        .metas
        .iter()
        .take(area.height as usize)
        .map(|m| {
            let (icon, color) = if m.ok {
                (t.icon_done(), t.ok())
            } else {
                (t.icon_failed(), t.error())
            };
            let when = m
                .started_at
                .map(|d| d.format("%m-%d %H:%M").to_string())
                .unwrap_or_else(|| "?".into());
            Line::from(vec![
                Span::styled(format!("{icon} "), Style::default().fg(color)),
                Span::styled(format!("{when}  "), Style::default().fg(t.muted())),
                Span::styled(format!("{:<12}", m.stage), Style::default().fg(t.fg())),
                Span::styled(format!("{:<8}", m.agent), Style::default().fg(t.info())),
                Span::styled(
                    m.total_cost_usd
                        .map(|c| format!("{:>8}", format!("${c:.3}")))
                        .unwrap_or_else(|| format!("{:>8}", "-")),
                    Style::default().fg(t.warn()),
                ),
                Span::styled(
                    m.usage
                        .as_ref()
                        .map(|u| format!("  {}↑ {}↓", u.input_tokens, u.output_tokens))
                        .unwrap_or_default(),
                    Style::default().fg(t.muted()),
                ),
            ])
        })
        .collect();
    f.render_widget(Paragraph::new(lines), area);
}

fn draw_plan(f: &mut Frame, app: &App, area: Rect) {
    let t = &app.cfg.theme;
    let plan = std::fs::read_to_string(app.dirs.plan_file(&app.slug)).ok();
    let spec = std::fs::read_to_string(app.dirs.spec_file(&app.slug)).ok();
    let text = match (plan, spec) {
        (Some(p), _) => p,
        (None, Some(s)) => format!("(no plan yet — spec below)\n\n{s}"),
        (None, None) => "no spec or plan yet — press Enter on the spec stage to write one".into(),
    };
    f.render_widget(
        Paragraph::new(text)
            .style(Style::default().fg(t.fg()))
            .wrap(Wrap { trim: false }),
        area,
    );
}

fn draw_status_bar(f: &mut Frame, app: &App, area: Rect) {
    let t = &app.cfg.theme;
    let left = app.status_msg.clone().unwrap_or_else(|| {
        "enter run · : commands · j/k move · tab panes · c check · ? help · q quit".into()
    });
    let budget = app.cfg.budget_daily_usd.map(|b| {
        let spent = app.today_spend();
        let color = if spent >= b {
            t.error()
        } else if spent >= 0.75 * b {
            t.warn()
        } else {
            t.muted()
        };
        (format!("${spent:.2}/${b:.2} "), color)
    });
    let running = app
        .running
        .map(|s| format!("{} {} ", SPINNER[app.spinner % SPINNER.len()], s.label()))
        .unwrap_or_default();
    let bar = Line::from(vec![
        Span::styled(format!(" {left}"), Style::default().fg(t.muted())),
        Span::raw(" "),
    ]);
    f.render_widget(
        Paragraph::new(bar).style(Style::default().bg(t.bg_dark())),
        area,
    );
    let mut right_spans: Vec<Span> = Vec::new();
    if let Some((text, color)) = budget {
        right_spans.push(Span::styled(text, Style::default().fg(color)));
    }
    if !running.is_empty() {
        right_spans.push(Span::styled(running, Style::default().fg(t.info())));
    }
    if !right_spans.is_empty() {
        let w: u16 = right_spans
            .iter()
            .map(|s| s.content.chars().count() as u16)
            .sum();
        if w < area.width {
            let right = Rect::new(area.right() - w, area.y, w, 1);
            f.render_widget(
                Paragraph::new(Line::from(right_spans)).style(Style::default().bg(t.bg_dark())),
                right,
            );
        }
    }
}

fn draw_help(f: &mut Frame, t: &Theme) {
    let area = centered_rect(f.area(), 46, 14);
    f.render_widget(Clear, area);
    let lines = vec![
        help_line(t, "enter", "run selected stage / open finding"),
        help_line(t, "j / k", "navigate"),
        help_line(t, "tab, 1-4", "switch pane"),
        help_line(t, "c / C", "check.sh fast / full"),
        help_line(t, "x", "cancel running stage"),
        help_line(t, "e", "open finding in $EDITOR"),
        help_line(t, "r", "refresh findings/auth"),
        help_line(t, "g / G", "scroll top / follow"),
        help_line(t, "q", "quit"),
    ];
    f.render_widget(
        Paragraph::new(lines)
            .block(titled_block(t, "keys", true).style(Style::default().bg(t.bg_dark()))),
        area,
    );
}

fn draw_confirm_quit(f: &mut Frame, t: &Theme) {
    let area = centered_rect(f.area(), 44, 3);
    f.render_widget(Clear, area);
    f.render_widget(
        Paragraph::new(Span::styled(
            "a run is active — quit anyway? (y/n)",
            Style::default().fg(t.attention()),
        ))
        .alignment(Alignment::Center)
        .block(titled_block(t, "confirm", true).style(Style::default().bg(t.bg_dark()))),
        area,
    );
}

fn help_line<'a>(t: &Theme, key: &'a str, desc: &'a str) -> Line<'a> {
    Line::from(vec![
        Span::styled(
            format!(" {key:<10}"),
            Style::default()
                .fg(t.highlight())
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(desc, Style::default().fg(t.fg())),
    ])
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

fn centered_vertically(area: Rect, height: u16) -> Rect {
    let y = area.y + area.height.saturating_sub(height) / 2;
    Rect::new(area.x, y, area.width, height.min(area.height))
}
