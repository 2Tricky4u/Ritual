//! Lane definitions for `ritual audit` - the optional whole-project review.
//! `.ritual/audit-lanes.md` is the user-editable source of truth: one `##`
//! heading per lane (a flow/tech/path of the project), body lines = that
//! lane's scope. Parsing is drift-tolerant in the repo's usual way: junk is
//! skipped, never an error. Selection guarantees two things the literature
//! asks for: the cross-flow `global-overview` lane ALWAYS runs (appended
//! last when the file doesn't define one), and any truncation to the
//! configured cap is reported, never silent.

/// The always-on cross-flow lane: contracts BETWEEN flows, docs-vs-code,
/// config-vs-behavior - the defects no single-flow lane can see.
pub const GLOBAL_LANE: &str = "global-overview";

const GLOBAL_LANE_SCOPE: &str = "Cross-flow consistency: the contracts BETWEEN the other lanes, \
     documentation vs actual behavior, configuration vs code, and \
     architecture-level invariants no single flow owns.";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lane {
    pub name: String,
    pub description: String,
}

/// The lanes that will actually run, plus how many file-defined lanes were
/// dropped to fit the cap (the caller MUST surface a non-zero count).
#[derive(Debug)]
pub struct LaneSelection {
    pub lanes: Vec<Lane>,
    pub truncated: usize,
}

/// Parse `audit-lanes.md`: exactly-`##` headings start lanes (deeper `###`
/// stays description text), content before the first heading is ignored,
/// empty-named headings are skipped, duplicate names (case-insensitive)
/// keep the first occurrence. An empty-description lane is kept - the name
/// alone scopes it.
pub fn parse_lanes(text: &str) -> Vec<Lane> {
    let mut lanes: Vec<Lane> = Vec::new();
    let mut current: Option<Lane> = None;
    for line in text.lines() {
        let trimmed = line.trim();
        let hashes = trimmed.chars().take_while(|c| *c == '#').count();
        if hashes == 2 {
            if let Some(lane) = current.take() {
                lanes.push(lane);
            }
            let name = trimmed.trim_matches('#').trim();
            if !name.is_empty() {
                current = Some(Lane {
                    name: name.to_string(),
                    description: String::new(),
                });
            }
            continue;
        }
        if let Some(lane) = &mut current {
            if !lane.description.is_empty() {
                lane.description.push('\n');
            }
            lane.description.push_str(line);
        }
    }
    if let Some(lane) = current.take() {
        lanes.push(lane);
    }
    for lane in &mut lanes {
        lane.description = lane.description.trim().to_string();
    }
    // Duplicate names would collide on report paths; keep the first.
    let mut seen: Vec<String> = Vec::new();
    lanes.retain(|l| {
        let key = l.name.to_lowercase();
        if seen.contains(&key) {
            false
        } else {
            seen.push(key);
            true
        }
    });
    lanes
}

