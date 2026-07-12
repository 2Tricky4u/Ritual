//! Deterministic dashboard renders on ratatui's TestBackend, snapshotted
//! with insta. `cargo insta review` to approve changes.

use ratatui::Terminal;
use ratatui::backend::TestBackend;

use ritual::config::Config;
use ritual::state::RitualDirs;
use ritual::ui::app::{App, Tab};
use ritual::ui::dashboard;

fn setup_app(tmp: &tempfile::TempDir) -> App {
    ritual::scaffold::init(tmp.path(), false).unwrap();
    let dirs = RitualDirs::new(tmp.path());
    let cfg = Config::default();
    App::new(cfg, dirs).unwrap()
}

fn render(app: &App) -> String {
    render_at(app, 90, 22)
}

fn render_at(app: &App, w: u16, h: u16) -> String {
    let backend = TestBackend::new(w, h);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|f| dashboard::draw(f, app)).unwrap();
    terminal.backend().to_string()
}

/// An app in spec-chat mode: a seeded spec, a couple of transcript turns, a
/// section target, and a caret sitting mid-input.
fn setup_chat_app(tmp: &tempfile::TempDir) -> App {
    use ritual::runner::events::AgentEvent;
    use ritual::stages::DocKind;
    use ritual::ui::app::{ChatState, ChatTarget, ChatTurn};

    ritual::scaffold::init(tmp.path(), false).unwrap();
    // No git in the tempdir → slug "detached".
    std::fs::create_dir_all(tmp.path().join(".ritual/features/detached")).unwrap();
    std::fs::write(
        tmp.path().join(".ritual/features/detached/spec.md"),
        "# Feature: Audio\n\n## Goal\nlow-latency playback\n\n\
         ## Behavior (the contract — WHAT, not HOW)\nmust retry on drop\n\n\
         ## Edge cases & failure modes\n\n## Out of scope\n",
    )
    .unwrap();
    let dirs = RitualDirs::new(tmp.path());
    let mut app = App::new(Config::default(), dirs).unwrap();
    app.chat = Some(ChatState {
        transcript: vec![
            ChatTurn::User("make behavior stricter: retry 3x".into()),
            ChatTurn::Assistant(vec![
                AgentEvent::Text {
                    text: "Tightening the retry rule.".into(),
                },
                AgentEvent::Completed {
                    ok: true,
                    result_text: None,
                    total_cost_usd: Some(0.03),
                    usage: None,
                    num_turns: Some(2),
                    duration_ms: Some(4200),
                    permission_denials: vec![],
                },
            ]),
            ChatTurn::System("✓ spec updated · $0.030".into()),
        ],
        input: "and cap it at 3 attempts".chars().collect(),
        cursor: 4, // caret mid-string
        targets: vec![
            ChatTarget {
                doc: DocKind::Spec,
                section: None,
                range: 0..9,
            },
            ChatTarget {
                doc: DocKind::Spec,
                section: Some("Behavior (the contract — WHAT, not HOW)".into()),
                range: 5..7,
            },
        ],
        target_idx: 1, // focused on the Behavior section
        scroll: 0,
        in_flight: false,
    });
    app
}

#[test]
fn dashboard_empty_project() {
    let tmp = tempfile::tempdir().unwrap();
    let app = setup_app(&tmp);
    insta::assert_snapshot!(render(&app));
}

#[test]
fn dashboard_findings_tab_with_data() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join(".ritual/findings")).unwrap();
    std::fs::write(
        tmp.path().join(".ritual/findings/20260711T000000Z-dual-review.json"),
        r#"{"ritual_findings":1,"stage":"dual-review","branch":"main",
            "findings":[
              {"id":1,"severity":"critical","title":"Race in state save","file":"src/state.rs","line":42,
               "scenario":"two writers","sources":["claude","codex"],"verdict":"confirmed","action":"pending"},
              {"id":2,"severity":"minor","title":"Long line","file":"src/x.rs","line":7,
               "scenario":"style-ish","sources":["codex"],"verdict":"unconfirmed","action":"pending"}
            ]}"#,
    )
    .unwrap();
    let mut app = setup_app(&tmp);
    app.tab = Tab::Findings;
    insta::assert_snapshot!(render(&app));
}

