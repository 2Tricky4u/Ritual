//! Structure helpers for ritual documents (spec.md / plan.md): the list of
//! selectable sections (for scoping a chat edit) and the "has the user written
//! anything real yet?" heuristic shared by the spec stage and the doc-chat
//! completion check. UI-agnostic and pure so it can be unit-tested directly.

use std::ops::Range;

/// Selectable sections of a document: one entry per level-2 (`##`) ATX
/// heading, each range spanning the heading line through the line before the
/// next `##`/`#`. The document title (`#`) and deeper subheadings (`###`+) are
/// not separately selectable: a `###` lives inside its parent `##` range.
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

/// Mechanical scope gate for a section-scoped edit: did the change stay
/// inside `range` (a `sections()` line range on `before`)? Confinement is
/// judged positionally - the lines before `range.start` and the lines from
/// `range.end` on must survive verbatim (same order, same count) at the top
/// and bottom of `after`; everything between is the section's new body.
///
/// Returns `None` when the edit leaked outside the section (or `after` is too
/// short to still contain the untouched prefix + suffix), else
/// `Some((added, removed))`: the line-multiset delta of the section body.
/// Note: a line inserted between the section's last line and the next `##`
/// heading counts as inside (that is textually where the section's body ends).
pub fn edits_confined(before: &str, after: &str, range: &Range<usize>) -> Option<(usize, usize)> {
    let b: Vec<&str> = before.lines().collect();
    let a: Vec<&str> = after.lines().collect();
    let start = range.start.min(b.len());
    let end = range.end.min(b.len()).max(start);
    let suffix_len = b.len() - end;
    if a.len() < start + suffix_len {
        return None; // shrunk past the section: prefix + suffix can't both survive
    }
    if a[..start] != b[..start] || a[a.len() - suffix_len..] != b[end..] {
        return None;
    }
    // Line-multiset delta of the section body (how many lines appeared/vanished).
    let mut counts: std::collections::HashMap<&str, i64> = std::collections::HashMap::new();
    for l in &a[start..a.len() - suffix_len] {
        *counts.entry(l).or_default() += 1;
    }
    for l in &b[start..end] {
        *counts.entry(l).or_default() -= 1;
    }
    let added = counts.values().filter(|v| **v > 0).sum::<i64>() as usize;
    let removed = -counts.values().filter(|v| **v < 0).sum::<i64>() as usize;
    Some((added, removed))
}

/// Multi-section scope gate: did the change stay inside the UNION of
/// `ranges` (each a `sections()` line range on `before`)? The complement of
/// the merged ranges is a sequence of LOCKED blocks that must survive
/// verbatim, in order, disjoint: the first anchored at the top of `after`
/// when the doc starts locked, the last anchored at the bottom when it ends
/// locked, interior blocks matched greedily leftmost (complete for
/// existence - the classic glob-matching argument, since the gaps between
/// blocks are unconstrained).
///
/// `None` = the edit leaked outside every allowed region. `Some((added,
/// removed))` = confined; the line-multiset delta of the allowed regions.
/// Empty `ranges` = the whole document is locked (identity required).
/// A single range behaves exactly like `edits_confined`.
pub fn edits_confined_multi(
    before: &str,
    after: &str,
    ranges: &[Range<usize>],
) -> Option<(usize, usize)> {
    let b: Vec<&str> = before.lines().collect();
    let a: Vec<&str> = after.lines().collect();
    let merged = merge_ranges(ranges, b.len());

    // Locked blocks: the complement of the merged ranges, in order.
    let mut locked: Vec<Range<usize>> = Vec::new();
    let mut cursor = 0;
    for r in &merged {
        if cursor < r.start {
            locked.push(cursor..r.start);
        }
        cursor = r.end;
    }
    if cursor < b.len() {
        locked.push(cursor..b.len());
    }
    let starts_locked = merged.first().is_none_or(|r| r.start > 0);
    let ends_locked = merged.last().is_none_or(|r| r.end < b.len());

    // Match `a` against locked0 · gap · locked1 · … ; record the gaps (the
    // allowed regions of `a`) for the counts.
    let mut pos = 0usize;
    let mut gaps: Vec<Range<usize>> = Vec::new();
    let n = locked.len();
    for (i, blk) in locked.iter().enumerate() {
        let lines = &b[blk.clone()];
        let at = if i == 0 && starts_locked {
            (a.len() >= lines.len() && &a[..lines.len()] == lines).then_some(0)?
        } else if i == n - 1 && ends_locked {
            let start = a.len().checked_sub(lines.len())?;
            (start >= pos && &a[start..] == lines).then_some(start)?
        } else {
            find_block(&a, lines, pos)?
        };
        if at > pos {
            gaps.push(pos..at);
        }
        pos = at + lines.len();
    }
    if ends_locked {
        if pos != a.len() {
            return None; // text past the end-anchored block (single-block case)
        }
    } else if pos < a.len() {
        gaps.push(pos..a.len());
    }

    // Line-multiset delta over allowed regions: after-gaps vs before-ranges.
    let mut counts: std::collections::HashMap<&str, i64> = std::collections::HashMap::new();
    for g in &gaps {
        for l in &a[g.clone()] {
            *counts.entry(l).or_default() += 1;
        }
    }
    for r in &merged {
        for l in &b[r.clone()] {
            *counts.entry(l).or_default() -= 1;
        }
    }
    let added = counts.values().filter(|v| **v > 0).sum::<i64>() as usize;
    let removed = -counts.values().filter(|v| **v < 0).sum::<i64>() as usize;
    Some((added, removed))
}

