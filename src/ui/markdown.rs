//! Markdown → styled ratatui lines, in the render-markdown.nvim spirit the
//! user lives in: heading icons, bg-banded code blocks, pink bullets, task
//! checkboxes, quote gutters, aligned tables. Parsed with pulldown-cmark —
//! no ad-hoc regexes.

use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag, TagEnd};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::theme::Theme;

const H_ICONS_NERD: [&str; 6] = ["󰲡", "󰲣", "󰲥", "󰲧", "󰲩", "󰲫"];

#[derive(Default)]
struct Inline {
    bold: bool,
    italic: bool,
    strike: bool,
    code: bool,
    link: bool,
    heading: Option<u8>,
    quote_depth: usize,
}

struct Renderer<'t> {
    t: &'t Theme,
    width: u16,
    lines: Vec<Line<'static>>,
    spans: Vec<Span<'static>>,
    inline: Inline,
    in_code_block: bool,
    // (ordered next-index or None) per nesting level
    lists: Vec<Option<u64>>,
    // table collection
    table: Option<Vec<Vec<String>>>,
    in_table_head: bool,
}

impl<'t> Renderer<'t> {
    fn style(&self) -> Style {
        let t = self.t;
        let mut style = Style::default();
        style = if self.inline.code {
            style.fg(t.ok()).bg(t.bg_row())
        } else if let Some(level) = self.inline.heading {
            let color = if level <= 2 {
                t.accent()
            } else {
                t.highlight()
            };
            style.fg(color).add_modifier(Modifier::BOLD)
        } else if self.inline.quote_depth > 0 {
            style.fg(t.muted()).add_modifier(Modifier::ITALIC)
        } else if self.inline.link {
            style.fg(t.info()).add_modifier(Modifier::UNDERLINED)
        } else {
            style.fg(t.fg())
        };
        if self.inline.bold {
            style = style.add_modifier(Modifier::BOLD);
        }
        if self.inline.italic {
            style = style.add_modifier(Modifier::ITALIC);
        }
        if self.inline.strike {
            style = style.add_modifier(Modifier::CROSSED_OUT);
        }
        style
    }

    fn flush(&mut self) {
        if !self.spans.is_empty() {
            let spans = std::mem::take(&mut self.spans);
            self.lines.push(Line::from(spans));
        }
    }

    fn blank(&mut self) {
        self.flush();
        if !matches!(self.lines.last(), Some(l) if l.spans.is_empty()) && !self.lines.is_empty() {
            self.lines.push(Line::default());
        }
    }

    fn quote_prefix(&mut self) {
        for _ in 0..self.inline.quote_depth {
            self.spans.push(Span::styled(
                "▍ ".to_string(),
                Style::default().fg(self.t.accent()),
            ));
        }
    }

    fn push_text(&mut self, text: &str) {
        if self.in_code_block {
            // Code blocks: one bg-banded row per line.
            for line in text.split('\n') {
                if line.is_empty() && text.ends_with('\n') {
                    continue;
                }
                let content = format!("  {line}");
                let pad = (self.width as usize).saturating_sub(content.chars().count());
                self.lines.push(Line::from(Span::styled(
                    format!("{content}{}", " ".repeat(pad)),
                    Style::default().fg(self.t.muted()).bg(self.t.bg_row()),
                )));
            }
            return;
        }
        if let Some(table) = &mut self.table {
            if let Some(row) = table.last_mut()
                && let Some(cell) = row.last_mut()
            {
                cell.push_str(text);
            }
            return;
        }
        if self.spans.is_empty() && self.inline.quote_depth > 0 {
            self.quote_prefix();
        }
        self.spans
            .push(Span::styled(text.to_string(), self.style()));
    }

    fn list_indent(&self) -> String {
        " ".repeat(self.lists.len().saturating_sub(1) * 2 + 1)
    }

