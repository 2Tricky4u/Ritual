//! The generated architecture map: `.ritual/architecture.md` grounds the Plan
//! stage in the code that actually exists. The agent writes a CANDIDATE file
//! (`architecture.md.new`); [`finalize`] validates it and atomically installs
//! it, then stamps the `.ritual`-scoped tree fingerprint into a sidecar so
//! guidance can flag the map as stale when the source tree moves on.

use std::path::PathBuf;

use anyhow::Result;

use crate::state::RitualDirs;

/// Freshness of the architecture map, advisory only (never blocks a launch).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchStatus {
    /// No doc, or the doc has no meaningful content.
    Missing,
    /// Stamp matches the current scoped fingerprint.
    Fresh,
    /// Stamp and current fingerprint disagree: code changed since generation.
    Stale,
    /// No stamp, or no current fingerprint (non-git): never reported stale.
    Unknown,
}

/// Pure freshness verdict from the three facts guidance/status/doctor share.
pub fn status(doc_meaningful: bool, stamp: Option<&str>, current: Option<&str>) -> ArchStatus {
    let _ = (doc_meaningful, stamp, current);
    unimplemented!("phase 1 red")
}

/// The user-facing nudge for a status; only Missing/Stale speak.
pub fn note(status: ArchStatus) -> Option<&'static str> {
    let _ = status;
    unimplemented!("phase 1 red")
}

/// The generation fingerprint recorded at the last successful refresh.
pub fn read_stamp(dirs: &RitualDirs) -> Option<String> {
    let _ = dirs;
    unimplemented!("phase 1 red")
}

/// Persist the generation fingerprint sidecar.
pub fn write_stamp(dirs: &RitualDirs, fp: &str) -> Result<()> {
    let _ = (dirs, fp);
    unimplemented!("phase 1 red")
}

/// Structural contract for a generated candidate: meaningful prose plus the
/// prescribed headings. The ~200-line cap is prompt guidance, never enforced.
pub fn validate_candidate(text: &str) -> Result<()> {
    let _ = text;
    unimplemented!("phase 1 red")
}

/// Validate the candidate and install it: old doc -> `.bak`, candidate ->
/// live doc, then stamp the scoped fingerprint (post-swap - the swap itself
/// is tree dirt). Returns whether staleness tracking is active (git tree).
/// EVERY failure leaves the live doc + sidecar untouched and removes the
/// candidate.
pub fn finalize(dirs: &RitualDirs, run_ok: bool) -> Result<bool> {
    let _ = (dirs, run_ok);
    unimplemented!("phase 1 red")
}

