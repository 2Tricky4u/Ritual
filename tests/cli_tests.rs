//! End-to-end subcommand tests. Agent runs use tests/fake_agent.sh via the
//! RITUAL_CLAUDE_CMD / RITUAL_CODEX_CMD seams; zero tokens burned.

use assert_cmd::Command;
use predicates::prelude::*;

fn fake_agent() -> String {
    format!("{}/tests/fake_agent.sh", env!("CARGO_MANIFEST_DIR"))
}

/// A tempdir project with git repo (branch main) and .ritual initialized.
fn setup_project() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    std::process::Command::new("git")
        .args(["init", "-q", "-b", "main"])
        .current_dir(tmp.path())
        .status()
        .unwrap();
    std::fs::write(tmp.path().join("Cargo.toml"), "[package]\nname=\"x\"\n").unwrap();
    Command::cargo_bin("ritual")
        .unwrap()
        .current_dir(tmp.path())
        .arg("init")
        .assert()
        .success();
    tmp
}

#[test]
fn init_scaffolds_project() {
    let tmp = setup_project();
    assert!(tmp.path().join(".ritual/state.json").exists());
    assert!(tmp.path().join("check.sh").exists());
    let gitignore = std::fs::read_to_string(tmp.path().join(".gitignore")).unwrap();
    assert!(gitignore.contains(".ritual/runs/"));
}

#[test]
fn init_skills_installs_the_workbench() {
    let tmp = setup_project();
    let fake_home = tmp.path().join("claude-home");
    Command::cargo_bin("ritual")
        .unwrap()
        .current_dir(tmp.path())
        .env("RITUAL_CLAUDE_HOME", &fake_home)
        .args(["init", "--skills"])
        .assert()
        .success()
        .stdout(predicate::str::contains("workbench →"))
        .stdout(predicate::str::contains("NOT auto-merged"));
    assert!(fake_home.join("skills/plan-review/SKILL.md").exists());
    assert!(fake_home.join("skills/consensus/SKILL.md").exists());
    assert!(fake_home.join("agents/code-reviewer.md").exists());
    assert!(fake_home.join("hooks/check-on-edit.sh").exists());

    // Idempotent second run: everything unchanged.
    Command::cargo_bin("ritual")
        .unwrap()
        .current_dir(tmp.path())
        .env("RITUAL_CLAUDE_HOME", &fake_home)
        .args(["init", "--skills"])
        .assert()
        .success()
        .stdout(predicate::str::contains("0 created, 0 updated"));
}

#[test]
fn skills_diff_reports_identical_divergent_and_missing() {
    let tmp = setup_project();
    let fake_home = tmp.path().join("claude-home");
    Command::cargo_bin("ritual")
        .unwrap()
        .current_dir(tmp.path())
        .env("RITUAL_CLAUDE_HOME", &fake_home)
        .args(["init", "--skills"])
        .assert()
        .success();

    // Everything freshly installed -> identical, no divergence footer.
    Command::cargo_bin("ritual")
        .unwrap()
        .current_dir(tmp.path())
        .env("RITUAL_CLAUDE_HOME", &fake_home)
        .args(["skills", "diff"])
        .assert()
        .success()
        .stdout(predicate::str::contains("tdd").and(predicate::str::contains("identical")))
        .stdout(predicate::str::contains("divergent").not());

    // Local edit + a removed skill -> flagged with the first divergent line.
    let tdd = fake_home.join("skills/tdd/SKILL.md");
    let mut text = std::fs::read_to_string(&tdd).unwrap();
    text = text.replacen('\n', "\nLOCAL TWEAK\n", 1);
    std::fs::write(&tdd, text).unwrap();
    std::fs::remove_dir_all(fake_home.join("skills/consensus")).unwrap();

    Command::cargo_bin("ritual")
        .unwrap()
        .current_dir(tmp.path())
        .env("RITUAL_CLAUDE_HOME", &fake_home)
        .args(["skills", "diff"])
        .assert()
        .success()
        .stdout(predicate::str::contains("differs at line 2"))
        .stdout(predicate::str::contains("installed: LOCAL TWEAK"))
        .stdout(predicate::str::contains("MISSING"))
        .stdout(predicate::str::contains("2 divergent"));
}

#[test]
fn status_renders_empty_and_with_feature() {
    let tmp = setup_project();
    Command::cargo_bin("ritual")
        .unwrap()
        .current_dir(tmp.path())
        .arg("status")
        .assert()
        .success()
        .stdout(predicate::str::contains("no features yet"));

    Command::cargo_bin("ritual")
        .unwrap()
        .current_dir(tmp.path())
        .args(["new", "Test", "Feature"])
        .assert()
        .success();

    Command::cargo_bin("ritual")
        .unwrap()
        .current_dir(tmp.path())
        .arg("status")
        .assert()
        .success()
        .stdout(predicate::str::contains("Test Feature"))
        .stdout(predicate::str::contains("plan-review"));
}

