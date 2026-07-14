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

/// The verdict of the heading-structured confinement gate: the line delta over
/// the allowed regions, plus which queued sections actually changed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConfineReport {
    pub added: usize,
    pub removed: usize,
    /// Headings (as `sections()` reports them) of QUEUED sections whose body
    /// changed. `on_fix_exited` uses this to downgrade a `#n: FIXED` claim whose
    /// own section never moved.
    pub changed: Vec<String>,
}

/// Heading-structured scope gate for a batch plan-fix: did the edit stay inside
/// the sections named in `queued`? Unlike `edits_confined_multi` (which matches
/// locked blocks by CONTENT SEARCH and so can be fooled by a planted verbatim
/// copy of a locked section), this keys identity on the heading. Every LOCKED
/// section (a `##` heading present in `before` whose title is not in `queued`)
/// must survive with an identical body-multiset in `after`, and the preamble
/// before the first `##` must be byte-identical. Queued sections may be freely
/// rewritten, renamed, or removed, and brand-new sections may appear.
///
/// `None` = a locked section (or the preamble) was altered/removed/duplicated:
/// the edit leaked. `Some(report)` = confined; `report.changed` names the queued
/// sections whose body actually moved. Duplicate locked headings are compared as
/// a multiset; any count mismatch is conservatively treated as a leak.
pub fn confine_by_heading(before: &str, after: &str, queued: &[String]) -> Option<ConfineReport> {
    let (pre_b, secs_b) = split_doc(before);
    let (pre_a, secs_a) = split_doc(after);
    if pre_b != pre_a {
        return None; // the preamble (title + intro) is locked
    }
    let queued: std::collections::HashSet<&str> = queued.iter().map(String::as_str).collect();

    // Group section bodies by heading for both revisions.
    let map_b = group_by_title(&secs_b);
    let map_a = group_by_title(&secs_a);

    let mut changed = Vec::new();
    for (title, bodies_b) in &map_b {
        if queued.contains(title.as_str()) {
            // A queued section: allowed to change; record whether it did.
            let moved = map_a
                .get(title)
                .is_none_or(|bodies_a| !eq_multiset(bodies_a, bodies_b));
            if moved {
                changed.push(title.clone());
            }
        } else {
            // A locked section: its body-multiset must be identical in `after`.
            match map_a.get(title) {
                Some(bodies_a) if eq_multiset(bodies_a, bodies_b) => {}
                _ => return None,
            }
        }
    }

    let (added, removed) = line_delta(before, after);
    Some(ConfineReport {
        added,
        removed,
        changed,
    })
}

/// Split a document into its preamble (lines before the first `##`) and its
/// `##` sections as `(title, body)` pairs, the body being the heading line
/// through the line before the next `##`/`#`.
fn split_doc(text: &str) -> (String, Vec<(String, String)>) {
    let lines: Vec<&str> = text.lines().collect();
    let secs = sections(text);
    let first = secs.first().map(|(_, r)| r.start).unwrap_or(lines.len());
    // `trim_end` normalizes trailing blank lines: a section's captured body
    // gains a blank separator when a new section is appended after it, which is
    // not a real edit to a locked section.
    let preamble = lines[..first].join("\n").trim_end().to_string();
    let items = secs
        .iter()
        .map(|(t, r)| {
            (
                t.clone(),
                lines[r.clone()].join("\n").trim_end().to_string(),
            )
        })
        .collect();
    (preamble, items)
}

fn group_by_title(secs: &[(String, String)]) -> std::collections::BTreeMap<String, Vec<String>> {
    let mut map: std::collections::BTreeMap<String, Vec<String>> =
        std::collections::BTreeMap::new();
    for (title, body) in secs {
        map.entry(title.clone()).or_default().push(body.clone());
    }
    map
}

fn eq_multiset(a: &[String], b: &[String]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut a: Vec<&String> = a.iter().collect();
    let mut b: Vec<&String> = b.iter().collect();
    a.sort();
    b.sort();
    a == b
}