/// `.ritual/architecture.md.bak`: the pre-refresh map (successful swaps only).
pub fn backup_file(dirs: &RitualDirs) -> PathBuf {
    dirs.architecture_file().with_extension("md.bak")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn dirs(tmp: &tempfile::TempDir) -> RitualDirs {
        let d = RitualDirs::new(tmp.path());
        std::fs::create_dir_all(d.root()).unwrap();
        d
    }

    fn git_init(p: &Path) {
        for args in [
            &["init", "-q", "-b", "main"][..],
            &["config", "user.email", "t@t"][..],
            &["config", "user.name", "t"][..],
        ] {
            std::process::Command::new("git")
                .args(args)
                .current_dir(p)
                .output()
                .unwrap();
        }
    }

    const VALID: &str = "# Architecture map\n\nA one-paragraph overview.\n\n\
                         ## Modules\n- src: everything\n\n\
                         ## Extension seams\n- new subcommand: copy the Audit arm\n";

    #[test]
    fn status_truth_table() {
        use ArchStatus::*;
        let fp = Some("head:digest");
        let other = Some("head:other");
        // (doc_meaningful, stamp, current) -> status
        let table = [
            (false, None, None, Missing),
            (false, fp, fp, Missing), // a sidecar can't resurrect a missing doc
            (true, None, None, Unknown),
            (true, None, fp, Unknown), // legacy doc without a stamp
            (true, fp, None, Unknown), // non-git: unknown, never stale
            (true, fp, fp, Fresh),
            (true, fp, other, Stale),
        ];
        for (meaningful, stamp, current, want) in table {
            assert_eq!(
                status(meaningful, stamp, current),
                want,
                "status({meaningful}, {stamp:?}, {current:?})"
            );
        }
    }

    #[test]
    fn only_missing_and_stale_carry_notes() {
        let missing = note(ArchStatus::Missing).expect("missing nudges");
        assert!(missing.contains("ritual architect"), "{missing}");
        let stale = note(ArchStatus::Stale).expect("stale nudges");
        assert!(stale.contains("code changed"), "{stale}");
        assert_eq!(note(ArchStatus::Fresh), None);
        assert_eq!(note(ArchStatus::Unknown), None);
    }

    #[test]
    fn validate_accepts_a_minimal_map_and_an_oversized_one() {
        validate_candidate(VALID).expect("minimal valid map");
        // ~200 lines is advisory: a big-but-valid map still passes.
        let big = format!("{VALID}{}", "- another seam bullet\n".repeat(300));
        validate_candidate(&big).expect("oversized map still accepted");
    }

    #[test]
    fn validate_rejects_empty_whitespace_and_comment_only() {
        assert!(validate_candidate("").is_err(), "empty");
        assert!(validate_candidate("  \n\t\n").is_err(), "whitespace");
        // Headings + comments only: no meaningful content.
        assert!(
            validate_candidate(
                "# Architecture map\n<!-- todo -->\n## Modules\n## Extension seams\n"
            )
            .is_err(),
            "comment-only body"
        );
    }

    #[test]
    fn validate_requires_each_prescribed_heading() {
        for missing in ["# Architecture map", "## Modules", "## Extension seams"] {
            let text: String = VALID
                .lines()
                .filter(|l| l.trim() != missing)
                .map(|l| format!("{l}\n"))
                .collect();
            let err = validate_candidate(&text).expect_err(missing);
            assert!(
                err.to_string()
                    .contains(missing.trim_start_matches(['#', ' '])),
                "error names the missing section: {err}"
            );
        }
    }

    #[test]
    fn validate_rejects_heading_lookalikes_in_prose() {
        // Mentions inside prose or extended headings are not the headings.
        let text = "# Architecture map\n\nsee ## Modules inline\n\
                    ## Modules and more\n## Extension seams\nprose\n";
        assert!(validate_candidate(text).is_err());
    }

    proptest::proptest! {
        /// Valid iff ALL three prescribed headings are present (any order),
        /// given a meaningful body - order/duplication must not matter.
        #[test]
        fn heading_subsets_validate_iff_complete(mask in 0u8..8, dup in 0usize..3) {
            let all = ["# Architecture map", "## Modules", "## Extension seams"];
            let mut text = String::from("real prose so the doc is meaningful\n");
            for (i, h) in all.iter().enumerate() {
                if mask & (1 << i) != 0 {
                    for _ in 0..=(dup % 2) {
                        text.push_str(h);
                        text.push('\n');
                    }
                }
            }
            let ok = validate_candidate(&text).is_ok();
            proptest::prop_assert_eq!(ok, mask == 0b111);
        }
    }

    #[test]
    fn stamp_roundtrips() {
        let tmp = tempfile::tempdir().unwrap();
        let d = dirs(&tmp);
        assert_eq!(read_stamp(&d), None, "no sidecar yet");
        write_stamp(&d, "head:digest").unwrap();
        assert_eq!(read_stamp(&d).as_deref(), Some("head:digest"));
    }

    #[test]
    fn finalize_installs_candidate_backs_up_and_stamps() {
        let tmp = tempfile::tempdir().unwrap();
        let d = dirs(&tmp);
        git_init(tmp.path());
        std::fs::write(d.architecture_file(), "the old map\n").unwrap();
        std::fs::write(d.architecture_candidate_file(), VALID).unwrap();

        let tracked = finalize(&d, true).expect("valid candidate installs");
        assert!(tracked, "git tree: staleness tracked");
        assert_eq!(
            std::fs::read_to_string(d.architecture_file()).unwrap(),
            VALID
        );
        assert_eq!(
            std::fs::read_to_string(backup_file(&d)).unwrap(),
            "the old map\n"
        );
        assert!(
            !d.architecture_candidate_file().exists(),
            "candidate consumed"
        );
        // Stamp equals the CURRENT scoped fingerprint (computed post-swap).
        let now = crate::provenance::arch_fingerprint(tmp.path()).unwrap();
        assert_eq!(read_stamp(&d).as_deref(), Some(now.as_str()));
    }

    #[test]
    fn finalize_rejects_a_failed_run_despite_a_valid_candidate() {
        let tmp = tempfile::tempdir().unwrap();
        let d = dirs(&tmp);
        std::fs::write(d.architecture_file(), "the old map\n").unwrap();
        std::fs::write(d.architecture_fingerprint_file(), "old:stamp\n").unwrap();
        std::fs::write(d.architecture_candidate_file(), VALID).unwrap();

        assert!(finalize(&d, false).is_err(), "run outcome is authoritative");
        assert_eq!(
            std::fs::read_to_string(d.architecture_file()).unwrap(),
            "the old map\n",
            "live doc untouched"
        );
        assert_eq!(
            std::fs::read_to_string(d.architecture_fingerprint_file()).unwrap(),
            "old:stamp\n",
            "sidecar untouched"
        );
        assert!(
            !d.architecture_candidate_file().exists(),
            "candidate cleaned"
        );
        assert!(!backup_file(&d).exists(), "no backup on failure");
    }

    #[test]
    fn finalize_requires_a_candidate() {
        let tmp = tempfile::tempdir().unwrap();
        let d = dirs(&tmp);
        std::fs::write(d.architecture_file(), "the old map\n").unwrap();

        let err = finalize(&d, true).expect_err("no candidate = no usable map");
        assert!(err.to_string().contains("no usable"), "{err}");
        assert_eq!(
            std::fs::read_to_string(d.architecture_file()).unwrap(),
            "the old map\n"
        );
    }

    #[test]
    fn finalize_rejects_an_invalid_candidate_and_leaves_no_debris() {
        let tmp = tempfile::tempdir().unwrap();
        let d = dirs(&tmp);
        // No pre-existing doc: failure must not create doc/.bak/sidecar.
        std::fs::write(d.architecture_candidate_file(), "just prose, no headings\n").unwrap();

        assert!(finalize(&d, true).is_err());
        assert!(
            !d.architecture_file().exists(),
            "no doc from a bad candidate"
        );
        assert!(!d.architecture_fingerprint_file().exists(), "no sidecar");
        assert!(!backup_file(&d).exists(), "no backup");
        assert!(
            !d.architecture_candidate_file().exists(),
            "candidate cleaned"
        );
    }

    #[test]
    fn finalize_without_git_installs_but_disables_staleness() {
        let tmp = tempfile::tempdir().unwrap();
        let d = dirs(&tmp);
        // A stale sidecar from a past git life must not survive to lie.
        std::fs::write(d.architecture_fingerprint_file(), "old:stamp\n").unwrap();
        std::fs::write(d.architecture_candidate_file(), VALID).unwrap();

        let tracked = finalize(&d, true).expect("non-git still installs");
        assert!(!tracked, "no git: staleness tracking off");
        assert_eq!(
            std::fs::read_to_string(d.architecture_file()).unwrap(),
            VALID
        );
        assert!(
            !d.architecture_fingerprint_file().exists(),
            "stale sidecar removed"
        );
    }
}