    fn emit_table(&mut self, rows: Vec<Vec<String>>) {
        if rows.is_empty() {
            return;
        }
        let t = self.t;
        let cols = rows.iter().map(|r| r.len()).max().unwrap_or(0);
        let mut widths = vec![0usize; cols];
        for row in &rows {
            for (i, cell) in row.iter().enumerate() {
                widths[i] = widths[i].max(cell.trim().chars().count());
            }
        }
        for (ri, row) in rows.iter().enumerate() {
            let mut spans: Vec<Span> = Vec::new();
            spans.push(Span::styled(
                "│ ".to_string(),
                Style::default().fg(t.divider()),
            ));
            for (ci, width) in widths.iter().enumerate() {
                let cell = row.get(ci).map(|s| s.trim()).unwrap_or("");
                let style = if ri == 0 {
                    Style::default().fg(t.accent()).add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(t.fg())
                };
                spans.push(Span::styled(format!("{cell:<width$}"), style));
                spans.push(Span::styled(
                    " │ ".to_string(),
                    Style::default().fg(t.divider()),
                ));
            }
            self.lines.push(Line::from(spans));
            if ri == 0 {
                let total: usize = widths.iter().map(|w| w + 3).sum::<usize>() + 1;
                self.lines.push(Line::from(Span::styled(
                    "─".repeat(total.min(self.width as usize)),
                    Style::default().fg(t.divider()),
                )));
            }
        }
    }
}

