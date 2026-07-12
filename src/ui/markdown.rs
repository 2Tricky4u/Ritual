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

/// Rendered document plus a source map: `src[i]` is the 0-based SOURCE line
/// that produced output line `i` (None for spacers/dividers). Callers use it
/// to highlight/scroll to a source-line range (chat's section focus).
pub struct Rendered {
    pub lines: Vec<Line<'static>>,
    pub src: Vec<Option<usize>>,
}

struct Renderer<'t> {
    t: &'t Theme,
    width: u16,
    lines: Vec<Line<'static>>,
    src: Vec<Option<usize>>,
    spans: Vec<Span<'static>>,
    inline: Inline,
    in_code_block: bool,
    // (ordered next-index or None) per nesting level
    lists: Vec<Option<u64>>,
    // table collection
    table: Option<Vec<Vec<String>>>,
    table_src: Option<usize>,
    in_table_head: bool,
    /// Source line of the event currently being processed.
    cur_src: Option<usize>,
    /// Source line latched for the in-progress `spans` buffer.
    line_src: Option<usize>,
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

    /// Every finished Line goes through here so `lines` and `src` stay in
    /// lockstep — THE invariant of the source map.
    fn push_line(&mut self, line: Line<'static>, src: Option<usize>) {
        self.lines.push(line);
        self.src.push(src);
    }

    /// Span pushes latch the current source line for the in-progress buffer.
    fn push_span(&mut self, span: Span<'static>) {
        if self.line_src.is_none() {
            self.line_src = self.cur_src;
        }
        self.spans.push(span);
    }

    fn flush(&mut self) {
        if !self.spans.is_empty() {
            let spans = std::mem::take(&mut self.spans);
            let src = self.line_src.take();
            self.push_line(Line::from(spans), src);
        }
        self.line_src = None;
    }

    fn blank(&mut self) {
        self.flush();
        if !matches!(self.lines.last(), Some(l) if l.spans.is_empty()) && !self.lines.is_empty() {
            self.push_line(Line::default(), None);
        }
    }

    fn quote_prefix(&mut self) {
        for _ in 0..self.inline.quote_depth {
            self.push_span(Span::styled(
                "▍ ".to_string(),
                Style::default().fg(self.t.accent()),
            ));
        }
    }

    fn push_text(&mut self, text: &str) {
        if self.in_code_block {
            // Code blocks: one bg-banded row per line, each mapped to its own
            // source line (the chunk's start + offset).
            let start = self.cur_src;
            for (i, line) in text.split('\n').enumerate() {
                if line.is_empty() && text.ends_with('\n') {
                    continue;
                }
                let content = format!("  {line}");
                let pad = (self.width as usize).saturating_sub(content.chars().count());
                self.push_line(
                    Line::from(Span::styled(
                        format!("{content}{}", " ".repeat(pad)),
                        Style::default().fg(self.t.muted()).bg(self.t.bg_row()),
                    )),
                    start.map(|s| s + i),
                );
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
        self.push_span(Span::styled(text.to_string(), self.style()));
    }

    fn list_indent(&self) -> String {
        " ".repeat(self.lists.len().saturating_sub(1) * 2 + 1)
    }

    fn emit_table(&mut self, rows: Vec<Vec<String>>) {
        if rows.is_empty() {
            return;
        }
        // Rows map to the table's start line + offset (header divider counts
        // as the delimiter row) — close enough for focus banding.
        let table_src = self.table_src.take();
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
            let row_src = table_src.map(|s| s + ri + usize::from(ri > 0));
            self.push_line(Line::from(spans), row_src);
            if ri == 0 {
                let total: usize = widths.iter().map(|w| w + 3).sum::<usize>() + 1;
                self.push_line(
                    Line::from(Span::styled(
                        "─".repeat(total.min(self.width as usize)),
                        Style::default().fg(t.divider()),
                    )),
                    table_src.map(|s| s + 1),
                );
            }
        }
    }
}

