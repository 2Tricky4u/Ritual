//! End-to-end subcommand tests. Agent runs use tests/fake_agent.sh via the
//! RITUAL_CLAUDE_CMD / RITUAL_CODEX_CMD seams — zero tokens burned.

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

    // Findings browser shows the canned finding — which is a confirmed
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