/// Apply the lane cap and guarantee the global lane: the global-overview lane
/// (the file's own, or a synthetic one) always survives and always runs LAST,
/// so at most `max_lanes - 1` file lanes run. `max_lanes` is clamped >= 1
/// upstream (config).
pub fn select_lanes(parsed: Vec<Lane>, max_lanes: usize) -> LaneSelection {
    let max_lanes = max_lanes.max(1);
    let (mut globals, others): (Vec<Lane>, Vec<Lane>) = parsed
        .into_iter()
        .partition(|l| l.name.eq_ignore_ascii_case(GLOBAL_LANE));
    let global = globals.drain(..).next().unwrap_or_else(|| Lane {
        name: GLOBAL_LANE.to_string(),
        description: GLOBAL_LANE_SCOPE.to_string(),
    });
    let keep = max_lanes - 1; // the global lane takes one slot
    let truncated = others.len().saturating_sub(keep);
    let mut lanes: Vec<Lane> = others.into_iter().take(keep).collect();
    lanes.push(global);
    LaneSelection { lanes, truncated }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_lanes_in_order_with_multiline_descriptions() {
        let lanes = parse_lanes(
            "preamble is ignored\n\
             ## runner\ndaemon lifecycle\nand archives\n\n### sub-heading stays\n\
             ## tui ##\n\
             ## findings\n",
        );
        assert_eq!(lanes.len(), 3);
        assert_eq!(lanes[0].name, "runner");
        assert!(lanes[0].description.contains("daemon lifecycle"));
        assert!(
            lanes[0].description.contains("### sub-heading stays"),
            "deeper headings are scope text, not lane boundaries"
        );
        assert_eq!(lanes[1].name, "tui", "trailing hashes trimmed");
        assert_eq!(lanes[2].name, "findings");
        assert_eq!(lanes[2].description, "", "name-only lane kept");
    }

    #[test]
    fn skips_empty_names_and_dedupes_case_insensitively() {
        let lanes = parse_lanes("## \norphan text\n## Runner\nfirst\n## runner\nsecond\n");
        assert_eq!(lanes.len(), 1);
        assert_eq!(lanes[0].name, "Runner");
        assert_eq!(lanes[0].description, "first", "duplicate keeps the FIRST");
    }

    #[test]
    fn selection_appends_global_last_and_reports_truncation() {
        let parsed = parse_lanes("## a\n## b\n## c\n");
        // Cap 3: global takes a slot, so only a + b survive; c is REPORTED.
        let sel = select_lanes(parsed, 3);
        let names: Vec<&str> = sel.lanes.iter().map(|l| l.name.as_str()).collect();
        assert_eq!(names, vec!["a", "b", GLOBAL_LANE]);
        assert_eq!(sel.truncated, 1);
        assert!(!sel.lanes.last().unwrap().description.is_empty());
    }

    #[test]
    fn selection_keeps_a_file_defined_global_wherever_it_appears() {
        // Any-case match, survives even when listed beyond the cap position,
        // and its user-written scope wins over the synthetic one.
        let parsed = parse_lanes("## a\n## b\n## GLOBAL-overview\nmy own scope\n");
        let sel = select_lanes(parsed, 3);
        let names: Vec<&str> = sel.lanes.iter().map(|l| l.name.as_str()).collect();
        assert_eq!(names, vec!["a", "b", "GLOBAL-overview"]);
        assert_eq!(sel.truncated, 0);
        assert_eq!(sel.lanes[2].description, "my own scope");
        // A similar-but-different name does NOT suppress the synthetic global.
        let sel = select_lanes(parse_lanes("## global-overview-extra\n"), 8);
        assert_eq!(sel.lanes.len(), 2);
        assert_eq!(sel.lanes[1].name, GLOBAL_LANE);
    }

    #[test]
    fn selection_at_exact_capacity_never_warns() {
        // 2 file lanes + global == cap 3: nothing truncated.
        let sel = select_lanes(parse_lanes("## a\n## b\n"), 3);
        assert_eq!(sel.lanes.len(), 3);
        assert_eq!(sel.truncated, 0);
    }

    proptest::proptest! {
        /// Parser soundness: output names are exactly the non-empty `##`
        /// names in source order (pre-dedup subset), and every name in the
        /// output existed as a heading in the input.
        #[test]
        fn parser_never_invents_lanes(lines in proptest::collection::vec("[a-zA-Z#/ ]{0,12}", 0..30)) {
            let text = lines.join("\n");
            let lanes = parse_lanes(&text);
            for lane in &lanes {
                let expected = lines.iter().any(|l| {
                    let t = l.trim();
                    t.chars().take_while(|c| *c == '#').count() == 2
                        && t.trim_matches('#').trim() == lane.name
                });
                proptest::prop_assert!(expected, "invented lane {:?}", lane.name);
            }
        }

        /// Selection always respects the cap and always ends with a global.
        #[test]
        fn selection_respects_cap_and_global(n in 0usize..20, max in 1usize..10) {
            let parsed: Vec<Lane> = (0..n)
                .map(|i| Lane { name: format!("lane{i}"), description: String::new() })
                .collect();
            let total = parsed.len();
            let sel = select_lanes(parsed, max);
            proptest::prop_assert!(sel.lanes.len() <= max.max(1));
            proptest::prop_assert!(
                sel.lanes.last().unwrap().name.eq_ignore_ascii_case(GLOBAL_LANE)
            );
            proptest::prop_assert_eq!(
                sel.lanes.len() - 1 + sel.truncated,
                total,
                "every file lane is either kept or counted as truncated"
            );
        }
    }
}
