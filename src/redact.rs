//! Secret redaction. Applied to every agent line BEFORE it is archived,
//! parsed, or rendered, and to generated reports: the audit trail must be
//! safe to commit and share. gitleaks-style patterns, conservative about
//! false positives (hashes/uuids/hex are allowlisted).

use std::sync::OnceLock;

use regex::Regex;

struct Pattern {
    kind: &'static str,
    re: Regex,
}

fn patterns() -> &'static Vec<Pattern> {
    static PATTERNS: OnceLock<Vec<Pattern>> = OnceLock::new();
    PATTERNS.get_or_init(|| {
        let mk = |kind: &'static str, re: &str| Pattern {
            kind,
            re: Regex::new(re).expect("valid redaction regex"),
        };
        vec![
            // Vendor-shaped keys (specific first, they win over generic).
            mk("aws", r"\bAKIA[0-9A-Z]{16}\b"),
            mk("github", r"\bgh[pousr]_[A-Za-z0-9]{36,255}\b"),
            mk("openai", r"\bsk-[A-Za-z0-9_-]{20,}\b"),
            mk("google", r"\bAIza[0-9A-Za-z_-]{35}\b"),
            mk("slack", r"\bxox[baprs]-[0-9A-Za-z-]{10,}\b"),
            mk("jwt", r"\beyJ[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}\b"),
            // Bearer credentials: space-separated, not an assignment shape.
            mk("bearer", r"(?i)\bbearer\s+[A-Za-z0-9+/_.=\-]{8,}"),
            // key = value assignments (api_key: "...", token=...).
            mk(
                "assignment",
                r#"(?i)\b(api[_-]?key|secret[_-]?key|secret|token|passwd|password|authorization)\b\s*[:=]\s*["']?[A-Za-z0-9+/_.\-]{8,}["']?"#,
            ),
        ]
    })
}

/// Stateful so multi-line PEM blocks survive line-at-a-time processing.
#[derive(Debug, Default)]
pub struct Redactor {
    enabled: bool,
    in_pem: bool,
}

impl Redactor {
    pub fn new(enabled: bool) -> Self {
        Self {
            enabled,
            in_pem: false,
        }
    }

    pub fn line(&mut self, line: &str) -> String {
        if !self.enabled {
            return line.to_string();
        }
        // PEM block state machine.
        if self.in_pem {
            if line.contains("-----END") {
                self.in_pem = false;
            }
            return "[REDACTED:pem]".to_string();
        }
        if line.contains("-----BEGIN") && line.contains("PRIVATE KEY") {
            // Single-line PEM (JSON-escaped) still ends here; multi-line arms the state.
            if !line.contains("-----END") {
                self.in_pem = true;
            }
            return "[REDACTED:pem]".to_string();
        }

        let mut out = line.to_string();
        for p in patterns() {
            if p.re.is_match(&out) {
                out =
                    p.re.replace_all(&out, format!("[REDACTED:{}]", p.kind))
                        .into_owned();
            }
        }
        redact_entropy(&out)
    }