/// Line-multiset delta between two documents: (# lines that appeared, # that
/// vanished). On a confined edit the locked regions cancel, so this equals the
/// delta over the allowed regions.
fn line_delta(before: &str, after: &str) -> (usize, usize) {
    let mut counts: std::collections::HashMap<&str, i64> = std::collections::HashMap::new();
    for l in after.lines() {
        *counts.entry(l).or_default() += 1;
    }
    for l in before.lines() {
        *counts.entry(l).or_default() -= 1;
    }
    let added = counts.values().filter(|v| **v > 0).sum::<i64>() as usize;
    let removed = -counts.values().filter(|v| **v < 0).sum::<i64>() as usize;
    (added, removed)
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

    // before: preamble "# Plan", locked B, queued A and C.
    const DECOY_BEFORE: &str = "# Plan\n\n## A\na1\n\n## B\nb1\n\n## C\nc1\n";

    #[test]
    fn confine_by_heading_accepts_a_confined_edit_and_names_the_changed_section() {
        let queued = vec!["A".to_string()];
        let after = "# Plan\n\n## A\na1\na2\na3\n\n## B\nb1\n";
        let rep = confine_by_heading(GATE_DOC, after, &queued).expect("confined");
        assert_eq!(rep.changed, vec!["A".to_string()]);
        assert_eq!((rep.added, rep.removed), (1, 0));
    }

    #[test]
    fn confine_by_heading_reports_only_the_sections_that_moved() {
        // Both A and B queued, but only A is edited: B must NOT be in `changed`,
        // so an over-claimed `#B: FIXED` can be downgraded.
        let queued = vec!["A".to_string(), "B".to_string()];
        let after = "# Plan\n\n## A\na1\na2\nextra\n\n## B\nb1\n";
        let rep = confine_by_heading(GATE_DOC, after, &queued).expect("confined");
        assert_eq!(rep.changed, vec!["A".to_string()]);
    }

    #[test]
    fn confine_by_heading_rejects_editing_a_locked_section() {
        let queued = vec!["A".to_string()];
        let after = "# Plan\n\n## A\na1\na2\n\n## B\nb1 leaked\n";
        assert_eq!(confine_by_heading(GATE_DOC, after, &queued), None);
    }

    #[test]
    fn confine_by_heading_closes_the_decoy_bypass() {
        // The attack: plant a verbatim copy of locked B inside allowed A, then
        // rewrite the REAL B. The positional gate is fooled (the locked block
        // is found at the decoy) - documents the bug we are fixing.
        let queued_ranges = [2..5usize, 8..10usize]; // A, C on DECOY_BEFORE
        let decoy = "# Plan\n\n## A\na1\n## B\nb1\n\n## B\nHACKED\n\n## C\nc1\n";
        assert!(
            edits_confined_multi(DECOY_BEFORE, decoy, &queued_ranges).is_some(),
            "positional gate is fooled by the decoy",
        );
        // The heading-structured gate keys on the heading, so the duplicated
        // locked B breaks the multiset and the batch is rejected.
        let queued = vec!["A".to_string(), "C".to_string()];
        assert_eq!(confine_by_heading(DECOY_BEFORE, decoy, &queued), None);
    }

    #[test]
    fn confine_by_heading_locks_the_preamble() {
        let queued = vec!["A".to_string(), "B".to_string()];
        // Every `##` section is queued, but the title line is rewritten.
        let after = "# Plan (rewritten)\n\n## A\na1\na2\n\n## B\nb1\n";
        assert_eq!(confine_by_heading(GATE_DOC, after, &queued), None);
    }

    #[test]
    fn confine_by_heading_allows_new_sections_and_renamed_queued_headings() {
        // A queued section may be renamed (its old title vanishes, a new one
        // appears) and a brand-new section may be added; B stays locked+intact.
        let queued = vec!["A".to_string()];
        let after = "# Plan\n\n## A renamed\na1\na2\n\n## B\nb1\n\n## Notes\nnew\n";
        assert!(confine_by_heading(GATE_DOC, after, &queued).is_some());
    }

    #[test]
    fn confine_by_heading_treats_duplicate_locked_headings_as_a_multiset() {
        let before = "# Plan\n\n## Dup\nx\n\n## Dup\ny\n\n## A\na1\n";
        let queued = vec!["A".to_string()];
        // Editing one of the two locked `## Dup` bodies leaks.
        let after = "# Plan\n\n## Dup\nx changed\n\n## Dup\ny\n\n## A\na1\n";
        assert_eq!(confine_by_heading(before, after, &queued), None);
        // Leaving both `## Dup` intact and editing only A is confined.
        let after_ok = "# Plan\n\n## Dup\nx\n\n## Dup\ny\n\n## A\na1\nmore\n";
        assert!(confine_by_heading(before, after_ok, &queued).is_some());
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
