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

    // ...and shares the MAIN repo's .ritual: status from inside the worktree
    // sees the same state file.
    assert!(!wt.join(".ritual").exists());
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