    /// Whole-text convenience (reports).
    pub fn text(&mut self, text: &str) -> String {
        text.lines()
            .map(|l| self.line(l))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// Long mixed-class tokens that look like credentials. Allowlist: pure hex
/// (git shas, digests), uuids, and paths, all common false positives.
fn redact_entropy(line: &str) -> String {
    static CANDIDATE: OnceLock<Regex> = OnceLock::new();
    // NOTE: no '/' in the class, since file paths are long mixed-class tokens too
    // (found live: `.ritual/findings/2026...-plan-review.json` got redacted).
    // Slash-bearing base64 secrets are still caught by the structured
    // patterns (bearer/assignment/vendor keys).
    let re = CANDIDATE.get_or_init(|| Regex::new(r"\b[A-Za-z0-9+=_-]{40,}\b").unwrap());
    re.replace_all(line, |caps: &regex::Captures| {
        let s = caps.get(0).unwrap().as_str();
        let hexish = s.chars().all(|c| c.is_ascii_hexdigit() || c == '-');
        let has_upper = s.chars().any(|c| c.is_ascii_uppercase());
        let has_lower = s.chars().any(|c| c.is_ascii_lowercase());
        let has_digit = s.chars().any(|c| c.is_ascii_digit());
        if hexish || !(has_upper && has_lower && has_digit) {
            s.to_string() // sha/uuid/word-like: keep
        } else {
            "[REDACTED:entropy]".to_string()
        }
    })
    .into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one(line: &str) -> String {
        Redactor::new(true).line(line)
    }

    proptest::proptest! {
        #![proptest_config(proptest::prelude::ProptestConfig::with_cases(128))]

        #[test]
        fn never_panics_and_disabled_is_identity(line in "\\PC{0,200}") {
            let _ = Redactor::new(true).line(&line);
            proptest::prop_assert_eq!(Redactor::new(false).line(&line), line);
        }

        #[test]
        fn planted_aws_keys_never_survive(
            prefix in "[a-zA-Z ]{0,20}",
            key_tail in "[0-9A-Z]{16}",
            suffix in "[a-zA-Z ]{0,20}",
        ) {
            let key = format!("AKIA{key_tail}");
            let out = Redactor::new(true).line(&format!("{prefix} {key} {suffix}"));
            proptest::prop_assert!(!out.contains(&key), "{}", out);
        }
    }

    #[test]
    fn google_and_slack_keys_are_redacted() {
        let google = format!("cfg AIza{}", "Ab1-".repeat(8) + "Ab1");
        assert!(one(&google).contains("[REDACTED:google]"), "{google}");
        assert!(one("hook xoxb-123456789012-secretpart").contains("[REDACTED:slack]"));
        assert!(one("token xoxp-1-abcdefghijkl").contains("[REDACTED:slack]"));
    }

    #[test]
    fn pem_state_machine_edges() {
        // BEGIN without END: everything after is swallowed until an END.
        let mut r = Redactor::new(true);
        assert_eq!(r.line("-----BEGIN RSA PRIVATE KEY-----"), "[REDACTED:pem]");
        assert_eq!(r.line("AAAAB3NzaC1yc2E"), "[REDACTED:pem]");
        assert_eq!(r.line("an innocent-looking line"), "[REDACTED:pem]");
        assert_eq!(r.line("-----END RSA PRIVATE KEY-----"), "[REDACTED:pem]");
        assert_eq!(r.line("back to normal"), "back to normal");

        // Single-line (JSON-escaped) PEM must NOT arm the state machine.
        let mut r = Redactor::new(true);
        assert_eq!(
            r.line("-----BEGIN PRIVATE KEY-----MIIEv-----END PRIVATE KEY-----"),
            "[REDACTED:pem]"
        );
        assert_eq!(r.line("next line untouched"), "next line untouched");

        // Certificates are not secrets, so BEGIN without PRIVATE KEY passes.
        assert_eq!(
            one("-----BEGIN CERTIFICATE-----"),
            "-----BEGIN CERTIFICATE-----"
        );
    }

    #[test]
    fn entropy_boundary_and_hexish_allowlist() {
        // Exactly 40 mixed-class chars trips the candidate; 39 does not.
        let t40 = "Zq7".repeat(13) + "Z";
        assert_eq!(t40.len(), 40);
        assert!(one(&format!("x {t40} y")).contains("[REDACTED:entropy]"));
        let t39 = &t40[..39];
        assert!(!one(&format!("x {t39} y")).contains("REDACTED"));

        // Hexish-with-dashes (uuid-ish, 40+ chars) stays: allowlisted.
        let dashed = "deadbeef-deadbeef-deadbeef-deadbeef-dead1";
        assert_eq!(one(dashed), dashed);
        // A 64-char sha256 stays.
        let sha = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
        assert_eq!(one(sha), sha);
    }

    #[test]
    fn text_processes_whole_documents_with_state() {
        let mut r = Redactor::new(true);
        let out = r.text(
            "line one\n-----BEGIN PRIVATE KEY-----\nMIIEkey\n-----END PRIVATE KEY-----\nline five",
        );
        assert_eq!(
            out,
            "line one\n[REDACTED:pem]\n[REDACTED:pem]\n[REDACTED:pem]\nline five"
        );
    }

    #[test]
    fn vendor_keys_are_redacted() {
        assert_eq!(one("key=AKIAIOSFODNN7EXAMPLE"), "key=[REDACTED:aws]");
        assert!(one("ghp_abcdefghijklmnopqrstuvwxyz0123456789").contains("[REDACTED:github]"));
        assert!(one("using sk-proj-abc123def456ghi789jkl012").contains("[REDACTED:openai]"));
        assert!(
            one("eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0In0.dozjgNryP4J3jVmNHl0w5N_XgL0n3I9P")
                .contains("[REDACTED:jwt]")
        );
    }

    #[test]
    fn assignments_are_redacted() {
        assert!(one(r#"api_key = "hunter2hunter2""#).contains("[REDACTED:assignment]"));
        assert!(one("Authorization: Bearer abc123def456").contains("[REDACTED:bearer]"));
        assert!(one("PASSWORD=supersecretvalue").contains("[REDACTED:assignment]"));
    }

    #[test]
    fn pem_blocks_are_redacted_statefully() {
        let mut r = Redactor::new(true);
        assert_eq!(r.line("-----BEGIN RSA PRIVATE KEY-----"), "[REDACTED:pem]");
        assert_eq!(r.line("MIIEowIBAAKCAQEA0Z3VS5JJcds3xfn"), "[REDACTED:pem]");
        assert_eq!(r.line("-----END RSA PRIVATE KEY-----"), "[REDACTED:pem]");
        assert_eq!(r.line("normal line after"), "normal line after");
    }

    #[test]
    fn false_positives_survive() {
        // git sha (pure hex)
        let sha = "aeb1c2d3e4f5061728394a5b6c7d8e9f01234567aeb1c2d3";
        assert_eq!(one(sha), sha);
        // long lowercase word/path-ish token: no mixed classes
        let word = "supercalifragilisticexpialidocioussupercali";
        assert_eq!(one(word), word);
        // normal prose untouched
        assert_eq!(
            one("the tokenizer splits words"),
            "the tokenizer splits words"
        );
    }

    #[test]
    fn file_paths_are_not_entropy() {
        // Regression: found live, the findings path was redacted in the
        // run archive because '/' was in the entropy charset.
        let line = r#""file_path": "/home/u/Documents/project/ritual/.ritual/findings/20260711T234357Z-plan-review.json""#;
        assert_eq!(one(line), line);
    }

    #[test]
    fn entropy_token_is_redacted() {
        let line = "leak: aB3dE5fG7hJ9kL1mN3pQ5rS7tU9vW1xY3zA5bC7d";
        assert!(one(line).contains("[REDACTED:entropy]"), "{}", one(line));
    }

    #[test]
    fn disabled_passes_through() {
        let mut r = Redactor::new(false);
        let s = "api_key = \"hunter2hunter2\"";
        assert_eq!(r.line(s), s);
    }
}
