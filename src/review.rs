//! Parser for the code-fix RE-REVIEW contract: the read-only reviewer's final
//! message must end with a block like
//!
//! ```text
//! REVIEW:
//! #1: RESOLVED
//! #2: UNRESOLVED still panics on empty input
//! REGRESSIONS: NONE
//! ```
//!
//! Pure and deliberately tolerant, mirroring `crate::answers` - model output
//! drifts, the parser must not.

use std::collections::HashMap;

/// One finding's re-review outcome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FindingReview {
    Resolved,
    Unresolved(String),
}

/// The whole re-review verdict: a per-finding map plus an overall regression
/// judgement (`None` = the reviewer said NONE / gave no regression line).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReviewVerdict {
    pub per_finding: HashMap<u32, FindingReview>,
    pub regressions: Option<String>,
}

/// Extract the re-review verdict from the run's final text. The `REVIEW:`
/// anchor is case-insensitive and the LAST block wins; per-finding lines match
/// `#<n>: RESOLVED|UNRESOLVED [reason]`; a `REGRESSIONS:` line anywhere sets the
/// overall judgement (`NONE`/empty -> None, else the description). No block ->
/// empty map + `None` (the caller's strict rule treats a missing verdict as a
/// failure).
pub fn parse_review(text: &str) -> ReviewVerdict {
    let lines: Vec<&str> = text.lines().collect();
    // A regressions line may appear anywhere; take the LAST one.
    let regressions = lines
        .iter()
        .rev()
        .find_map(|l| parse_regressions(l))
        .flatten();
    let anchor = lines
        .iter()
        .rposition(|l| l.trim().to_ascii_lowercase().starts_with("review:"));
    let mut per_finding = HashMap::new();
    if let Some(start) = anchor {
        for line in &lines[start + 1..] {
            if let Some((n, v)) = parse_line(line) {
                per_finding.insert(n, v);
            }
        }
    }
    ReviewVerdict {
        per_finding,
        regressions,
    }
}

/// `#3: RESOLVED` / `#3: UNRESOLVED still broken` -> (3, review). None for junk.
fn parse_line(line: &str) -> Option<(u32, FindingReview)> {
    let t = line.trim().strip_prefix('#')?;
    let (num, rest) = t.split_once(':')?;
    let n: u32 = num.trim().parse().ok()?;
    let rest = rest.trim();
    let lower = rest.to_ascii_lowercase();
    if let Some(tail) = lower.strip_prefix("unresolved") {
        let reason = rest[rest.len() - tail.len()..]
            .trim_start_matches([':', '-', ' '])
            .trim();
        let reason = if reason.is_empty() {
            "no reason given".to_string()
        } else {
            reason.to_string()
        };
        return Some((n, FindingReview::Unresolved(reason)));
    }
    if lower.starts_with("resolved") {
        return Some((n, FindingReview::Resolved));
    }
    None
}

/// `REGRESSIONS: <desc>` -> Some(Some(desc)); `REGRESSIONS: NONE` (or empty) ->
/// Some(None); not a regressions line -> None.
fn parse_regressions(line: &str) -> Option<Option<String>> {
    let t = line.trim();
    let lower = t.to_ascii_lowercase();
    let tail = lower.strip_prefix("regressions:")?;
    let desc = t[t.len() - tail.len()..].trim();
    if desc.is_empty() || desc.eq_ignore_ascii_case("none") {
        Some(None)
    } else {
        Some(Some(desc.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_canonical_block() {
        let text = "Looks good.\n\nREVIEW:\n#1: RESOLVED\n#2: UNRESOLVED still panics on empty\nREGRESSIONS: NONE\n";
        let v = parse_review(text);
        assert_eq!(v.per_finding[&1], FindingReview::Resolved);
        assert_eq!(
            v.per_finding[&2],
            FindingReview::Unresolved("still panics on empty".into())
        );
        assert_eq!(v.regressions, None);
    }

    #[test]
    fn regressions_present() {
        let text = "REVIEW:\n#1: RESOLVED\nREGRESSIONS: breaks the retry path in mod.rs\n";
        let v = parse_review(text);
        assert_eq!(v.per_finding[&1], FindingReview::Resolved);
        assert_eq!(
            v.regressions,
            Some("breaks the retry path in mod.rs".into())
        );
    }

    #[test]
    fn tolerates_case_and_junk() {
        let text = "preamble\nreview:\n  #3 : resolved (rewrote the guard)\ncommentary\n#10: Unresolved - the null case remains\nregressions: none\ntrailing\n";
        let v = parse_review(text);
        assert_eq!(v.per_finding[&3], FindingReview::Resolved);
        assert_eq!(
            v.per_finding[&10],
            FindingReview::Unresolved("the null case remains".into())
        );
        assert_eq!(v.per_finding.len(), 2);
        assert_eq!(v.regressions, None);
    }

    #[test]
    fn last_block_wins() {
        let text = "REVIEW:\n#1: UNRESOLVED first pass\n\nreconsidered\n\nREVIEW:\n#1: RESOLVED\nREGRESSIONS: NONE\n";
        let v = parse_review(text);
        assert_eq!(v.per_finding.len(), 1);
        assert_eq!(v.per_finding[&1], FindingReview::Resolved);
    }

    #[test]
    fn no_block_is_empty_and_no_regressions() {
        let v = parse_review("I reviewed the diff and it seems fine.");
        assert!(v.per_finding.is_empty());
        assert_eq!(v.regressions, None);
        assert!(parse_review("").per_finding.is_empty());
    }
}