/// Render a markdown document to themed lines plus a source-line map.
/// Pure; caller windows/scrolls.
pub fn render(text: &str, t: &Theme, width: u16) -> Rendered {
    let mut options = Options::empty();
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_STRIKETHROUGH);
    options.insert(Options::ENABLE_TASKLISTS);

    // Byte offset -> 0-based source line.
    let line_starts: Vec<usize> = std::iter::once(0)
        .chain(text.bytes().enumerate().filter_map(
            |(i, b)| {
                if b == b'\n' { Some(i + 1) } else { None }
            },
        ))
        .collect();
    let byte_to_line = |off: usize| line_starts.partition_point(|&s| s <= off).saturating_sub(1);

    let mut r = Renderer {
        t,
        width,
        lines: Vec::new(),
        src: Vec::new(),
        spans: Vec::new(),
        inline: Inline::default(),
        in_code_block: false,
        lists: Vec::new(),
        table: None,
        table_src: None,
        in_table_head: false,
        cur_src: None,
        line_src: None,
    };

    for (event, range) in Parser::new_ext(text, options).into_offset_iter() {
        r.cur_src = Some(byte_to_line(range.start));
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
                r.push_span(Span::styled(format!("{icon} "), Style::default().fg(color)));
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
                    let src = r.cur_src;
                    r.push_line(
                        Line::from(Span::styled(
                            format!(" {lang}"),
                            Style::default()
                                .fg(t.grey_fg())
                                .add_modifier(Modifier::ITALIC),
                        )),
                        src,
                    );
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
                r.push_span(Span::raw(indent));
                r.push_span(marker);
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
                r.push_span(Span::styled(glyph.to_string(), Style::default().fg(color)));
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
                r.table_src = r.cur_src;
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
                let src = r.cur_src;
                r.push_line(
                    Line::from(Span::styled(
                        "─".repeat(width as usize),
                        Style::default().fg(t.divider()),
                    )),
                    src,
                );
            }
            _ => {}
        }
    }
    r.flush();
    debug_assert_eq!(r.lines.len(), r.src.len());
    Rendered {
        lines: r.lines,
        src: r.src,
    }
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
        let lines = render(md, &t, 60).lines;
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
        let lines = render(md, &t, 60).lines;
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
        let lines = render(md, &t, 20).lines;
        let joined = text_of(&lines).join("\n");
        assert!(joined.contains("▍ wisdom here"));
        assert!(joined.contains("────"));
        assert!(joined.contains("after"));
    }

    /// Output index of the first line whose text contains `needle`.
    fn find(lines: &[Line], needle: &str) -> usize {
        text_of(lines)
            .iter()
            .position(|l| l.contains(needle))
            .unwrap_or_else(|| panic!("no output line contains {needle:?}"))
    }

    #[test]
    fn torture_doc_keeps_lockstep_and_survives_every_construct() {
        let t = Theme::default();
        // Every construct at once, deliberately ragged and nested.
        let md = "# T\n\n\
            5. five\n6. six\n\n\
            > outer\n> > inner nested quote\n\n\
            ```\nno language fence\n```\n\n\
            | a | b | c |\n|---|---|\n| 1 |\n| x | y | z | extra |\n\n\
            line one  \nhard break\nsoft break\n\n\
            | wide | cells |\n|---|---|\n| 你好世界 | ok |\n\n\
            [link](https://x) and ~~gone~~\n";
        let r = render(md, &t, 60);
        assert_eq!(r.lines.len(), r.src.len(), "lockstep on the torture doc");
        let joined = text_of(&r.lines).join("\n");
        // Ordered list respects a start offset other than 1.
        assert!(joined.contains("5. five"), "{joined}");
        assert!(joined.contains("6. six"));
        // Language-less fences still render their content.
        assert!(joined.contains("no language fence"));
        // Ragged table: every row renders, padded to the widest row.
        assert!(joined.contains("1"));
        assert!(joined.contains("extra"));
        // Nested quotes and breaks survive.
        assert!(joined.contains("inner nested quote"));
        assert!(joined.contains("hard break"));
        // Wide unicode cells don't panic and keep their text.
        assert!(joined.contains("你好世界"));
        assert!(joined.contains("link"));
    }

    #[test]
    fn empty_table_and_crlf_input_are_harmless() {
        let t = Theme::default();
        // A header-only table and CRLF line endings.
        for md in ["| a |\n|---|\n", "# H\r\n\r\nprose line\r\n"] {
            let r = render(md, &t, 40);
            assert_eq!(r.lines.len(), r.src.len(), "lockstep for {md:?}");
        }
        let r = render("# H\r\n\r\nprose line\r\n", &t, 40);
        assert!(text_of(&r.lines).join("\n").contains("prose line"));
    }

    #[test]
    fn source_map_stays_in_lockstep_and_points_home() {
        let t = Theme::default();
        // Source lines:      0        1 2         3 4        5 6      7 8
        let md = "# Title\n\n## Alpha\n\nbody a\n\n```rust\nfn x() {}\n```\n\n## Beta\nbody b\n";
        let r = render(md, &t, 60);
        assert_eq!(r.lines.len(), r.src.len(), "map out of lockstep");

        assert_eq!(r.src[find(&r.lines, "Title")], Some(0));
        assert_eq!(r.src[find(&r.lines, "Alpha")], Some(2));
        assert_eq!(r.src[find(&r.lines, "body a")], Some(4));
        // Code interior maps to its own source line (7).
        assert_eq!(r.src[find(&r.lines, "fn x()")], Some(7));
        assert_eq!(r.src[find(&r.lines, "Beta")], Some(10));
        assert_eq!(r.src[find(&r.lines, "body b")], Some(11));
        // Spacer lines carry no source.
        assert!(
            r.lines
                .iter()
                .zip(&r.src)
                .any(|(l, s)| l.spans.is_empty() && s.is_none())
        );
    }

    #[test]
    fn source_map_covers_the_other_test_docs() {
        // Lockstep invariant on every doc the other tests exercise.
        let t = Theme::default();
        for md in [
            "# Title\n\nSome **bold** and `code`.\n\n- alpha\n- beta\n  - nested\n\n1. one\n2. two\n\n```rust\nfn main() {}\n```\n",
            "| a | b |\n|---|---|\n| 1 | 2 |\n\n- [x] done thing\n- [ ] todo thing\n",
            "> wisdom here\n\n---\n\nafter\n",
            "",
        ] {
            let r = render(md, &t, 40);
            assert_eq!(r.lines.len(), r.src.len(), "lockstep broken for {md:?}");
        }
    }
}