/// Render a markdown document to themed lines. Pure; caller windows/scrolls.
pub fn render(text: &str, t: &Theme, width: u16) -> Vec<Line<'static>> {
    let mut options = Options::empty();
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_STRIKETHROUGH);
    options.insert(Options::ENABLE_TASKLISTS);

    let mut r = Renderer {
        t,
        width,
        lines: Vec::new(),
        spans: Vec::new(),
        inline: Inline::default(),
        in_code_block: false,
        lists: Vec::new(),
        table: None,
        in_table_head: false,
    };

    for event in Parser::new_ext(text, options) {
        match event {
            Event::Start(Tag::Heading { level, .. }) => {
                r.blank();
                let level = level as u8;
                r.inline.heading = Some(level);
                let icon = match t.icons {
                    crate::theme::IconSet::Nerd => {
                        H_ICONS_NERD[(level as usize - 1).min(5)].to_string()
                    }
                    crate::theme::IconSet::Ascii => "#".repeat(level as usize),
                };
                let color = if level <= 2 {
                    t.accent()
                } else {
                    t.highlight()
                };
                r.spans
                    .push(Span::styled(format!("{icon} "), Style::default().fg(color)));
            }
            Event::End(TagEnd::Heading(..)) => {
                r.inline.heading = None;
                r.flush();
            }
            Event::Start(Tag::Paragraph) => {
                if r.lists.is_empty() && r.table.is_none() {
                    r.blank();
                }
            }
            Event::End(TagEnd::Paragraph) => r.flush(),
            Event::Start(Tag::BlockQuote(_)) => {
                r.blank();
                r.inline.quote_depth += 1;
            }
            Event::End(TagEnd::BlockQuote(_)) => {
                r.inline.quote_depth = r.inline.quote_depth.saturating_sub(1);
                r.flush();
            }
            Event::Start(Tag::CodeBlock(kind)) => {
                r.blank();
                if let CodeBlockKind::Fenced(lang) = &kind
                    && !lang.is_empty()
                {
                    r.lines.push(Line::from(Span::styled(
                        format!(" {lang}"),
                        Style::default()
                            .fg(t.grey_fg())
                            .add_modifier(Modifier::ITALIC),
                    )));
                }
                r.in_code_block = true;
            }
            Event::End(TagEnd::CodeBlock) => {
                r.in_code_block = false;
            }
            Event::Start(Tag::List(start)) => {
                if r.lists.is_empty() {
                    r.blank();
                } else {
                    r.flush();
                }
                r.lists.push(start);
            }
            Event::End(TagEnd::List(_)) => {
                r.lists.pop();
                r.flush();
            }
            Event::Start(Tag::Item) => {
                r.flush();
                let indent = r.list_indent();
                let marker = match r.lists.last_mut() {
                    Some(Some(n)) => {
                        let m = format!("{n}. ");
                        *n += 1;
                        Span::styled(m, Style::default().fg(t.info()))
                    }
                    _ => Span::styled("• ".to_string(), Style::default().fg(t.highlight())),
                };
                r.spans.push(Span::raw(indent));
                r.spans.push(marker);
            }
            Event::End(TagEnd::Item) => r.flush(),
            Event::TaskListMarker(checked) => {
                let (glyph, color) = if checked {
                    (
                        match t.icons {
                            crate::theme::IconSet::Nerd => " ",
                            crate::theme::IconSet::Ascii => "[x] ",
                        },
                        t.ok(),
                    )
                } else {
                    (
                        match t.icons {
                            crate::theme::IconSet::Nerd => " ",
                            crate::theme::IconSet::Ascii => "[ ] ",
                        },
                        t.muted(),
                    )
                };
                r.spans
                    .push(Span::styled(glyph.to_string(), Style::default().fg(color)));
            }
            Event::Start(Tag::Emphasis) => r.inline.italic = true,
            Event::End(TagEnd::Emphasis) => r.inline.italic = false,
            Event::Start(Tag::Strong) => r.inline.bold = true,
            Event::End(TagEnd::Strong) => r.inline.bold = false,
            Event::Start(Tag::Strikethrough) => r.inline.strike = true,
            Event::End(TagEnd::Strikethrough) => r.inline.strike = false,
            Event::Start(Tag::Link { .. }) => r.inline.link = true,
            Event::End(TagEnd::Link) => r.inline.link = false,
            Event::Start(Tag::Table(_)) => {
                r.blank();
                r.table = Some(Vec::new());
            }
            Event::End(TagEnd::Table) => {
                if let Some(rows) = r.table.take() {
                    r.emit_table(rows);
                }
            }
            Event::Start(Tag::TableHead) => {
                r.in_table_head = true;
                if let Some(table) = &mut r.table {
                    table.push(Vec::new());
                }
            }
            Event::End(TagEnd::TableHead) => r.in_table_head = false,
            Event::Start(Tag::TableRow) => {
                if let Some(table) = &mut r.table {
                    table.push(Vec::new());
                }
            }
            Event::Start(Tag::TableCell) => {
                if let Some(table) = &mut r.table
                    && let Some(row) = table.last_mut()
                {
                    row.push(String::new());
                }
            }
            Event::Text(text) => r.push_text(&text),
            Event::Code(code) => {
                let saved = r.inline.code;
                r.inline.code = true;
                r.push_text(&format!(" {code} "));
                r.inline.code = saved;
            }
            Event::SoftBreak => r.push_text(" "),
            Event::HardBreak => r.flush(),
            Event::Rule => {
                r.blank();
                r.lines.push(Line::from(Span::styled(
                    "─".repeat(width as usize),
                    Style::default().fg(t.divider()),
                )));
            }
            _ => {}
        }
    }
    r.flush();
    r.lines
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text_of(lines: &[Line]) -> Vec<String> {
        lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect()
    }

    #[test]
    fn renders_headings_lists_code_and_inline() {
        let t = Theme::default();
        let md = "# Title\n\nSome **bold** and `code`.\n\n- alpha\n- beta\n  - nested\n\n1. one\n2. two\n\n```rust\nfn main() {}\n```\n";
        let lines = render(md, &t, 60);
        let texts = text_of(&lines);
        let joined = texts.join("\n");
        assert!(joined.contains("Title"));
        assert!(joined.contains("• alpha"));
        assert!(joined.contains("• nested"));
        assert!(joined.contains("1. one"));
        assert!(joined.contains("2. two"));
        assert!(joined.contains("fn main() {}"));
        assert!(joined.contains(" code "));
        // Heading line is styled with the accent color.
        let title = lines
            .iter()
            .find(|l| text_of(std::slice::from_ref(l))[0].contains("Title"))
            .unwrap();
        assert_eq!(title.spans.last().unwrap().style.fg, Some(t.accent()));
        // Code block rows carry the band background.
        let code_row = lines
            .iter()
            .find(|l| text_of(std::slice::from_ref(l))[0].contains("fn main"))
            .unwrap();
        assert_eq!(code_row.spans[0].style.bg, Some(t.bg_row()));
    }

    #[test]
    fn renders_tables_and_tasks() {
        let t = Theme::default();
        let md = "| a | b |\n|---|---|\n| 1 | 2 |\n\n- [x] done thing\n- [ ] todo thing\n";
        let lines = render(md, &t, 60);
        let joined = text_of(&lines).join("\n");
        assert!(joined.contains("│ a"), "{joined}");
        assert!(joined.contains("│ 1"));
        assert!(joined.contains("done thing"));
        assert!(joined.contains("todo thing"));
    }

    #[test]
    fn quotes_and_rules() {
        let t = Theme::default();
        let md = "> wisdom here\n\n---\n\nafter\n";
        let lines = render(md, &t, 20);
        let joined = text_of(&lines).join("\n");
        assert!(joined.contains("▍ wisdom here"));
        assert!(joined.contains("────"));
        assert!(joined.contains("after"));
    }
}
