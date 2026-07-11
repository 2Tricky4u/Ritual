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
    let backend = TestBackend::new(90, 22);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|f| dashboard::draw(f, app)).unwrap();
    terminal.backend().to_string()
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
fn dashboard_palette_filters() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = setup_app(&tmp);
    app.palette = Some(ritual::ui::app::PaletteState {
        input: "run".into(),
        selected: 1,
    });
    insta::assert_snapshot!(render(&app));
}