/// Clamp to `len`, drop empties, sort, merge overlapping AND adjacent ranges.
fn merge_ranges(ranges: &[Range<usize>], len: usize) -> Vec<Range<usize>> {
    let mut rs: Vec<Range<usize>> = ranges
        .iter()
        .map(|r| {
            let s = r.start.min(len);
            s..r.end.min(len).max(s)
        })
        .filter(|r| r.start < r.end)
        .collect();
    rs.sort_by_key(|r| r.start);
    let mut merged: Vec<Range<usize>> = Vec::new();
    for r in rs {
        match merged.last_mut() {
            Some(last) if r.start <= last.end => last.end = last.end.max(r.end),
            _ => merged.push(r),
        }
    }
    merged
}

/// Leftmost start index ≥ `from` where `blk` appears verbatim in `a`.
fn find_block(a: &[&str], blk: &[&str], from: usize) -> Option<usize> {
    let last_start = a.len().checked_sub(blk.len())?;
    (from..=last_start).find(|&i| &a[i..i + blk.len()] == blk)
}

/// The ATX heading level (1-6) and trimmed title text, or None. Requires the
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

/// True once the document has real prose: any visible line that is non-empty
/// and not a markdown heading (`#`) or an HTML/template tag (`<`). This is
/// the contract for "the spec stage is done": the template's headings and
/// `<!-- ... -->` prompts alone do not count. Comment state is tracked ACROSS
/// lines, so the inner lines of a multi-line `<!-- ... -->` block never count
/// as content (they used to, a false positive that marked untouched specs
/// done and injected empty invariants).
pub fn has_meaningful_content(text: &str) -> bool {
    let mut in_comment = false;
    for line in text.lines() {
        // Strip every comment span from the line, keeping cross-line state.
        let mut visible = String::new();
        let mut rest = line;
        loop {
            if in_comment {
                match rest.find("-->") {
                    Some(i) => {
                        in_comment = false;
                        rest = &rest[i + 3..];
                    }
                    None => break, // whole remainder is inside the comment
                }
            } else {
                match rest.find("<!--") {
                    Some(i) => {
                        visible.push_str(&rest[..i]);
                        in_comment = true;
                        rest = &rest[i + 4..];
                    }
                    None => {
                        visible.push_str(rest);
                        break;
                    }
                }
            }
        }
        let t = visible.trim();
        if !t.is_empty() && !t.starts_with(['#', '<']) {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPEC_TEMPLATE: &str = include_str!("../templates/spec-template.md");

    /// lines: 0 "# Plan", 1 "", 2 "## A", 3 "a1", 4 "a2", 5 "", 6 "## B", 7 "b1"
    /// sections: A = 2..6, B = 6..8 (matches `sections()`, asserted below).
    const GATE_DOC: &str = "# Plan\n\n## A\na1\na2\n\n## B\nb1\n";

    #[test]
    fn edits_confined_accepts_changes_inside_the_section() {
        let secs = sections(GATE_DOC);
        assert_eq!(secs[0], ("A".into(), 2..6));
        assert_eq!(secs[1], ("B".into(), 6..8));
        let a = &secs[0].1;

        // Replace one body line -> confined, +1/-1.
        let after = "# Plan\n\n## A\na1 fixed\na2\n\n## B\nb1\n";
        assert_eq!(edits_confined(GATE_DOC, after, a), Some((1, 1)));
        // Section grows.
        let after = "# Plan\n\n## A\na1\na1b\na2\n\n## B\nb1\n";
        assert_eq!(edits_confined(GATE_DOC, after, a), Some((1, 0)));
        // Section shrinks.
        let after = "# Plan\n\n## A\na1\n\n## B\nb1\n";
        assert_eq!(edits_confined(GATE_DOC, after, a), Some((0, 1)));
        // The section's own heading line (range.start) is inside.
        let after = "# Plan\n\n## A (renamed)\na1\na2\n\n## B\nb1\n";
        assert_eq!(edits_confined(GATE_DOC, after, a), Some((1, 1)));
        // A line landing just before the next heading is part of this section.
        let after = "# Plan\n\n## A\na1\na2\n\nextra\n## B\nb1\n";
        assert_eq!(edits_confined(GATE_DOC, after, a), Some((1, 0)));
        // No line-level change (e.g. trailing-newline strip) -> confined 0/0.
        assert_eq!(
            edits_confined(GATE_DOC, GATE_DOC.trim_end_matches('\n'), a),
            Some((0, 0))
        );
    }

    #[test]
    fn edits_confined_rejects_anything_outside_the_section() {
        let range = 2..6; // section A
        // Edit before the section (title line).
        let after = "# Plan v2\n\n## A\na1\na2\n\n## B\nb1\n";
        assert_eq!(edits_confined(GATE_DOC, after, &range), None);
        // Edit on the line just above the heading (range.start - 1).
        let after = "# Plan\nsneak\n## A\na1\na2\n\n## B\nb1\n";
        assert_eq!(edits_confined(GATE_DOC, after, &range), None);
        // Edit at range.end (the next section's heading).
        let after = "# Plan\n\n## A\na1\na2\n\n## B (renamed)\nb1\n";
        assert_eq!(edits_confined(GATE_DOC, after, &range), None);
        // Edit after the section (next section's body).
        let after = "# Plan\n\n## A\na1\na2\n\n## B\nb1 changed\n";
        assert_eq!(edits_confined(GATE_DOC, after, &range), None);
        // Line inserted past the next heading.
        let after = "# Plan\n\n## A\na1\na2\n\n## B\nsneak\nb1\n";
        assert_eq!(edits_confined(GATE_DOC, after, &range), None);
        // Delete inside + re-insert the identical line outside still leaks.
        let after = "# Plan\n\n## A\na2\n\n## B\nb1\na1\n";
        assert_eq!(edits_confined(GATE_DOC, after, &range), None);
        // Whole-file rewrite.
        assert_eq!(edits_confined(GATE_DOC, "totally new\n", &range), None);
        // Truncated below prefix+suffix length.
        assert_eq!(edits_confined(GATE_DOC, "# Plan\n", &range), None);
    }

    /// lines: 0 "# Plan", 1 "", 2 "## A", 3 "a1", 4 "", 5 "## B", 6 "b1",
    /// 7 "", 8 "## C", 9 "c1"  → sections: A=2..5, B=5..8, C=8..10.
    const GATE_DOC3: &str = "# Plan\n\n## A\na1\n\n## B\nb1\n\n## C\nc1\n";

    #[test]
    fn multi_single_range_equals_edits_confined() {
        // Every case the single-range tests exercise must agree.
        let cases: &[(&str, Range<usize>)] = &[
            ("# Plan\n\n## A\na1 fixed\na2\n\n## B\nb1\n", 2..6),
            ("# Plan\n\n## A\na1\na1b\na2\n\n## B\nb1\n", 2..6),
            ("# Plan\n\n## A\na1\n\n## B\nb1\n", 2..6),
            ("# Plan\n\n## A (renamed)\na1\na2\n\n## B\nb1\n", 2..6),
            ("# Plan\n\n## A\na1\na2\n\nextra\n## B\nb1\n", 2..6),
            ("# Plan v2\n\n## A\na1\na2\n\n## B\nb1\n", 2..6),
            ("# Plan\nsneak\n## A\na1\na2\n\n## B\nb1\n", 2..6),
            ("# Plan\n\n## A\na1\na2\n\n## B (renamed)\nb1\n", 2..6),
            ("# Plan\n\n## A\na1\na2\n\n## B\nb1 changed\n", 2..6),
            ("# Plan\n\n## A\na2\n\n## B\nb1\na1\n", 2..6),
            ("totally new\n", 2..6),
            ("# Plan\n", 2..6),
            ("# Plan\n\n## A\na1\na2\n\n## B\nb1 fixed\nb2\nb3\n", 6..8),
            ("# Plan\n\n## A\nleak\na2\n\n## B\nb1\n", 6..8),
            ("anything\nat all\n", 0..8),
        ];
        for (after, range) in cases {
            assert_eq!(
                edits_confined_multi(GATE_DOC, after, std::slice::from_ref(range)),
                edits_confined(GATE_DOC, after, range),
                "parity broke for range {range:?} on {after:?}"
            );
        }
    }

    #[test]
    fn multi_accepts_edits_in_both_sections_and_rejects_between() {
        let ranges = vec![2..5, 8..10]; // A and C; B stays locked between them
        // Edit both allowed sections at once.
        let after = "# Plan\n\n## A\na1 fixed\n\n## B\nb1\n\n## C\nc1 fixed\n";
        assert_eq!(
            edits_confined_multi(GATE_DOC3, after, &ranges),
            Some((2, 2))
        );
        // Edit the locked section between the ranges.
        let after = "# Plan\n\n## A\na1\n\n## B\nb1 sneaky\n\n## C\nc1\n";
        assert_eq!(edits_confined_multi(GATE_DOC3, after, &ranges), None);
        // Edit the locked prefix.
        let after = "# Plan v2\n\n## A\na1\n\n## B\nb1\n\n## C\nc1\n";
        assert_eq!(edits_confined_multi(GATE_DOC3, after, &ranges), None);
        // Last section is allowed: free tail growth.
        let after = "# Plan\n\n## A\na1\n\n## B\nb1\n\n## C\nc1\nc2\nc3\n";
        assert_eq!(
            edits_confined_multi(GATE_DOC3, after, &ranges),
            Some((2, 0))
        );
    }

    #[test]
    fn multi_merges_overlapping_adjacent_and_unordered() {
        // A (2..6) and B (6..8) are adjacent: together they free 2..8, so an
        // edit in B passes even when the ranges arrive reversed.
        let after = "# Plan\n\n## A\na1\na2\n\n## B\nb1 fixed\n";
        assert_eq!(
            edits_confined_multi(GATE_DOC, after, &[6..8, 2..6]),
            Some((1, 1))
        );
        // Overlapping ranges merge to the same span.
        assert_eq!(
            edits_confined_multi(GATE_DOC, after, &[2..7, 5..8]),
            Some((1, 1))
        );
        // ...but the prefix stays locked.
        let after = "# Plan v2\n\n## A\na1\na2\n\n## B\nb1\n";
        assert_eq!(edits_confined_multi(GATE_DOC, after, &[6..8, 2..6]), None);
    }

    #[test]
    fn multi_interior_locked_block_equal_to_inserted_text() {
        // Insert a duplicate of the locked "## B" heading INSIDE allowed A:
        // greedy matching must not bind the locked block to the duplicate.
        let ranges = vec![2..5, 8..10];
        let after = "# Plan\n\n## A\na1\n## B\n\n## B\nb1\n\n## C\nc1\n";
        assert_eq!(
            edits_confined_multi(GATE_DOC3, after, &ranges),
            Some((1, 0))
        );
    }

    #[test]
    fn multi_degenerate_ranges() {
        // Empty ranges: the whole document is locked -> identity required.
        assert_eq!(edits_confined_multi(GATE_DOC, GATE_DOC, &[]), Some((0, 0)));
        assert_eq!(edits_confined_multi(GATE_DOC, "# Plan\nx\n", &[]), None);
        // Out-of-bounds ranges clamp to nothing -> identity required.
        assert_eq!(
            edits_confined_multi(GATE_DOC, GATE_DOC, std::slice::from_ref(&(50..99))),
            Some((0, 0))
        );
        assert_eq!(
            edits_confined_multi(GATE_DOC, "# Plan\nx\n", std::slice::from_ref(&(50..99))),
            None
        );
        // An allowed section may be deleted entirely (empty gap).
        let after = "# Plan\n\n## B\nb1\n\n## C\nc1\n";
        assert_eq!(
            edits_confined_multi(GATE_DOC3, after, std::slice::from_ref(&(2..5))),
            Some((0, 3))
        );
        // Whole-doc range: everything allowed.
        assert_eq!(
            edits_confined_multi(GATE_DOC3, "anything\n", std::slice::from_ref(&(0..10))),
            Some((1, 10))
        );
    }

    #[test]
    fn edits_confined_last_section_and_degenerate_ranges() {
        // Last section (range.end == line count, empty suffix): free tail.
        let b = 6..8;
        let after = "# Plan\n\n## A\na1\na2\n\n## B\nb1 fixed\nb2\nb3\n";
        assert_eq!(edits_confined(GATE_DOC, after, &b), Some((3, 1)));
        // ...but its prefix is still guarded.
        let after = "# Plan\n\n## A\nleak\na2\n\n## B\nb1\n";
        assert_eq!(edits_confined(GATE_DOC, after, &b), None);
        // Heading-only section gains a body.
        let doc = "## Empty\n## Next\nn1\n";
        let secs = sections(doc);
        assert_eq!(secs[0].1, 0..1);
        let after = "## Empty\nfilled\n## Next\nn1\n";
        assert_eq!(edits_confined(doc, after, &secs[0].1), Some((1, 0)));
        // Whole-doc range: everything is inside.
        assert_eq!(
            edits_confined(GATE_DOC, "anything\nat all\n", &(0..8)),
            Some((2, 8))
        );
        // Out-of-bounds range clamps instead of panicking.
        assert_eq!(edits_confined("a\n", "a\n", &(5..9)), Some((0, 0)));
    }

    #[test]
    fn template_yields_the_four_h2_sections() {
        let secs = sections(SPEC_TEMPLATE);
        let names: Vec<&str> = secs.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "Goal",
                "Behavior (the contract: WHAT, not HOW)",
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
    }

    #[test]
    fn multiline_comments_are_never_meaningful() {
        // The regression: inner comment lines used to count as prose,
        // marking untouched specs done and injecting empty invariants.
        assert!(!has_meaningful_content("<!--\nfill this in\nplease\n-->\n"));
        assert!(!has_meaningful_content(
            "# H\n<!-- examples:\n- looks like a real bullet\n-->\n"
        ));
        // Real prose outside the block still counts.
        assert!(has_meaningful_content("<!--\nhint\n-->\nreal prose\n"));
        // Content after the closer on the same line counts.
        assert!(has_meaningful_content("<!-- hint\n--> tail matters\n"));
        // An unclosed comment swallows the rest of the document.
        assert!(!has_meaningful_content(
            "<!--\nreal-looking\nnever closed\n"
        ));
        // Inline comments are stripped; surrounding prose survives.
        assert!(has_meaningful_content("before <!-- x --> after\n"));
        assert!(!has_meaningful_content("<!-- one-liner -->\n"));
        // Two comment spans on one line, nothing between them.
        assert!(!has_meaningful_content("<!-- a --><!-- b -->\n"));
        assert!(!has_meaningful_content(
            "# Feature: x\n\n## Goal\n<!-- prompt -->\n"
        ));
        assert!(has_meaningful_content("# Goal\nWe build the widget.\n"));
        assert!(!has_meaningful_content(""));
        assert!(!has_meaningful_content("   \n\t\n"));
    }
}