#[test]
fn dashboard_findings_tab_with_resolved_shown() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join(".ritual/findings")).unwrap();
    std::fs::write(
        tmp.path()
            .join(".ritual/findings/20260712T000000Z-dual-review.json"),
        r#"{"ritual_findings":1,"stage":"dual-review","branch":"main",
            "findings":[
              {"id":1,"severity":"critical","title":"Open bug","file":"src/a.rs","line":3,
               "scenario":"boom","sources":["claude","codex"],"verdict":"confirmed","action":"pending"},
              {"id":2,"severity":"major","title":"Was fixed","file":"src/b.rs","line":9,
               "scenario":"","sources":["codex"],"verdict":"confirmed","action":"fixed"},
              {"id":3,"severity":"minor","title":"Noise","file":null,"line":null,
               "scenario":"","sources":["claude"],"verdict":"unconfirmed","action":"dismissed"}
            ]}"#,
    )
    .unwrap();
    let mut app = setup_app(&tmp);
    app.tab = Tab::Findings;
    app.show_resolved = true;
    insta::assert_snapshot!(render(&app));
}

#[test]
fn dashboard_help_overlay() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = setup_app(&tmp);
    app.show_help = true;
    insta::assert_snapshot!(render(&app));
}

#[test]
fn dashboard_ascii_mode() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = setup_app(&tmp);
    app.cfg.theme.icons = ritual::theme::IconSet::Ascii;
    insta::assert_snapshot!(render(&app));
}

#[test]
fn dashboard_running_with_budget() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join(".ritual/runs")).unwrap();
    std::fs::write(
        tmp.path().join(".ritual/runs/20260712T000000Z-x.meta.json"),
        format!(
            r#"{{"run_id":"r","stage":"plan-review","ok":true,"total_cost_usd":4.2,"started_at":"{}"}}"#,
            chrono::Utc::now().to_rfc3339()
        ),
    )
    .unwrap();
    let mut app = setup_app(&tmp);
    app.cfg.budget_daily_usd = Some(5.0);
    app.running = Some(ritual::state::StageId::PlanReview);
    app.check = ritual::ui::app::CheckState::Green;
    insta::assert_snapshot!(render(&app));
}

#[test]
fn dashboard_plan_tab_renders_markdown() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join(".ritual/features/detached")).unwrap();
    std::fs::write(
        tmp.path().join(".ritual/features/detached/plan.md"),
        "# The Plan\n\nSome **bold** intro with `inline code`.\n\n- first step\n- second step\n  - nested detail\n\n```rust\nfn ritual() {}\n```\n\n| stage | state |\n|---|---|\n| spec | done |\n",
    )
    .unwrap();
    let mut app = setup_app(&tmp);
    app.tab = Tab::Plan;
    insta::assert_snapshot!(render(&app));
}

#[test]
fn dashboard_spec_chat_wide() {
    // 120 cols: sidebar + spec preview | chat side by side.
    let tmp = tempfile::tempdir().unwrap();
    let app = setup_chat_app(&tmp);
    insta::assert_snapshot!(render_at(&app, 120, 30));
}

#[test]
fn dashboard_spec_chat_narrow() {
    // 80 cols: sidebar dropped, preview | chat take the full width.
    let tmp = tempfile::tempdir().unwrap();
    let app = setup_chat_app(&tmp);
    insta::assert_snapshot!(render_at(&app, 80, 24));
}

#[test]
fn dashboard_spec_chat_multiline_input() {
    // A 3-line input: the box grows and the caret sits mid-line-two.
    let tmp = tempfile::tempdir().unwrap();
    let mut app = setup_chat_app(&tmp);
    {
        let chat = app.chat.as_mut().unwrap();
        chat.input = "first line\nsecond line\nthird".chars().collect();
        chat.cursor = 17; // inside "second line"
    }
    insta::assert_snapshot!(render_at(&app, 100, 26));
}

#[test]
fn dashboard_spec_chat_ascii() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = setup_chat_app(&tmp);
    app.cfg.theme.icons = ritual::theme::IconSet::Ascii;
    insta::assert_snapshot!(render_at(&app, 100, 26));
}

#[test]
fn dashboard_guide_tab_renders() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = setup_app(&tmp);
    app.tab = Tab::Guide;
    app.guide_scroll = 4; // land on real content, prove scrolling works
    insta::assert_snapshot!(render(&app));
}

#[test]
fn dashboard_palette_filters() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = setup_app(&tmp);
    app.palette = Some(ritual::ui::app::PaletteState {
        input: "run".into(),
        selected: 1,
    });
    insta::assert_snapshot!(render(&app));
}
