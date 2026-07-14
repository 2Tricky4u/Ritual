//! Parser for the batch plan-fix ANSWERS contract: the run's final message
//! must end with a block like
//!
//! ```text
//! ANSWERS:
//! #1: FIXED
//! #2: DECLINED needs a spec change first
//! ```
//!
//! Pure and deliberately tolerant - model output drifts, the parser must not.

use std::collections::HashMap;

/// One finding's verdict from the batch run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AnswerVerdict {
    Fixed,
    Declined(String),
}

/// Extract the per-finding verdicts from a run's final text. Tolerant:
/// the `ANSWERS:` anchor is case-insensitive and may sit anywhere (the LAST
/// block wins when the model repeats itself); verdict lines match
/// `#<n>: FIXED|DECLINED [reason]` case-insensitively, junk lines inside the
/// block are skipped, duplicate numbers last-win, a missing reason becomes
/// "no reason given". No block at all -> empty map (the caller treats every
/// queued finding as declined).
pub fn parse_answers(text: &str) -> HashMap<u32, AnswerVerdict> {
    let lines: Vec<&str> = text.lines().collect();
    let anchor = lines
        .iter()
        .rposition(|l| l.trim().to_ascii_lowercase().starts_with("answers:"));
    let Some(start) = anchor else {
        return HashMap::new();
    };
    let mut out = HashMap::new();
    for line in &lines[start + 1..] {
        let Some((n, verdict)) = parse_line(line) else {
            continue;
        };
        out.insert(n, verdict);
    }
    out
}

/// `#3: FIXED` / `#3: DECLINED because …` -> (3, verdict). None for junk.
fn parse_line(line: &str) -> Option<(u32, AnswerVerdict)> {
    let t = line.trim().strip_prefix('#')?;
    let (num, rest) = t.split_once(':')?;
    let n: u32 = num.trim().parse().ok()?;
    let rest = rest.trim();
    let lower = rest.to_ascii_lowercase();
    if lower.starts_with("fixed") {
        return Some((n, AnswerVerdict::Fixed));
    }
    if let Some(tail) = lower.strip_prefix("declined") {
        // Take the reason from the ORIGINAL casing, same offset.
        let reason = rest[rest.len() - tail.len()..]
            .trim_start_matches([':', '-', '-', ' '])
            .trim();
        let reason = if reason.is_empty() {
            "no reason given".to_string()
        } else {
            reason.to_string()
        };
        return Some((n, AnswerVerdict::Declined(reason)));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_canonical_block() {
        let text =
            "I fixed steps 2 and 4.\n\nANSWERS:\n#1: FIXED\n#2: DECLINED needs a spec change\n";
        let v = parse_answers(text);
        assert_eq!(v.len(), 2);
        assert_eq!(v[&1], AnswerVerdict::Fixed);
        assert_eq!(v[&2], AnswerVerdict::Declined("needs a spec change".into()));
    }

    #[test]
    fn tolerates_case_junk_and_surrounding_text() {
        let text = "preamble\nanswers:\n  #3 : fixed (rewrote the retention rule)\nsome commentary\n#10: Declined - overlaps finding 3\ntrailing prose\n";
        let v = parse_answers(text);
        assert_eq!(v[&3], AnswerVerdict::Fixed);
        assert_eq!(v[&10], AnswerVerdict::Declined("overlaps finding 3".into()));
        assert_eq!(v.len(), 2, "junk lines never become verdicts");
    }

    #[test]
    fn last_block_and_duplicate_numbers_win() {
        let text = "ANSWERS:\n#1: DECLINED first try\n\nrevised…\n\nANSWERS:\n#1: FIXED\n#1: DECLINED changed my mind\n";
        let v = parse_answers(text);
        assert_eq!(v.len(), 1);
        assert_eq!(v[&1], AnswerVerdict::Declined("changed my mind".into()));
    }

    #[test]
    fn declined_reason_defaults_when_empty() {
        let v = parse_answers("ANSWERS:\n#7: DECLINED\n");
        assert_eq!(v[&7], AnswerVerdict::Declined("no reason given".into()));
    }

    #[test]
    fn no_block_yields_empty() {
        assert!(parse_answers("I did some things to the plan.").is_empty());
        assert!(parse_answers("").is_empty());
        // A lone "#1: FIXED" without the anchor is not a verdict.
        assert!(parse_answers("#1: FIXED\n").is_empty());
    }
}