#[test]
fn run_plan_review_e2e_with_fake_agent() {
    let tmp = setup_project();
    // A plan must exist (slug for branch "main" is "main").
    std::fs::create_dir_all(tmp.path().join(".ritual/features/main")).unwrap();
    std::fs::write(tmp.path().join(".ritual/features/main/plan.md"), "# plan").unwrap();

    Command::cargo_bin("ritual")
        .unwrap()
        .current_dir(tmp.path())
        .env("RITUAL_CLAUDE_CMD", fake_agent())
        .env("RITUAL_CODEX_CMD", fake_agent()) // `login status` preflight -> exit 0
        .env("FAKE_AGENT_DELAY", "0")
        .env(
            "FAKE_AGENT_FINDINGS",
            ".ritual/findings/20260711T200000Z-plan-review.json",
        )
        .args(["run", "plan-review"])
        .assert()
        .success()
        .stdout(predicate::str::contains("plan-review ok"));

    // Artifacts: raw archive + meta + state updated to done.
    let runs: Vec<_> = std::fs::read_dir(tmp.path().join(".ritual/runs"))
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert!(
        runs.iter().any(|f| f.ends_with(".jsonl")),
        "raw archive missing: {runs:?}"
    );
    assert!(
        runs.iter().any(|f| f.ends_with(".meta.json")),
        "meta missing: {runs:?}"
    );

    let state = std::fs::read_to_string(tmp.path().join(".ritual/state.json")).unwrap();
    assert!(state.contains(r#""plan_review""#));
    assert!(state.contains(r#""status": "done""#));

    // History shows the run.
    Command::cargo_bin("ritual")
        .unwrap()
        .current_dir(tmp.path())
        .arg("history")
        .assert()
        .success()
        .stdout(predicate::str::contains("plan-review"));

    // Findings browser shows the canned finding, which is a confirmed
    // critical, so the scriptability contract demands exit code 1.
    Command::cargo_bin("ritual")
        .unwrap()
        .current_dir(tmp.path())
        .arg("findings")
        .assert()
        .code(1)
        .stdout(predicate::str::contains("Canned test finding"));
}

#[test]
fn mutants_turns_survivors_into_findings() {
    let tmp = setup_project();
    let fake = format!("{}/tests/fake_mutants.sh", env!("CARGO_MANIFEST_DIR"));
    // A commit on main + an uncommitted change give `git diff main` content.
    std::process::Command::new("git")
        .args(["add", "-A"])
        .current_dir(tmp.path())
        .status()
        .unwrap();
    std::process::Command::new("git")
        .args([
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "commit",
            "-qm",
            "init",
        ])
        .current_dir(tmp.path())
        .status()
        .unwrap();

    // Empty diff -> explicit no-op.
    Command::cargo_bin("ritual")
        .unwrap()
        .current_dir(tmp.path())
        .env("RITUAL_MUTANTS_CMD", &fake)
        .arg("mutants")
        .assert()
        .success()
        .stdout(predicate::str::contains("nothing to mutate"));

    // Modify a TRACKED file (untracked ones never appear in `git diff`).
    std::fs::write(
        tmp.path().join("Cargo.toml"),
        "[package]\nname=\"x\"\nversion=\"0.2.0\"\n",
    )
    .unwrap();
    let argv_log = tmp.path().join("mutants-argv.log");
    Command::cargo_bin("ritual")
        .unwrap()
        .current_dir(tmp.path())
        .env("RITUAL_MUTANTS_CMD", &fake)
        .env("FAKE_MUTANTS_ARGV_LOG", &argv_log)
        .arg("mutants")
        .assert()
        .success()
        .stdout(predicate::str::contains("1 caught, 1 missed"))
        .stdout(predicate::str::contains("surviving mutant(s)"));

    // The tool got the diff-scoped invocation.
    let argv = std::fs::read_to_string(&argv_log).unwrap();
    assert!(argv.contains("--in-diff"));
    assert!(argv.contains("--no-shuffle"));
    assert!(argv.contains("--timeout"));

    // The survivor is now a major/confirmed finding in the findings dir.
    let findings: Vec<_> = std::fs::read_dir(tmp.path().join(".ritual/findings"))
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().ends_with("-mutants.json"))
        .collect();
    assert_eq!(findings.len(), 1);
    let text = std::fs::read_to_string(findings[0].path()).unwrap();
    assert!(text.contains("surviving mutant: FnValue in canned_fn"));
    assert!(text.contains(r#""severity": "major""#));
    assert!(text.contains("mutated to: true"));

    // Baseline-red (exit 4) is a hard error with advice.
    Command::cargo_bin("ritual")
        .unwrap()
        .current_dir(tmp.path())
        .env("RITUAL_MUTANTS_CMD", &fake)
        .env("FAKE_MUTANTS_EXIT", "4")
        .arg("mutants")
        .assert()
        .failure()
        .stderr(predicate::str::contains("baseline tests already failing"));
}

#[test]
fn coderabbit_reviews_land_as_unconfirmed_findings() {
    let tmp = setup_project();
    let fake = format!("{}/tests/fake_coderabbit.sh", env!("CARGO_MANIFEST_DIR"));
    std::fs::write(
        tmp.path().join(".ritual/config.toml"),
        "[coderabbit]\nenabled = true\n",
    )
    .unwrap();
    std::fs::create_dir_all(tmp.path().join(".ritual/features/main")).unwrap();

    Command::cargo_bin("ritual")
        .unwrap()
        .current_dir(tmp.path())
        .env("RITUAL_CLAUDE_CMD", fake_agent())
        .env("RITUAL_CODEX_CMD", fake_agent())
        .env("RITUAL_CODERABBIT_CMD", &fake)
        .env("FAKE_AGENT_DELAY", "0")
        .env(
            "FAKE_AGENT_FINDINGS",
            ".ritual/findings/20260711T230000Z-dual-review.json",
        )
        .args(["run", "dual-review"])
        .assert()
        .success()
        .stdout(predicate::str::contains("coderabbit review →"));

    let path = std::fs::read_dir(tmp.path().join(".ritual/findings"))
        .unwrap()
        .filter_map(|e| e.ok())
        .find(|e| {
            e.file_name()
                .to_string_lossy()
                .ends_with("-coderabbit.json")
        })
        .expect("coderabbit findings file")
        .path();
    let text = std::fs::read_to_string(path).unwrap();
    assert!(text.contains("Canned rabbit finding"));
    assert!(text.contains(r#""verdict": "unconfirmed""#));
    assert!(text.contains(r#""coderabbit""#));

    // A rate-limited/failed review is a notice, never a pipeline failure.
    Command::cargo_bin("ritual")
        .unwrap()
        .current_dir(tmp.path())
        .env("RITUAL_CLAUDE_CMD", fake_agent())
        .env("RITUAL_CODEX_CMD", fake_agent())
        .env("RITUAL_CODERABBIT_CMD", &fake)
        .env("FAKE_CODERABBIT_EXIT", "1")
        .env("FAKE_AGENT_DELAY", "0")
        .env(
            "FAKE_AGENT_FINDINGS",
            ".ritual/findings/20260711T230001Z-dual-review.json",
        )
        .args(["run", "dual-review"])
        .assert()
        .success()
        .stdout(predicate::str::contains("coderabbit skipped"));
}

#[test]
fn sandbox_wrapper_wraps_the_detached_agent() {
    let tmp = setup_project();
    let wrapper = format!("{}/tests/fake_wrapper.sh", env!("CARGO_MANIFEST_DIR"));
    let log = tmp.path().join("wrapper.log");
    std::fs::write(
        tmp.path().join(".ritual/config.toml"),
        format!("[sandbox]\nenabled = true\nwrapper = \"{wrapper}\"\n"),
    )
    .unwrap();
    std::fs::create_dir_all(tmp.path().join(".ritual/features/main")).unwrap();
    std::fs::write(tmp.path().join(".ritual/features/main/plan.md"), "# plan").unwrap();

    Command::cargo_bin("ritual")
        .unwrap()
        .current_dir(tmp.path())
        .env("RITUAL_CLAUDE_CMD", fake_agent())
        .env("RITUAL_CODEX_CMD", fake_agent())
        .env("FAKE_AGENT_DELAY", "0")
        .env("FAKE_WRAPPER_LOG", &log)
        .env(
            "FAKE_AGENT_FINDINGS",
            ".ritual/findings/20260711T220000Z-plan-review.json",
        )
        .args(["run", "plan-review"])
        .assert()
        .success()
        .stdout(predicate::str::contains("plan-review ok"));

    // The daemon spawned wrapper -> agent, and the run still completed.
    let logged = std::fs::read_to_string(&log).expect("wrapper ran");
    assert!(logged.contains("wrapped:"));
    assert!(logged.contains("fake_agent.sh"));
    // The meta records the EFFECTIVE argv (wrapper included) for repro.
    let meta = std::fs::read_dir(tmp.path().join(".ritual/runs"))
        .unwrap()
        .filter_map(|e| e.ok())
        .find(|e| e.file_name().to_string_lossy().ends_with(".meta.json"))
        .expect("meta written");
    let text = std::fs::read_to_string(meta.path()).unwrap();
    assert!(
        text.contains("fake_wrapper.sh"),
        "wrapper not in meta argv: {text}"
    );
}

#[test]
fn secrets_gate_blocks_until_dismissed() {
    let tmp = setup_project();
    let fake = format!("{}/tests/fake_gitleaks.sh", env!("CARGO_MANIFEST_DIR"));
    // An untracked file is exactly the agent-wrote-a-.env attack surface.
    std::fs::write(tmp.path().join("leaky.py"), "x = 1\napi_key = \"h\"\n").unwrap();

    Command::cargo_bin("ritual")
        .unwrap()
        .current_dir(tmp.path())
        .env("RITUAL_GITLEAKS_CMD", &fake)
        .arg("secrets")
        .assert()
        .code(1)
        .stdout(predicate::str::contains("1 leak(s)"))
        .stdout(predicate::str::contains(".gitleaksignore"));

    // The hit is a critical/confirmed finding -> the findings contract blocks.
    let path = std::fs::read_dir(tmp.path().join(".ritual/findings"))
        .unwrap()
        .filter_map(|e| e.ok())
        .find(|e| e.file_name().to_string_lossy().ends_with("-secrets.json"))
        .expect("secrets findings file")
        .path();
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("secret: generic-api-key"));
    assert!(
        text.contains(r#""file": "leaky.py""#),
        "stage prefix stripped: {text}"
    );
    assert!(text.contains(r#""fingerprint": "leaky.py:generic-api-key:2""#));
    Command::cargo_bin("ritual")
        .unwrap()
        .current_dir(tmp.path())
        .arg("findings")
        .assert()
        .code(1);

    // A human dismissing it unblocks the contract.
    let dismissed = text.replace(r#""action": "pending""#, r#""action": "dismissed""#);
    std::fs::write(&path, dismissed).unwrap();
    Command::cargo_bin("ritual")
        .unwrap()
        .current_dir(tmp.path())
        .arg("findings")
        .assert()
        .success();

    // Clean scan exits zero.
    Command::cargo_bin("ritual")
        .unwrap()
        .current_dir(tmp.path())
        .env("RITUAL_GITLEAKS_CMD", &fake)
        .env("FAKE_GITLEAKS_EXIT", "0")
        .arg("secrets")
        .assert()
        .success()
        .stdout(predicate::str::contains("no secrets found"));
}

#[test]
fn costs_rolls_up_per_stage_with_cache_rates() {
    let tmp = setup_project();
    std::fs::create_dir_all(tmp.path().join(".ritual/runs")).unwrap();
    let today = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ");
    for (name, stage, cost) in [
        ("20260711T000001Z-1-dual-review", "dual-review", 2.5),
        ("20260711T000002Z-2-dual-review", "dual-review", 1.5),
        ("20260711T000003Z-3-plan-review", "plan-review", 0.5),
    ] {
        std::fs::write(
            tmp.path().join(format!(".ritual/runs/{name}.meta.json")),
            format!(
                r#"{{"run_id":"{name}","stage":"{stage}","started_at":"{today}",
                    "total_cost_usd":{cost},
                    "usage":{{"input_tokens":100,"output_tokens":50,
                              "cache_read_input_tokens":900,"cache_creation_input_tokens":10}}}}"#
            ),
        )
        .unwrap();
    }

    let out = Command::cargo_bin("ritual")
        .unwrap()
        .current_dir(tmp.path())
        .args(["costs", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let v: serde_json::Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(v["all_time"][0]["stage"], "dual-review"); // biggest spend first
    assert_eq!(v["all_time"][0]["runs"], 2);
    assert_eq!(v["all_time"][0]["total_usd"], 4.0);
    assert_eq!(v["all_time"][0]["cache_read"], 1800);
    assert_eq!(v["today"][1]["stage"], "plan-review");

    // Styled output renders the gauge line when a budget is set.
    std::fs::write(
        tmp.path().join(".ritual/config.toml"),
        "budget_daily_usd = 10.0\n",
    )
    .unwrap();
    Command::cargo_bin("ritual")
        .unwrap()
        .current_dir(tmp.path())
        .arg("costs")
        .assert()
        .success()
        .stdout(predicate::str::contains("cache"))
        .stdout(predicate::str::contains("daily budget: $4.50 / $10.00"));
}

#[test]
fn lessons_distill_dispositions_and_refresh_before_dual_review() {
    let tmp = setup_project();
    std::fs::create_dir_all(tmp.path().join(".ritual/findings")).unwrap();
    std::fs::write(
        tmp.path()
            .join(".ritual/findings/20260710T000000Z-dual-review.json"),
        r#"{"stage":"dual-review","findings":[
            {"title":"style nit","file":"src/a.rs","line":1,"action":"dismissed"},
            {"title":"real leak","file":"src/b.rs","line":7,"action":"fixed"}
        ]}"#,
    )
    .unwrap();

    Command::cargo_bin("ritual")
        .unwrap()
        .current_dir(tmp.path())
        .args(["lessons", "--stdout"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Known noise"))
        .stdout(predicate::str::contains("style nit (src/a.rs:1)"))
        .stdout(predicate::str::contains("real leak (src/b.rs:7)"));
    assert!(tmp.path().join(".ritual/lessons.md").exists());

    // A dual-review run refreshes the file before spawning the agent.
    std::fs::remove_file(tmp.path().join(".ritual/lessons.md")).unwrap();
    Command::cargo_bin("ritual")
        .unwrap()
        .current_dir(tmp.path())
        .env("RITUAL_CLAUDE_CMD", fake_agent())
        .env("RITUAL_CODEX_CMD", fake_agent())
        .env("FAKE_AGENT_DELAY", "0")
        .env(
            "FAKE_AGENT_FINDINGS",
            ".ritual/findings/20260711T210000Z-dual-review.json",
        )
        .args(["run", "dual-review"])
        .assert()
        .success();
    assert!(
        tmp.path().join(".ritual/lessons.md").exists(),
        "dual-review must refresh the review memory"
    );
}

#[test]
fn run_plan_review_without_findings_needs_attention() {
    let tmp = setup_project();
    std::fs::create_dir_all(tmp.path().join(".ritual/features/main")).unwrap();
    std::fs::write(tmp.path().join(".ritual/features/main/plan.md"), "# plan").unwrap();

    // No FAKE_AGENT_FINDINGS: run succeeds but writes no findings file.
    Command::cargo_bin("ritual")
        .unwrap()
        .current_dir(tmp.path())
        .env("RITUAL_CLAUDE_CMD", fake_agent())
        .env("RITUAL_CODEX_CMD", fake_agent())
        .env("FAKE_AGENT_DELAY", "0")
        .args(["run", "plan-review"])
        .assert()
        .success()
        .stdout(predicate::str::contains("no findings file"));

    let state = std::fs::read_to_string(tmp.path().join(".ritual/state.json")).unwrap();
    assert!(state.contains(r#""status": "needs_attention""#));
}

#[test]
fn run_fails_cleanly_when_agent_fails() {
    let tmp = setup_project();
    std::fs::create_dir_all(tmp.path().join(".ritual/features/main")).unwrap();
    std::fs::write(tmp.path().join(".ritual/features/main/plan.md"), "# plan").unwrap();

    Command::cargo_bin("ritual")
        .unwrap()
        .current_dir(tmp.path())
        .env("RITUAL_CLAUDE_CMD", fake_agent())
        .env("RITUAL_CODEX_CMD", fake_agent())
        .env("FAKE_AGENT_DELAY", "0")
        .env("FAKE_AGENT_EXIT", "3")
        .args(["run", "plan-review"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("failed"));

    let state = std::fs::read_to_string(tmp.path().join(".ritual/state.json")).unwrap();
    assert!(state.contains(r#""status": "failed""#));
}

#[test]
fn status_json_is_machine_readable() {
    let tmp = setup_project();
    Command::cargo_bin("ritual")
        .unwrap()
        .current_dir(tmp.path())
        .args(["new", "JsonFeature"])
        .assert()
        .success();
    let out = Command::cargo_bin("ritual")
        .unwrap()
        .current_dir(tmp.path())
        .args(["status", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let v: serde_json::Value = serde_json::from_slice(&out).expect("valid JSON");
    assert_eq!(v["current_branch"], "main");
    assert_eq!(v["features"]["main"]["title"], "JsonFeature");
}

#[test]
fn complete_check_exits_nonzero_until_coverage_passes() {
    let tmp = setup_project();
    // Fresh project: coverage never ran, so `complete --check` is a CI red.
    Command::cargo_bin("ritual")
        .unwrap()
        .current_dir(tmp.path())
        .args(["complete", "--check"])
        .assert()
        .code(1)
        .stdout(predicate::str::contains("not complete"));
}

#[test]
fn reset_plan_dry_run_then_force_wipes_the_plan() {
    let tmp = setup_project();
    let root = tmp.path();
    let feat = root.join(".ritual/features/main");
    std::fs::create_dir_all(&feat).unwrap();
    std::fs::write(feat.join("plan.md"), "# Plan\n").unwrap();
    std::fs::write(
        root.join(".ritual/findings/20260101T000000Z-plan-review.json"),
        r#"{"stage":"plan-review","branch":"main","findings":[]}"#,
    )
    .unwrap();

    // Dry run: reports but changes nothing.
    Command::cargo_bin("ritual")
        .unwrap()
        .current_dir(root)
        .arg("reset-plan")
        .assert()
        .success()
        .stdout(predicate::str::contains("dry run"));
    assert!(feat.join("plan.md").exists(), "dry run kept plan.md");

    // --force: actually resets.
    Command::cargo_bin("ritual")
        .unwrap()
        .current_dir(root)
        .args(["reset-plan", "--force"])
        .assert()
        .success()
        .stdout(predicate::str::contains("reset plan"));
    assert!(!feat.join("plan.md").exists(), "plan.md deleted");
    assert!(
        !root
            .join(".ritual/findings/20260101T000000Z-plan-review.json")
            .exists()
    );
}

#[test]
fn complete_check_requires_a_deliverables_checklist_even_when_coverage_is_clean() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = setup_project();
    let root = tmp.path();
    // A plan with NO ## Deliverables section (the reward-hacking trap).
    let feat = root.join(".ritual/features/main");
    std::fs::create_dir_all(&feat).unwrap();
    std::fs::write(feat.join("plan.md"), "# Plan\n\n## Steps\n1. do it\n").unwrap();
    // A clean coverage report + green check.sh already on disk.
    std::fs::write(
        root.join(".ritual/findings/20260101T000000Z-coverage.json"),
        r#"{"stage":"coverage","satisfied":[],"findings":[]}"#,
    )
    .unwrap();
    std::fs::write(root.join("check.sh"), "#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(
        root.join("check.sh"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    // Deterministic backstop: no deliverables declared -> NOT complete (exit 1),
    // even though coverage is "clean" and the tree is green.
    Command::cargo_bin("ritual")
        .unwrap()
        .current_dir(root)
        .args(["complete", "--check"])
        .assert()
        .code(1)
        .stdout(predicate::str::contains("deliverables"));
}

fn complete_agent() -> String {
    format!(
        "{}/tests/fixtures/complete_agent.sh",
        env!("CARGO_MANIFEST_DIR")
    )
}

#[test]
fn complete_stops_when_the_coverage_judge_writes_no_report() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = setup_project();
    let root = tmp.path();
    std::fs::write(root.join("check.sh"), "#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(
        root.join("check.sh"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    let feat = root.join(".ritual/features/main");
    std::fs::create_dir_all(&feat).unwrap();
    std::fs::write(
        feat.join("plan.md"),
        "# Plan\n\n## Deliverables\n- [ ] D1: x - accept: y exists - route: media.txt\n",
    )
    .unwrap();
    // The judge writes no report -> the loop must NOT declare "clean"; exit 1.
    Command::cargo_bin("ritual")
        .unwrap()
        .current_dir(root)
        .env("RITUAL_CLAUDE_CMD", complete_agent())
        .env("RITUAL_CODEX_CMD", complete_agent())
        .env("COMPLETE_AGENT_NO_REPORT", "1")
        .args(["complete"])
        .assert()
        .code(1)
        .stdout(predicate::str::contains("no report"));
}

#[test]
fn complete_drives_the_deliverable_loop_to_done() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = setup_project();
    let root = tmp.path();
    // A green check.sh (the completeness gate re-checks the tree).
    std::fs::write(root.join("check.sh"), "#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(
        root.join("check.sh"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    // One code deliverable whose target file does not exist yet.
    let feat = root.join(".ritual/features/main");
    std::fs::create_dir_all(&feat).unwrap();
    std::fs::write(
        feat.join("plan.md"),
        "# Plan\n\n## Deliverables\n- [ ] D1: media file - accept: media.txt exists - route: media.txt\n",
    )
    .unwrap();

    // The loop: coverage flags D1 missing -> code-fix builds media.txt ->
    // coverage confirms -> Done. Exits 0.
    Command::cargo_bin("ritual")
        .unwrap()
        .current_dir(root)
        .env("RITUAL_CLAUDE_CMD", complete_agent())
        .env("RITUAL_CODEX_CMD", complete_agent())
        .args(["complete"])
        .assert()
        .success()
        .stdout(predicate::str::contains("all deliverables satisfied"));

    assert!(
        root.join("media.txt").exists(),
        "the fix agent built the file"
    );
    let plan = std::fs::read_to_string(feat.join("plan.md")).unwrap();
    assert!(plan.contains("- [x] D1"), "coverage ticked D1: {plan}");

    // And the CI gate now agrees.
    Command::cargo_bin("ritual")
        .unwrap()
        .current_dir(root)
        .args(["complete", "--check"])
        .assert()
        .success()
        .stdout(predicate::str::contains("complete"));
}

#[test]
fn findings_exit_code_contract() {
    let tmp = setup_project();
    // Non-blocking finding: exit 0.
    std::fs::write(
        tmp.path()
            .join(".ritual/findings/20260711T000000Z-dual-review.json"),
        r#"{"ritual_findings":1,"stage":"dual-review","findings":[
            {"id":1,"severity":"major","title":"meh","verdict":"confirmed"}]}"#,
    )
    .unwrap();
    Command::cargo_bin("ritual")
        .unwrap()
        .current_dir(tmp.path())
        .arg("findings")
        .assert()
        .success();

    // Confirmed critical: exit 1.
    std::fs::write(
        tmp.path()
            .join(".ritual/findings/20260711T000001Z-dual-review.json"),
        r#"{"ritual_findings":1,"stage":"dual-review","findings":[
            {"id":1,"severity":"critical","title":"bad","verdict":"confirmed"}]}"#,
    )
    .unwrap();
    Command::cargo_bin("ritual")
        .unwrap()
        .current_dir(tmp.path())
        .arg("findings")
        .assert()
        .code(1);

    // The same critical marked dismissed: unblocked (exit 0), hidden by
    // default, visible with --all.
    std::fs::write(
        tmp.path()
            .join(".ritual/findings/20260711T000001Z-dual-review.json"),
        r#"{"ritual_findings":1,"stage":"dual-review","findings":[
            {"id":1,"severity":"critical","title":"bad","verdict":"confirmed","action":"dismissed"}]}"#,
    )
    .unwrap();
    Command::cargo_bin("ritual")
        .unwrap()
        .current_dir(tmp.path())
        .arg("findings")
        .assert()
        .success()
        .stdout(predicate::str::contains("resolved finding(s) hidden"));
    Command::cargo_bin("ritual")
        .unwrap()
        .current_dir(tmp.path())
        .args(["findings", "--all"])
        .assert()
        .success()
        .stdout(predicate::str::contains("bad"));
}

#[test]
fn history_json_is_array() {
    let tmp = setup_project();
    std::fs::write(
        tmp.path().join(".ritual/runs/20260711T000000Z-x.meta.json"),
        r#"{"run_id":"r1","stage":"plan-review","ok":true}"#,
    )
    .unwrap();
    let out = Command::cargo_bin("ritual")
        .unwrap()
        .current_dir(tmp.path())
        .args(["history", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let v: serde_json::Value = serde_json::from_slice(&out).expect("valid JSON");
    assert_eq!(v[0]["run_id"], "r1");
}

#[test]
fn bad_keybinding_config_errors_cleanly() {
    let tmp = setup_project();
    std::fs::write(
        tmp.path().join(".ritual/config.toml"),
        "[keys]\n\"summon-shoggoth\" = \"s\"\n",
    )
    .unwrap();
    Command::cargo_bin("ritual")
        .unwrap()
        .current_dir(tmp.path())
        .arg("status")
        .assert()
        .failure()
        .stderr(predicate::str::contains("unknown action"));
}

#[test]
fn budget_ceiling_blocks_runs_unless_forced() {
    let tmp = setup_project();
    std::fs::create_dir_all(tmp.path().join(".ritual/features/main")).unwrap();
    std::fs::write(tmp.path().join(".ritual/features/main/plan.md"), "# plan").unwrap();
    std::fs::write(
        tmp.path().join(".ritual/config.toml"),
        "budget_daily_usd = 1.0\n",
    )
    .unwrap();
    // A run from today that already spent past the ceiling.
    let now = chrono::Utc::now().to_rfc3339();
    std::fs::write(
        tmp.path().join(".ritual/runs/20260711T000000Z-x.meta.json"),
        format!(r#"{{"run_id":"r","stage":"plan-review","ok":true,"total_cost_usd":2.5,"started_at":"{now}"}}"#),
    )
    .unwrap();

    Command::cargo_bin("ritual")
        .unwrap()
        .current_dir(tmp.path())
        .env("RITUAL_CLAUDE_CMD", fake_agent())
        .env("RITUAL_CODEX_CMD", fake_agent())
        .env("FAKE_AGENT_DELAY", "0")
        .args(["run", "plan-review"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("daily budget reached"));

    // --force overrides.
    Command::cargo_bin("ritual")
        .unwrap()
        .current_dir(tmp.path())
        .env("RITUAL_CLAUDE_CMD", fake_agent())
        .env("RITUAL_CODEX_CMD", fake_agent())
        .env("FAKE_AGENT_DELAY", "0")
        .env(
            "FAKE_AGENT_FINDINGS",
            ".ritual/findings/20260711T210000Z-plan-review.json",
        )
        .args(["run", "plan-review", "--force"])
        .assert()
        .success();
}

#[test]
fn report_generates_markdown_with_redaction() {
    let tmp = setup_project();
    Command::cargo_bin("ritual")
        .unwrap()
        .current_dir(tmp.path())
        .args(["new", "RepFeature"])
        .assert()
        .success();
    std::fs::write(
        tmp.path().join(".ritual/features/main/spec.md"),
        "goal with token = \"supersecretvalue123\"",
    )
    .unwrap();
    let out = Command::cargo_bin("ritual")
        .unwrap()
        .current_dir(tmp.path())
        .arg("report")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let out = String::from_utf8_lossy(&out);
    let path = out
        .trim()
        .strip_prefix("report: ")
        .expect("report path printed");
    let text = std::fs::read_to_string(tmp.path().join(path).canonicalize().unwrap_or(path.into()))
        .or_else(|_| std::fs::read_to_string(path))
        .unwrap();
    assert!(text.contains("RepFeature"));
    assert!(text.contains("## Pipeline"));
    assert!(
        !text.contains("supersecretvalue123"),
        "secret leaked into report"
    );
}

#[test]
fn ci_mode_emits_junit_and_fails_on_blocking_findings() {
    let tmp = setup_project();
    std::fs::create_dir_all(tmp.path().join(".ritual/features/main")).unwrap();
    std::fs::write(tmp.path().join(".ritual/features/main/plan.md"), "# plan").unwrap();

    // Canned finding is confirmed critical -> JUnit failure -> nonzero exit.
    Command::cargo_bin("ritual")
        .unwrap()
        .current_dir(tmp.path())
        .env("RITUAL_CLAUDE_CMD", fake_agent())
        .env("RITUAL_CODEX_CMD", fake_agent())
        .env("FAKE_AGENT_DELAY", "0")
        .env(
            "FAKE_AGENT_FINDINGS",
            ".ritual/findings/20260711T220000Z-plan-review.json",
        )
        .args(["run", "plan-review", "--ci"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("blocking finding"));

    let ci_files: Vec<_> = std::fs::read_dir(tmp.path().join(".ritual/ci"))
        .unwrap()
        .filter_map(|e| e.ok())
        .collect();
    assert_eq!(ci_files.len(), 1);
    let xml = std::fs::read_to_string(ci_files[0].path()).unwrap();
    assert!(xml.contains("<testsuite"));
    assert!(xml.contains(r#"failures="1""#));
    assert!(xml.contains("Canned test finding"));

    // Chain + repro landed in the meta.
    let meta_file = std::fs::read_dir(tmp.path().join(".ritual/runs"))
        .unwrap()
        .filter_map(|e| e.ok())
        .find(|e| e.file_name().to_string_lossy().ends_with(".meta.json"))
        .unwrap();
    let meta = std::fs::read_to_string(meta_file.path()).unwrap();
    assert!(meta.contains(r#""chain""#));
    assert!(meta.contains(r#""repro""#));

    // And verify-log confirms the chain.
    Command::cargo_bin("ritual")
        .unwrap()
        .current_dir(tmp.path())
        .arg("verify-log")
        .assert()
        .success()
        .stdout(predicate::str::contains("chain intact"));
}

#[test]
fn worktree_feature_shares_state_and_resolves_dirs() {
    let tmp = setup_project();
    // Commit so a worktree can be created.
    std::process::Command::new("git")
        .args(["add", "-A"])
        .current_dir(tmp.path())
        .status()
        .unwrap();
    std::process::Command::new("git")
        .args([
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "commit",
            "-qm",
            "init",
        ])
        .current_dir(tmp.path())
        .status()
        .unwrap();

    Command::cargo_bin("ritual")
        .unwrap()
        .current_dir(tmp.path())
        .args(["new", "Parallel", "Thing", "--worktree", "feat/parallel"])
        .assert()
        .success()
        .stdout(predicate::str::contains("worktree:"));

    // The worktree checkout exists...
    let wt = tmp.path().parent().unwrap().join(format!(
        "{}-feat-parallel",
        tmp.path().file_name().unwrap().to_string_lossy()
    ));
    assert!(wt.is_dir(), "worktree dir missing: {}", wt.display());

    // ...and shares the MAIN repo's .ritual: committed files (invariants.md)
    // materialize a .ritual/ inside the checkout, but discovery still binds
    // to the main root; status from inside the worktree sees the same state.
    assert!(wt.join(".ritual/invariants.md").exists());
    assert!(!wt.join(".ritual/state.json").exists());
    let out = Command::cargo_bin("ritual")
        .unwrap()
        .current_dir(&wt)
        .args(["status", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let v: serde_json::Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(v["features"]["feat-parallel"]["title"], "Parallel Thing");
    assert_eq!(v["current_branch"], "feat/parallel");

    // Cleanup the worktree dir (outside the tempdir root).
    std::process::Command::new("git")
        .args(["worktree", "remove", "--force"])
        .arg(&wt)
        .current_dir(tmp.path())
        .status()
        .unwrap();
}

#[test]
fn pr_comment_posts_redacted_summary_via_fake_gh() {
    let tmp = setup_project();
    let fake_gh = format!("{}/tests/fake_gh.sh", env!("CARGO_MANIFEST_DIR"));

    // No findings yet: clean error.
    Command::cargo_bin("ritual")
        .unwrap()
        .current_dir(tmp.path())
        .env("RITUAL_GH_CMD", &fake_gh)
        .arg("pr-comment")
        .assert()
        .failure()
        .stderr(predicate::str::contains("no dual-review findings"));

    // A findings file with an open critical (with a seeded secret), a
    // dismissed finding, and an unconfirmed one.
    std::fs::write(
        tmp.path()
            .join(".ritual/findings/20260712T190000Z-dual-review.json"),
        r#"{"ritual_findings":1,"stage":"dual-review","branch":"main",
            "generated_at":"2026-07-12T19:00:00Z",
            "source_models":{"claude":"c","codex":"x"},
            "findings":[
              {"id":1,"severity":"critical","title":"token leak","file":"src/a.rs","line":3,
               "scenario":"api_key = \"hunter2hunter2\" in logs","sources":["claude","codex"],
               "verdict":"confirmed","action":"pending"},
              {"id":2,"severity":"major","title":"already dismissed","verdict":"confirmed","action":"dismissed"}
            ]}"#,
    )
    .unwrap();

    Command::cargo_bin("ritual")
        .unwrap()
        .current_dir(tmp.path())
        .env("RITUAL_GH_CMD", &fake_gh)
        .env("FAKE_GH_LOG_DIR", tmp.path())
        .args(["pr-comment", "--inline"])
        .assert()
        .success()
        .stdout(predicate::str::contains("posted summary comment on #7"))
        .stdout(predicate::str::contains("inline:"));

    // The fake gh recorded the body: open finding present, dismissed one
    // absent, and the assignment-shaped secret redacted.
    let body = std::fs::read_to_string(tmp.path().join("gh-stdin.log")).unwrap();
    assert!(body.contains("token leak"));
    assert!(!body.contains("already dismissed"));
    assert!(body.contains("[REDACTED"), "secret survived: {body}");
    assert!(!body.contains("hunter2hunter2"));
    // Inline call went to the pulls comments API with the head commit.
    let args = std::fs::read_to_string(tmp.path().join("gh-args.log")).unwrap();
    assert!(args.contains("pulls/7/comments"));
    assert!(args.contains("commit_id=abc123def456"));
}

#[test]
fn doctor_passes_healthy_and_fails_without_check_sh() {
    let tmp = setup_project();
    // Skills installed into a fake home so the drift check passes.
    let fake_home = tmp.path().join("claude-home");
    Command::cargo_bin("ritual")
        .unwrap()
        .current_dir(tmp.path())
        .env("RITUAL_CLAUDE_HOME", &fake_home)
        .args(["init", "--skills"])
        .assert()
        .success();
    std::fs::write(
        fake_home.join("settings.json"),
        r#"{"hooks":{"PostToolUse":[{"hooks":[{"command":"~/.claude/hooks/check-on-edit.sh"}]}]}}"#,
    )
    .unwrap();

    // Healthy project: exit 0 (fake agent answers auth/mcp probes).
    Command::cargo_bin("ritual")
        .unwrap()
        .current_dir(tmp.path())
        .env("RITUAL_CLAUDE_HOME", &fake_home)
        .env("RITUAL_CLAUDE_CMD", fake_agent())
        .env("RITUAL_CODEX_CMD", fake_agent())
        .arg("doctor")
        .assert()
        .success()
        .stdout(predicate::str::contains("0 failure(s)"));

    // Remove check.sh: hard failure, exit 1.
    std::fs::remove_file(tmp.path().join("check.sh")).unwrap();
    Command::cargo_bin("ritual")
        .unwrap()
        .current_dir(tmp.path())
        .env("RITUAL_CLAUDE_HOME", &fake_home)
        .env("RITUAL_CLAUDE_CMD", fake_agent())
        .env("RITUAL_CODEX_CMD", fake_agent())
        .arg("doctor")
        .assert()
        .code(1)
        .stdout(predicate::str::contains("missing"));
}

#[test]
fn ps_and_attach_follow_and_kill_live_runs() {
    let tmp = setup_project();
    std::fs::create_dir_all(tmp.path().join(".ritual/features/main")).unwrap();
    std::fs::write(tmp.path().join(".ritual/features/main/plan.md"), "# plan").unwrap();

    // Slow run in the background so ps/attach can catch it live.
    let mut launcher = std::process::Command::new(assert_cmd::cargo::cargo_bin("ritual"))
        .current_dir(tmp.path())
        .env("RITUAL_CLAUDE_CMD", fake_agent())
        .env("RITUAL_CODEX_CMD", fake_agent())
        .env("FAKE_AGENT_DELAY", "0.4")
        .env(
            "FAKE_AGENT_FINDINGS",
            ".ritual/findings/20260712T180000Z-plan-review.json",
        )
        .args(["run", "plan-review"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(1500));

    // ps shows the live run with its stage.
    let out = Command::cargo_bin("ritual")
        .unwrap()
        .current_dir(tmp.path())
        .arg("ps")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let ps_out = String::from_utf8_lossy(&out);
    assert!(
        ps_out.contains("plan-review"),
        "ps missed the run: {ps_out}"
    );
    let run_id = ps_out
        .lines()
        .find(|l| l.contains("plan-review"))
        .and_then(|l| l.split_whitespace().next())
        .expect("run id in ps output")
        .to_string();

    // attach follows it to completion and exits 0.
    Command::cargo_bin("ritual")
        .unwrap()
        .current_dir(tmp.path())
        .arg("attach")
        .arg(&run_id)
        .assert()
        .success()
        .stdout(predicate::str::contains("attached to"));
    launcher.wait().unwrap();

    // attach on the now-finished run prints its summary, still exit 0.
    Command::cargo_bin("ritual")
        .unwrap()
        .current_dir(tmp.path())
        .arg("attach")
        .arg(&run_id)
        .assert()
        .success()
        .stdout(predicate::str::contains("plan-review"));

    // Unknown id: clean error.
    Command::cargo_bin("ritual")
        .unwrap()
        .current_dir(tmp.path())
        .args(["attach", "no-such-run"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("no such run"));

    // --kill: start another slow run and terminate it.
    let mut launcher2 = std::process::Command::new(assert_cmd::cargo::cargo_bin("ritual"))
        .current_dir(tmp.path())
        .env("RITUAL_CLAUDE_CMD", fake_agent())
        .env("RITUAL_CODEX_CMD", fake_agent())
        .env("FAKE_AGENT_DELAY", "0.5")
        .args(["run", "plan-review", "--force"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(1500));
    let out = Command::cargo_bin("ritual")
        .unwrap()
        .current_dir(tmp.path())
        .arg("ps")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let run2 = String::from_utf8_lossy(&out)
        .lines()
        .find(|l| l.contains("plan-review"))
        .and_then(|l| l.split_whitespace().next().map(str::to_string))
        .expect("second live run");
    Command::cargo_bin("ritual")
        .unwrap()
        .current_dir(tmp.path())
        .args(["attach", &run2, "--kill"])
        .assert()
        .success()
        .stdout(predicate::str::contains("killed"));
    let _ = launcher2.wait();
}

#[test]
fn detached_run_survives_the_launcher() {
    let tmp = setup_project();
    std::fs::create_dir_all(tmp.path().join(".ritual/features/main")).unwrap();
    std::fs::write(tmp.path().join(".ritual/features/main/plan.md"), "# plan").unwrap();

    // Slow fake agent: the launcher gets killed mid-run; the daemon must
    // finish alone and write the meta.
    let mut launcher = std::process::Command::new(assert_cmd::cargo::cargo_bin("ritual"))
        .current_dir(tmp.path())
        .env("RITUAL_CLAUDE_CMD", fake_agent())
        .env("RITUAL_CODEX_CMD", fake_agent())
        .env("FAKE_AGENT_DELAY", "0.3")
        .env(
            "FAKE_AGENT_FINDINGS",
            ".ritual/findings/20260712T000000Z-plan-review.json",
        )
        .args(["run", "plan-review"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();

    // Give it time to daemonize, then kill the launcher hard.
    std::thread::sleep(std::time::Duration::from_millis(1200));
    launcher.kill().unwrap();
    launcher.wait().unwrap();

    // The daemon keeps going: meta.json appears within a few seconds.
    let runs = tmp.path().join(".ritual/runs");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    let meta_written = loop {
        let found = std::fs::read_dir(&runs)
            .map(|rd| {
                rd.filter_map(|e| e.ok())
                    .any(|e| e.file_name().to_string_lossy().ends_with(".meta.json"))
            })
            .unwrap_or(false);
        if found {
            break true;
        }
        if std::time::Instant::now() > deadline {
            break false;
        }
        std::thread::sleep(std::time::Duration::from_millis(300));
    };
    assert!(meta_written, "daemon did not survive launcher death");

    // Sidecars cleaned up after completion.
    std::thread::sleep(std::time::Duration::from_millis(500));
    let leftovers: Vec<_> = std::fs::read_dir(&runs)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".status") || n.ends_with(".request.json"))
        .collect();
    assert!(leftovers.is_empty(), "sidecars left behind: {leftovers:?}");
}

#[test]
fn run_unknown_stage_is_an_error() {
    let tmp = setup_project();
    Command::cargo_bin("ritual")
        .unwrap()
        .current_dir(tmp.path())
        .args(["run", "summon-shoggoth"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("unknown stage"));
}

/// Run one plan-review to completion with the fake agent; leaves a chained
/// meta + raw archive + one findings file. Returns the project tempdir.
fn project_with_one_run() -> tempfile::TempDir {
    let tmp = setup_project();
    std::fs::create_dir_all(tmp.path().join(".ritual/features/main")).unwrap();
    std::fs::write(tmp.path().join(".ritual/features/main/plan.md"), "# plan").unwrap();
    Command::cargo_bin("ritual")
        .unwrap()
        .current_dir(tmp.path())
        .env("RITUAL_CLAUDE_CMD", fake_agent())
        .env("RITUAL_CODEX_CMD", fake_agent())
        .env("FAKE_AGENT_DELAY", "0")
        .env(
            "FAKE_AGENT_FINDINGS",
            ".ritual/findings/20260712T120000Z-plan-review.json",
        )
        .args(["run", "plan-review"])
        .assert()
        .success();
    tmp
}

#[test]
fn chat_edits_spec_and_marks_stage_done() {
    let tmp = setup_project();
    Command::cargo_bin("ritual")
        .unwrap()
        .current_dir(tmp.path())
        .args(["new", "Chatty", "Feature"])
        .assert()
        .success();
    let spec = tmp.path().join(".ritual/features/main/spec.md");
    let before = std::fs::read_to_string(&spec).unwrap(); // template, no real content yet

    Command::cargo_bin("ritual")
        .unwrap()
        .current_dir(tmp.path())
        .env("RITUAL_CLAUDE_CMD", fake_agent())
        .env("RITUAL_CODEX_CMD", fake_agent())
        .env("FAKE_AGENT_DELAY", "0")
        .env("FAKE_AGENT_SPEC_EDIT", ".ritual/features/main/spec.md")
        .args(["chat", "add a retry invariant", "--section", "Goal"])
        .assert()
        .success()
        .stdout(predicate::str::contains("spec updated"));

    let after = std::fs::read_to_string(&spec).unwrap();
    assert_ne!(before, after, "the chat should have edited spec.md");
    assert!(after.contains("A concrete change applied by the fake agent."));

    // The spec stage is now done; a run was recorded.
    let state = std::fs::read_to_string(tmp.path().join(".ritual/state.json")).unwrap();
    assert!(state.contains(r#""spec""#));
    assert!(state.contains(r#""status": "done""#));
    let runs: Vec<_> = std::fs::read_dir(tmp.path().join(".ritual/runs"))
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert!(
        runs.iter()
            .any(|f| f.contains("spec-chat") && f.ends_with(".meta.json")),
        "spec-chat meta missing: {runs:?}"
    );
}

#[test]
fn chat_can_target_the_plan_and_creates_it() {
    let tmp = setup_project();
    Command::cargo_bin("ritual")
        .unwrap()
        .current_dir(tmp.path())
        .args(["new", "Planny"])
        .assert()
        .success();
    let plan = tmp.path().join(".ritual/features/main/plan.md");
    assert!(!plan.exists());

    Command::cargo_bin("ritual")
        .unwrap()
        .current_dir(tmp.path())
        .env("RITUAL_CLAUDE_CMD", fake_agent())
        .env("RITUAL_CODEX_CMD", fake_agent())
        .env("FAKE_AGENT_DELAY", "0")
        .env("FAKE_AGENT_SPEC_EDIT", ".ritual/features/main/plan.md")
        .args(["chat", "outline the steps", "--plan"])
        .assert()
        .success()
        .stdout(predicate::str::contains("plan updated"));

    assert!(plan.exists());
    assert!(
        std::fs::read_to_string(&plan)
            .unwrap()
            .contains("A concrete change")
    );
}

#[test]
fn chat_reports_no_change_when_the_agent_edits_nothing() {
    let tmp = setup_project();
    Command::cargo_bin("ritual")
        .unwrap()
        .current_dir(tmp.path())
        .args(["new", "Idle"])
        .assert()
        .success();
    // No FAKE_AGENT_SPEC_EDIT: the agent streams but touches no file.
    Command::cargo_bin("ritual")
        .unwrap()
        .current_dir(tmp.path())
        .env("RITUAL_CLAUDE_CMD", fake_agent())
        .env("RITUAL_CODEX_CMD", fake_agent())
        .env("FAKE_AGENT_DELAY", "0")
        .args(["chat", "do nothing useful"])
        .assert()
        .success()
        .stdout(predicate::str::contains("no change to spec"));

    // Spec stays pending (not marked done).
    let state = std::fs::read_to_string(tmp.path().join(".ritual/state.json")).unwrap();
    assert!(!state.contains(r#""status": "done""#));
}

#[test]
fn rapid_back_to_back_runs_do_not_clobber_each_other() {
    // Regression: second-precision run ids used to collide when two runs
    // landed in the same second, overwriting each other's archive/meta/status.
    let tmp = setup_project();
    std::fs::create_dir_all(tmp.path().join(".ritual/features/main")).unwrap();
    std::fs::write(tmp.path().join(".ritual/features/main/plan.md"), "# plan").unwrap();

    for _ in 0..3 {
        Command::cargo_bin("ritual")
            .unwrap()
            .current_dir(tmp.path())
            .env("RITUAL_CLAUDE_CMD", fake_agent())
            .env("RITUAL_CODEX_CMD", fake_agent())
            .env("FAKE_AGENT_DELAY", "0")
            .args(["run", "plan-review", "--force"])
            .assert()
            .success();
    }

    let entries: Vec<String> = std::fs::read_dir(tmp.path().join(".ritual/runs"))
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    let metas = entries.iter().filter(|f| f.ends_with(".meta.json")).count();
    let archives = entries.iter().filter(|f| f.ends_with(".jsonl")).count();
    assert_eq!(metas, 3, "expected 3 distinct metas, got: {entries:?}");
    assert_eq!(
        archives, 3,
        "expected 3 distinct archives, got: {entries:?}"
    );

    // And the tamper-evident chain still verifies across all three.
    Command::cargo_bin("ritual")
        .unwrap()
        .current_dir(tmp.path())
        .arg("verify-log")
        .assert()
        .success()
        .stdout(predicate::str::contains("3 chained run(s) verified"));
}

#[test]
fn export_emits_valid_otlp_spans_after_a_run() {
    let tmp = project_with_one_run();
    let out = Command::cargo_bin("ritual")
        .unwrap()
        .current_dir(tmp.path())
        .arg("export")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    // stdout is OTLP-JSON lines; the last line must parse and describe the run.
    let line = String::from_utf8_lossy(&out);
    let line = line.lines().next().expect("at least one span");
    let v: serde_json::Value = serde_json::from_str(line).expect("valid OTLP JSON");
    let span = &v["resourceSpans"][0]["scopeSpans"][0]["spans"][0];
    assert_eq!(span["name"], "ritual:plan-review");
    assert_eq!(span["status"]["code"], 1);
}

#[test]
fn export_to_file_writes_spans() {
    let tmp = project_with_one_run();
    let out_path = tmp.path().join("spans.jsonl");
    Command::cargo_bin("ritual")
        .unwrap()
        .current_dir(tmp.path())
        .args(["export", "--out"])
        .arg(&out_path)
        .assert()
        .success()
        .stderr(predicate::str::contains("span(s) exported"));
    let text = std::fs::read_to_string(&out_path).unwrap();
    assert!(text.contains("ritual:plan-review"));
}

#[test]
fn bench_golden_scores_recall_and_cost_per_hit() {
    let tmp = setup_project();
    std::fs::create_dir_all(tmp.path().join(".ritual/features/main")).unwrap();
    std::fs::write(tmp.path().join(".ritual/features/main/plan.md"), "# plan").unwrap();
    // The fake agent's canned finding title, as the golden expectation.
    std::fs::write(tmp.path().join("golden.json"), r#"["Canned test finding"]"#).unwrap();

    Command::cargo_bin("ritual")
        .unwrap()
        .current_dir(tmp.path())
        .env("RITUAL_CLAUDE_CMD", fake_agent())
        .env("RITUAL_CODEX_CMD", fake_agent())
        .env("FAKE_AGENT_DELAY", "0")
        .env(
            "FAKE_AGENT_FINDINGS",
            ".ritual/findings/20260711T010000Z-plan-review.json",
        )
        .args([
            "bench",
            "plan-review",
            "--runs",
            "1",
            "--golden",
            "golden.json",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("golden recall 100%"))
        .stdout(predicate::str::contains("cost per golden hit: $"));
}

#[test]
fn export_audit_trail_chains_verifiably_from_the_cli() {
    let tmp = setup_project();
    let runs = tmp.path().join(".ritual/runs");
    std::fs::create_dir_all(&runs).unwrap();
    // Zero runs: sane no-op.
    Command::cargo_bin("ritual")
        .unwrap()
        .current_dir(tmp.path())
        .args(["export", "--audit-trail"])
        .assert()
        .success()
        .stderr(predicate::str::contains("0 audit record(s)"));

    for i in 1..=3 {
        std::fs::write(
            runs.join(format!("20260711T00000{i}Z-r.meta.json")),
            format!(
                r#"{{"run_id":"r{i}","stage":"plan-review","agent":"claude","ok":true,
                    "started_at":"2026-07-11T00:00:0{i}Z"}}"#
            ),
        )
        .unwrap();
    }
    let out = Command::cargo_bin("ritual")
        .unwrap()
        .current_dir(tmp.path())
        .args(["export", "--audit-trail"])
        .assert()
        .success()
        .stderr(predicate::str::contains("3 audit record(s)"))
        .get_output()
        .stdout
        .clone();
    // Independently re-verify the JCS/SHA-256 chain over the emitted lines.
    let lines: Vec<&str> = std::str::from_utf8(&out)
        .unwrap()
        .lines()
        .filter(|l| !l.trim().is_empty())
        .collect();
    assert_eq!(lines.len(), 3);
    for (i, line) in lines.iter().enumerate() {
        let rec: serde_json::Value = serde_json::from_str(line).unwrap();
        if i == 0 {
            assert!(rec["prev_hash"].is_null());
        } else {
            let expect = ritual::provenance::sha256_hex(lines[i - 1].as_bytes());
            assert_eq!(rec["prev_hash"].as_str().unwrap(), expect, "link {i}");
        }
        assert_eq!(rec["trust_level"], "L2");
    }
}

#[test]
fn pr_comment_inline_posts_anchored_review_comments() {
    let tmp = setup_project();
    let fake_gh = format!("{}/tests/fake_gh.sh", env!("CARGO_MANIFEST_DIR"));
    std::fs::create_dir_all(tmp.path().join(".ritual/findings")).unwrap();
    std::fs::write(
        tmp.path()
            .join(".ritual/findings/20260712T000000Z-dual-review.json"),
        r#"{"stage":"dual-review","branch":"main","findings":[
            {"title":"anchored bug","severity":"major","verdict":"confirmed",
             "file":"src/a.rs","line":42,"scenario":"s","snippet":"let x = y;",
             "sources":["claude","codex"],"action":"pending"},
            {"title":"no location","severity":"minor","verdict":"confirmed"}]}"#,
    )
    .unwrap();

    Command::cargo_bin("ritual")
        .unwrap()
        .current_dir(tmp.path())
        .env("RITUAL_GH_CMD", &fake_gh)
        .env("FAKE_GH_LOG_DIR", tmp.path())
        .args(["pr-comment", "7", "--inline"])
        .assert()
        .success()
        .stdout(predicate::str::contains("inline: 1 posted, 0 failed"));

    let args = std::fs::read_to_string(tmp.path().join("gh-args.log")).unwrap();
    assert!(args.contains("pulls/7/comments"), "{args}");
    assert!(args.contains("path=src/a.rs"));
    assert!(args.contains("line=42"));
    assert!(args.contains("commit_id=abc123def456"));
    // The snippet rides in the comment body as fenced evidence.
    assert!(args.contains("let x = y;"));
}

#[test]
fn mutants_exit_code_arms_from_the_cli() {
    let tmp = setup_project();
    let fake = format!("{}/tests/fake_mutants.sh", env!("CARGO_MANIFEST_DIR"));
    std::process::Command::new("git")
        .args(["add", "-A"])
        .current_dir(tmp.path())
        .status()
        .unwrap();
    std::process::Command::new("git")
        .args([
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "commit",
            "-qm",
            "init",
        ])
        .current_dir(tmp.path())
        .status()
        .unwrap();
    std::fs::write(
        tmp.path().join("Cargo.toml"),
        "[package]\nname=\"x\"\nversion=\"0.9.0\"\n",
    )
    .unwrap();

    // Exit 3 (timeouts occurred) still parses outcomes and reports.
    Command::cargo_bin("ritual")
        .unwrap()
        .current_dir(tmp.path())
        .env("RITUAL_MUTANTS_CMD", &fake)
        .env("FAKE_MUTANTS_EXIT", "3")
        .arg("mutants")
        .assert()
        .success()
        .stdout(predicate::str::contains("1 caught, 1 missed"));

    // Exit 1 = the tool rejected its arguments: a clean, actionable error.
    Command::cargo_bin("ritual")
        .unwrap()
        .current_dir(tmp.path())
        .env("RITUAL_MUTANTS_CMD", &fake)
        .env("FAKE_MUTANTS_EXIT", "1")
        .arg("mutants")
        .assert()
        .failure()
        .stderr(predicate::str::contains("rejected its arguments"));
}

#[test]
fn lessons_writes_the_file_when_not_streaming() {
    let tmp = setup_project();
    std::fs::create_dir_all(tmp.path().join(".ritual/findings")).unwrap();
    std::fs::write(
        tmp.path()
            .join(".ritual/findings/20260710T000000Z-dual-review.json"),
        r#"{"stage":"dual-review","findings":[{"title":"n","action":"dismissed"}]}"#,
    )
    .unwrap();
    Command::cargo_bin("ritual")
        .unwrap()
        .current_dir(tmp.path())
        .arg("lessons")
        .assert()
        .success()
        .stdout(predicate::str::contains("lessons →"));
    assert!(tmp.path().join(".ritual/lessons.md").exists());
}

#[test]
fn bench_runs_headless_stage_and_prints_scorecard() {
    let tmp = setup_project();
    std::fs::create_dir_all(tmp.path().join(".ritual/features/main")).unwrap();
    std::fs::write(tmp.path().join(".ritual/features/main/plan.md"), "# plan").unwrap();

    Command::cargo_bin("ritual")
        .unwrap()
        .current_dir(tmp.path())
        .env("RITUAL_CLAUDE_CMD", fake_agent())
        .env("RITUAL_CODEX_CMD", fake_agent())
        .env("FAKE_AGENT_DELAY", "0")
        .args(["bench", "plan-review", "--runs", "2"])
        .assert()
        .success()
        .stdout(predicate::str::contains("bench: plan-review × 2 run(s)"))
        .stdout(predicate::str::contains("ok-rate 100%"));
}

#[test]
fn bench_rejects_interactive_stages() {
    let tmp = setup_project();
    Command::cargo_bin("ritual")
        .unwrap()
        .current_dir(tmp.path())
        .env("RITUAL_CLAUDE_CMD", fake_agent())
        .env("RITUAL_CODEX_CMD", fake_agent())
        .env("FAKE_AGENT_DELAY", "0")
        .args(["bench", "spec", "--runs", "1"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("only supports headless"));
}

#[test]
fn repro_prints_recorded_bundle_and_env_comparison() {
    let tmp = project_with_one_run();
    // Discover the run id from history --json.
    let out = Command::cargo_bin("ritual")
        .unwrap()
        .current_dir(tmp.path())
        .args(["history", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let hist: serde_json::Value = serde_json::from_slice(&out).unwrap();
    let run_id = hist[0]["run_id"].as_str().expect("run_id present");

    Command::cargo_bin("ritual")
        .unwrap()
        .current_dir(tmp.path())
        .env("RITUAL_CLAUDE_CMD", fake_agent())
        .env("RITUAL_CODEX_CMD", fake_agent())
        .args(["repro", run_id])
        .assert()
        .success()
        // The recorded bundle is pretty-printed (always has this key)...
        .stdout(predicate::str::contains("git_commit"))
        // ...followed by an environment comparison verdict.
        .stdout(predicate::str::contains("environment"));
}

#[test]
fn repro_unknown_run_is_an_error() {
    let tmp = setup_project();
    Command::cargo_bin("ritual")
        .unwrap()
        .current_dir(tmp.path())
        .args(["repro", "no-such-run"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("no run"));
}

#[test]
fn clean_prunes_with_checkpoint_and_chain_stays_verifiable() {
    let tmp = setup_project();
    std::fs::create_dir_all(tmp.path().join(".ritual/features/main")).unwrap();
    std::fs::write(tmp.path().join(".ritual/features/main/plan.md"), "# plan").unwrap();
    let runs = tmp.path().join(".ritual/runs");
    std::fs::create_dir_all(&runs).unwrap();

    // Seed three AGED chained runs directly (real fake-agent runs are always
    // today-dated and today-protection would keep them all).
    let mut prev = ritual::provenance::GENESIS.to_string();
    for i in 1..=3 {
        let id = format!("20260701T00000{i}Z-old");
        let archive = runs.join(format!("{id}.jsonl"));
        std::fs::write(&archive, format!("line-{i}\n")).unwrap();
        let mut meta = ritual::history::RunMeta {
            run_id: id.clone(),
            stage: "plan-review".into(),
            ok: true,
            started_at: Some(chrono::Utc::now() - chrono::Duration::days(3)),
            ..Default::default()
        };
        let chain =
            ritual::provenance::compute_link(&prev, &std::fs::read(&archive).unwrap(), &meta)
                .unwrap();
        prev = chain.this.clone();
        meta.chain = Some(chain);
        std::fs::write(
            runs.join(format!("{id}.meta.json")),
            serde_json::to_string(&meta).unwrap(),
        )
        .unwrap();
    }

    // Prune down to 1: two chained runs go, a checkpoint covers them.
    Command::cargo_bin("ritual")
        .unwrap()
        .current_dir(tmp.path())
        .args(["clean", "--keep", "1"])
        .assert()
        .success()
        .stdout(predicate::str::contains("2 group(s) deleted"));
    assert!(runs.join("checkpoint.json").exists());

    // verify-log reports the checkpoint and stays intact.
    Command::cargo_bin("ritual")
        .unwrap()
        .current_dir(tmp.path())
        .arg("verify-log")
        .assert()
        .success()
        .stdout(predicate::str::contains("chain intact: checkpoint("));

    // A NEW real run chains onto the survivor and everything still verifies.
    Command::cargo_bin("ritual")
        .unwrap()
        .current_dir(tmp.path())
        .env("RITUAL_CLAUDE_CMD", fake_agent())
        .env("RITUAL_CODEX_CMD", fake_agent())
        .env("FAKE_AGENT_DELAY", "0")
        .env(
            "FAKE_AGENT_FINDINGS",
            ".ritual/findings/20260712T170000Z-plan-review.json",
        )
        .args(["run", "plan-review"])
        .assert()
        .success();
    Command::cargo_bin("ritual")
        .unwrap()
        .current_dir(tmp.path())
        .arg("verify-log")
        .assert()
        .success()
        .stdout(predicate::str::contains("2 run(s) verified"));
}

#[test]
fn verify_log_detects_a_tampered_archive() {
    let tmp = project_with_one_run();
    // Sanity: the fresh chain verifies.
    Command::cargo_bin("ritual")
        .unwrap()
        .current_dir(tmp.path())
        .arg("verify-log")
        .assert()
        .success()
        .stdout(predicate::str::contains("chain intact"));

    // Tamper with the raw archive after the fact.
    let archive = std::fs::read_dir(tmp.path().join(".ritual/runs"))
        .unwrap()
        .filter_map(|e| e.ok())
        .find(|e| e.file_name().to_string_lossy().ends_with(".jsonl"))
        .expect("raw archive exists");
    std::fs::write(archive.path(), "tampered!\n").unwrap();

    // verify-log must now break and exit nonzero.
    Command::cargo_bin("ritual")
        .unwrap()
        .current_dir(tmp.path())
        .arg("verify-log")
        .assert()
        .code(1)
        .stderr(predicate::str::contains("CHAIN BROKEN"));
}
