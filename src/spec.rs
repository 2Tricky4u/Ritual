//! Structure helpers for ritual documents (spec.md / plan.md): the list of
//! selectable sections (for scoping a chat edit) and the "has the user written
//! anything real yet?" heuristic shared by the spec stage and the doc-chat
//! completion check. UI-agnostic and pure so it can be unit-tested directly.

use std::ops::Range;

/// Selectable sections of a document: one entry per level-2 (`##`) ATX
/// heading, each range spanning the heading line through the line before the
/// next `##`/`#`. The document title (`#`) and deeper subheadings (`###`+) are
/// not separately selectable — a `###` lives inside its parent `##` range.
/// Returns `[]` for a document with no `##` headings (chat then offers only
/// the whole document). Both the spec template and ritual's plans use `##`
/// for their sections.
pub fn sections(text: &str) -> Vec<(String, Range<usize>)> {
    let lines: Vec<&str> = text.lines().collect();
    let mut out = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        let Some((2, title)) = heading(line) else {
            continue;
        };
        // A section ends at the next heading of level <= 2 (sibling `##` or a
        // new `#`), else at end of document.
        let end = lines
            .iter()
            .enumerate()
            .skip(i + 1)
            .find(|(_, l)| heading(l).is_some_and(|(lvl, _)| lvl <= 2))
            .map(|(j, _)| j)
            .unwrap_or(lines.len());
        out.push((title, i..end));
    }
    out
}

/// The ATX heading level (1–6) and trimmed title text, or None. Requires the
/// mandatory space after the `#`s (so `###` alone or `#tag` is not a heading).
fn heading(line: &str) -> Option<(usize, String)> {
    let t = line.trim_start();
    let level = t.bytes().take_while(|&b| b == b'#').count();
    if level == 0 || level > 6 {
        return None;
    }
    let title = t[level..].strip_prefix(' ')?.trim();
    Some((level, title.to_string()))
}

/// True once the document has real prose — any line that is non-empty and not
/// a markdown heading (`#`) or an HTML/template comment (`<`). This is the
/// contract for "the spec stage is done": the template's headings and
/// `<!-- ... -->` prompts alone do not count.
pub fn has_meaningful_content(text: &str) -> bool {
    text.lines()
        .any(|l| !l.trim().is_empty() && !l.trim_start().starts_with(['#', '<']))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPEC_TEMPLATE: &str = include_str!("../templates/spec-template.md");

    #[test]
    fn template_yields_the_four_h2_sections() {
        let secs = sections(SPEC_TEMPLATE);
        let names: Vec<&str> = secs.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "Goal",
                "Behavior (the contract — WHAT, not HOW)",
                "Edge cases & failure modes",
                "Out of scope",
            ]
        );
        // Ranges are ordered, non-overlapping, and cover distinct line spans.
        for w in secs.windows(2) {
            assert!(w[0].1.end <= w[1].1.start, "ranges overlap: {secs:?}");
            assert!(w[0].1.start < w[0].1.end, "empty range: {:?}", w[0]);
        }
        // Every range indexes real lines.
        let n_lines = SPEC_TEMPLATE.lines().count();
        assert!(secs.iter().all(|(_, r)| r.end <= n_lines));
    }

    #[test]
    fn a_section_range_contains_its_own_body_not_the_next_heading() {
        let doc = "# Title\n\n## Alpha\nbody a\n\n## Beta\nbody b\n";
        let secs = sections(doc);
        assert_eq!(secs.len(), 2);
        let lines: Vec<&str> = doc.lines().collect();
        let (name, range) = &secs[0];
        assert_eq!(name, "Alpha");
        let slice: Vec<&str> = lines[range.clone()].to_vec();
        assert!(slice.contains(&"body a"));
        assert!(!slice.contains(&"## Beta"));
    }

    #[test]
    fn deeper_subheadings_stay_inside_the_parent_section() {
        let doc = "## Parent\nintro\n### Child\ndetail\n## Next\n";
        let secs = sections(doc);
        assert_eq!(secs.len(), 2);
        let lines: Vec<&str> = doc.lines().collect();
        let slice: Vec<&str> = lines[secs[0].1.clone()].to_vec();
        assert!(slice.contains(&"### Child")); // subheading grouped under Parent
        assert!(slice.contains(&"detail"));
        assert!(!slice.contains(&"## Next"));
    }

    #[test]
    fn no_h2_headings_yields_no_sections() {
        assert!(sections("# Only a title\n\nsome prose\n").is_empty());
        assert!(sections("").is_empty());
        assert!(sections("no headings at all\njust text\n").is_empty());
    }

    #[test]
    fn hashes_without_a_space_are_not_headings() {
        assert!(sections("##nospace\n#tag\n").is_empty());
    }

    #[test]
    fn meaningful_content_ignores_headings_and_comments() {
        assert!(!has_meaningful_content(SPEC_TEMPLATE)); // template = headings + comments only
        assert!(!has_meaningful_content(
            "# Feature: x\n\n## Goal\n<!-- prompt -->\n"
        ));
        assert!(has_meaningful_content("# Goal\nWe build the widget.\n"));
        assert!(!has_meaningful_content(""));
        assert!(!has_meaningful_content("   \n\t\n"));
    }
}
