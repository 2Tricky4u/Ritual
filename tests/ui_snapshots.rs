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
         ## Behavior (the contract: WHAT, not HOW)\nmust retry on drop\n\n\
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
                    error_subtype: None,
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
                missing: false,
            },
            ChatTarget {
                doc: DocKind::Spec,
                section: Some("Behavior (the contract: WHAT, not HOW)".into()),
                range: 5..7,
                missing: false,
            },
        ],
        target_idx: 1, // focused on the Behavior section
        scroll: 0,
        in_flight: false,
        pending: Default::default(),
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
               "snippet":"let st = load(&path)?; // no lock\nsave(&path, &st)?;",
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
fn dashboard_help_overlay_findings() {
    // The which-key overlay is context-aware: on Findings it lists the
    // finding-triage keys, not the Live-tab run keys.
    let tmp = tempfile::tempdir().unwrap();
    let mut app = setup_app(&tmp);
    app.tab = Tab::Findings;
    app.show_help = true;
    insta::assert_snapshot!(render(&app));
}

#[test]
fn dashboard_help_overlay_plan() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = setup_app(&tmp);
    app.tab = Tab::Plan;
    app.show_help = true;
    insta::assert_snapshot!(render(&app));
}

#[test]
fn dashboard_help_overlay_history() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = setup_app(&tmp);
    app.tab = Tab::History;
    app.show_help = true;
    insta::assert_snapshot!(render(&app));
}

#[test]
fn dashboard_help_overlay_guide() {
    // Guide previously had NO context section at all; it now advertises
    // enter (run selected stage) plus the shared actions.
    let tmp = tempfile::tempdir().unwrap();
    let mut app = setup_app(&tmp);
    app.tab = Tab::Guide;
    app.show_help = true;
    insta::assert_snapshot!(render(&app));
}

#[test]
fn dashboard_help_overlay_finding_detail() {
    // Over the finding-detail modal, which-key advertises ONLY the keys
    // `detail_input` honors (finding actions + up/down), not the global/tab
    // keys that modal swallows.
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join(".ritual/findings")).unwrap();
    std::fs::write(
        tmp.path()
            .join(".ritual/findings/20260711T000000Z-dual-review.json"),
        r#"{"ritual_findings":1,"stage":"dual-review","branch":"main","findings":[{"id":1,"severity":"major","title":"x","file":"src/a.rs","line":1,"scenario":"s","sources":["claude"],"verdict":"confirmed","action":"pending"}]}"#,
    )
    .unwrap();
    let mut app = setup_app(&tmp);
    app.tab = Tab::Findings;
    app.finding_detail = true;
    app.show_help = true;
    insta::assert_snapshot!(render(&app));
}

#[test]
fn dashboard_findings_tab_triage_states() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join(".ritual/findings")).unwrap();
    std::fs::write(
        tmp.path()
            .join(".ritual/findings/20260713T000000Z-plan-review.json"),
        r#"{"ritual_findings":1,"stage":"plan-review","branch":"main",
            "findings":[
              {"id":1,"severity":"major","title":"queued for the claude batch",
               "plan_step":"Step 2","scenario":"s1","sources":["claude"],
               "verdict":"confirmed","action":"pending","answer":"auto"},
              {"id":2,"severity":"major","title":"yours to fix by hand",
               "file":"src/a.rs","line":3,"scenario":"s2","sources":["codex"],
               "verdict":"confirmed","action":"pending","answer":"manual"},
              {"id":3,"severity":"minor","title":"declined by the last batch",
               "plan_step":"Step 4","scenario":"s3","sources":["claude"],
               "verdict":"confirmed","action":"pending","reason":"needs a spec change"},
              {"id":4,"severity":"major","title":"resolution recorded by the review",
               "plan_step":"Step 5","scenario":"s4","sources":["claude","codex"],
               "verdict":"accepted","action":"Resolved by reordering the dump before the snapshot."},
              {"id":5,"severity":"minor","title":"retracted in round 2",
               "plan_step":"Step 6","scenario":"s5","sources":["codex"],
               "verdict":"refuted","action":"pending"},
              {"id":6,"severity":"critical","title":"untriaged confirmed plan gap",
               "plan_step":"Step 2","scenario":"s6","sources":["claude","codex"],
               "verdict":"confirmed","action":"pending"}
            ]}"#,
    )
    .unwrap();
    let mut app = setup_app(&tmp);
    app.tab = Tab::Findings;
    insta::assert_snapshot!(render(&app));
}

#[test]
fn dashboard_triage_confirm() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = setup_app(&tmp);
    app.tab = Tab::Findings;
    app.triage_confirm = Some(ritual::ui::app::TriageConfirm {
        items: Vec::new(),
        archive: 31,
        queue_auto: 2,
        queue_manual: 1,
        dismiss: 1,
        needs_you: 3,
    });
    insta::assert_snapshot!(render(&app));
}

/// The worst-case apply confirm (code findings + a note line): the y/u/esc
/// key row must stay visible - a fixed panel height used to clip it.
#[test]
fn dashboard_apply_confirm_code_findings() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = setup_app(&tmp);
    app.tab = Tab::Findings;
    app.apply_confirm = Some(ritual::ui::app::ApplyConfirm {
        slug: "detached".into(),
        count: 4,
        plan_count: 0,
        code_count: 4,
        skipped_other_features: 1,
        anchor_lost: 0,
        unqueue: None,
    });
    insta::assert_snapshot!(render(&app));
}

#[test]
fn dashboard_finding_detail_code_finding() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join(".ritual/findings")).unwrap();
    std::fs::write(
        tmp.path()
            .join(".ritual/findings/20260711T000000Z-dual-review.json"),
        r#"{"ritual_findings":1,"stage":"dual-review","branch":"main",
            "findings":[
              {"id":1,"severity":"critical","title":"Race in state save","file":"src/state.rs","line":42,
               "snippet":"let st = load(&path)?; // no lock\nsave(&path, &st)?;",
               "scenario":"two writers clobber each other","sources":["claude","codex"],
               "verdict":"confirmed","action":"pending"}
            ]}"#,
    )
    .unwrap();
    let mut app = setup_app(&tmp);
    app.tab = Tab::Findings;
    app.finding_detail = true;
    insta::assert_snapshot!(render(&app));
}

#[test]
fn dashboard_finding_detail_plan_finding_wraps() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join(".ritual/findings")).unwrap();
    std::fs::write(
        tmp.path()
            .join(".ritual/findings/20260711T000000Z-plan-review.json"),
        r#"{"ritual_findings":1,"stage":"plan-review","branch":"main",
            "findings":[
              {"id":1,"severity":"major","title":"Deletion paths built from untrusted run ids can escape the runs dir",
               "plan_step":"Step 2 (delete via history::load_all metas)",
               "scenario":"A malicious or corrupt meta file carrying a run_id like ../../src lets the cleanup step build a deletion path outside .ritual/runs, deleting arbitrary project files when clean executes with --keep 0 on a poisoned workspace.",
               "sources":["claude"],"verdict":"confirmed","action":"pending"}
            ]}"#,
    )
    .unwrap();
    let mut app = setup_app(&tmp);
    app.tab = Tab::Findings;
    app.finding_detail = true;
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

/// The which-key honesty contract: every context advertises exactly the keys
/// that work there. Encoded literally so any drift (a new action forgotten,
/// a section reshuffle, a phantom unbound entry) fails loudly.
#[test]
fn whichkey_sections_advertise_exactly_what_works() {
    use ritual::keymap::Action::*;
    use ritual::ui::dashboard::{WkEntry, whichkey_sections};
    let tmp = tempfile::tempdir().unwrap();
    let mut app = setup_app(&tmp);

    let names = |app: &ritual::ui::app::App| -> Vec<(&'static str, usize)> {
        whichkey_sections(app)
            .iter()
            .map(|(t, v)| (*t, v.len()))
            .collect()
    };

    // Every tab carries its ctx + the three shared sections.
    for (tab, ctx, n) in [
        (Tab::Live, "run", 1),
        (Tab::Findings, "findings", 12),
        (Tab::History, "history", 2),
        (Tab::Plan, "plan", 2),
        (Tab::Guide, "guide", 1),
    ] {
        app.tab = tab;
        app.finding_detail = false;
        assert_eq!(
            names(&app),
            // actions grew to 9 with the palette-only Architect entry.
            vec![(ctx, n), ("actions", 9), ("global", 7), ("move", 6)],
            "sections for {ctx}"
        );
        // Confirm (Enter launches/opens) is advertised on EVERY tab.
        let sections = whichkey_sections(&app);
        assert!(
            sections[0].1.contains(&WkEntry::Act(Confirm)),
            "{ctx} must advertise enter"
        );
    }

    // The findings ctx carries the palette-only entry (renders as `:`), and
    // the plan ctx carries ResetPlan the same way.
    app.tab = Tab::Findings;
    assert!(
        whichkey_sections(&app)[0]
            .1
            .contains(&WkEntry::Act(FindingsApply)),
        "findings advertises the palette-only apply"
    );
    app.tab = Tab::Plan;
    assert!(
        whichkey_sections(&app)[0]
            .1
            .contains(&WkEntry::Act(ResetPlan))
    );

    // The stage-detail modal advertises only what stage_detail_input honors.
    app.stage_detail = true;
    let stage = whichkey_sections(&app);
    assert_eq!(stage.len(), 2);
    assert_eq!(stage[0].0, "stage");
    assert!(stage[0].1.contains(&WkEntry::Lit {
        keys: "esc/q/i",
        desc: "close"
    }));
    app.stage_detail = false;

    // The finding-detail modal advertises only what detail_input honors,
    // including the literal close keys.
    app.finding_detail = true;
    let detail = whichkey_sections(&app);
    assert_eq!(detail.len(), 2);
    assert_eq!(detail[0].0, "finding");
    assert!(detail[0].1.contains(&WkEntry::Lit {
        keys: "esc/q/enter",
        desc: "close"
    }));
    assert_eq!(
        detail[1],
        ("move", vec![WkEntry::Act(Up), WkEntry::Act(Down)])
    );
    app.finding_detail = false;

    // No phantom rows: every Act entry across all contexts either has a
    // bound chord or a non-empty palette label (renders as `:` — never
    // silently dropped like the old FindingsApply).
    let km = ritual::keymap::Keymap::default();
    for tab in [
        Tab::Live,
        Tab::Findings,
        Tab::History,
        Tab::Plan,
        Tab::Guide,
    ] {
        app.tab = tab;
        for (title, entries) in whichkey_sections(&app) {
            for e in entries {
                if let WkEntry::Act(a) = e {
                    assert!(
                        !km.chords_for(a).is_empty() || !ritual::keymap::describe(a).is_empty(),
                        "phantom entry {a:?} in section {title}"
                    );
                }
            }
        }
    }
}

/// A project whose whole pipeline finished two hours ago, then spec.md was
/// edited: plan/plan-review/tests-red must wear the stale marker, the label
/// turns warn, and the guidance strip names the rerun.
fn setup_stale_app(tmp: &tempfile::TempDir) -> App {
    ritual::scaffold::init(tmp.path(), false).unwrap();
    let fin = (chrono::Utc::now() - chrono::Duration::hours(2)).to_rfc3339();
    let stages: String = [
        "spec",
        "plan",
        "plan_review",
        "tests_red",
        "implement",
        "dual_review",
        "coverage",
    ]
    .iter()
    .map(|s| format!(r#""{s}":{{"status":"done","finished_at":"{fin}"}}"#))
    .collect::<Vec<_>>()
    .join(",");
    std::fs::write(
        tmp.path().join(".ritual/state.json"),
        format!(
            r#"{{"version":1,"features":{{"detached":{{"branch":"detached","title":"t",
                "created_at":"2026-07-16T00:00:00Z","updated_at":"2026-07-16T00:00:00Z",
                "stages":{{{stages}}}}}}}}}"#
        ),
    )
    .unwrap();
    std::fs::create_dir_all(tmp.path().join(".ritual/features/detached")).unwrap();
    // Touched NOW, i.e. after every finished_at: the staleness trigger.
    std::fs::write(
        tmp.path().join(".ritual/features/detached/spec.md"),
        "# Feature\n\n## Goal\nrefined after the pipeline ran\n",
    )
    .unwrap();
    let dirs = RitualDirs::new(tmp.path());
    App::new(Config::default(), dirs).unwrap()
}

#[test]
fn dashboard_sidebar_stale_stage_and_next_line() {
    let tmp = tempfile::tempdir().unwrap();
    let app = setup_stale_app(&tmp);
    let out = render(&app);
    assert!(out.contains("» next: plan"), "{out}");
    assert!(out.contains("spec.md changed"), "{out}");
    insta::assert_snapshot!(out);
}

#[test]
fn dashboard_sidebar_stale_stage_ascii() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = setup_stale_app(&tmp);
    app.cfg.theme.icons = ritual::theme::IconSet::Ascii;
    let out = render(&app);
    assert!(out.contains("[~]"), "ascii stale marker: {out}");
    insta::assert_snapshot!(out);
}

#[test]
fn dashboard_live_wraps_long_agent_output() {
    // A long assistant line used to clip at the right edge - readable up to the
    // pane width, then cut. It now word-wraps across rows; the tail of the
    // sentence is visible instead of lost.
    use ritual::runner::events::AgentEvent;
    let tmp = tempfile::tempdir().unwrap();
    let mut app = setup_app(&tmp);
    app.tab = ritual::ui::app::Tab::Live;
    app.stream.push(AgentEvent::Text {
        text: concat!(
            "I have finished writing the failing tests for the parser and they ",
            "all fail for the right reason: the drift-tolerant branch is not yet ",
            "implemented, so unknown events panic instead of becoming Raw.",
        )
        .into(),
    });
    let out = render(&app);
    // The tail of the sentence renders (it was clipped before wrapping).
    assert!(out.contains("Raw."), "long line must wrap, not clip: {out}");
    insta::assert_snapshot!(out);
}

#[test]
fn dashboard_stage_detail_stale() {
    // `i` on a Done-but-stale stage: status + finished + staleness reason +
    // the rerun suggestion.
    let tmp = tempfile::tempdir().unwrap();
    let mut app = setup_stale_app(&tmp);
    app.selected = 1; // plan (stale: spec touched after it ran)
    app.stage_detail = true;
    let out = render(&app);
    assert!(out.contains("spec.md changed after this ran"), "{out}");
    assert!(out.contains("re-run"), "{out}");
    insta::assert_snapshot!(out);
}

#[test]
fn dashboard_stage_detail_plan_shows_architecture_freshness() {
    // `i` on the plan stage surfaces the map's freshness (Missing here: a
    // fresh scaffold has no architecture.md) so the planner knows what the
    // plan will (not) be grounded in.
    let tmp = tempfile::tempdir().unwrap();
    let mut app = setup_app(&tmp);
    app.selected = 1; // plan
    app.stage_detail = true;
    let out = render(&app);
    assert!(out.contains("architecture map"), "{out}");
    assert!(out.contains("missing"), "{out}");
    insta::assert_snapshot!(out);
}

#[test]
fn dashboard_palette_surfaces_the_architect_action() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = setup_app(&tmp);
    app.palette = Some(ritual::ui::app::PaletteState {
        input: "architec".into(),
        selected: 0,
    });
    let out = render(&app);
    assert!(out.contains("refresh the architecture map"), "{out}");
    insta::assert_snapshot!(out);
}

#[test]
fn dashboard_stage_detail_blocked() {
    // `i` on coverage with no plan: the deliverables/launch blocker shows.
    let tmp = tempfile::tempdir().unwrap();
    let mut app = setup_app(&tmp);
    app.selected = 6; // coverage
    app.stage_detail = true;
    let out = render(&app);
    assert!(out.contains("plan.md missing"), "{out}");
    insta::assert_snapshot!(out);
}

#[test]
fn dashboard_history_scrolled_to_bottom() {
    // 30 runs on a 22-row frame: the oldest are beyond one screen and were
    // unreachable before history gained a scroll model. An over-large scroll
    // clamps to the renderer-reported extent (last page: oldest visible).
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join(".ritual/runs")).unwrap();
    for i in 0..30u32 {
        std::fs::write(
            tmp.path()
                .join(format!(".ritual/runs/20260712T{i:06}Z-x.meta.json")),
            format!(
                r#"{{"run_id":"r{i:02}","stage":"run-{i:02}","ok":true,"total_cost_usd":0.1,"started_at":"2026-07-12T00:{:02}:00Z"}}"#,
                i % 60
            ),
        )
        .unwrap();
    }
    let mut app = setup_app(&tmp);
    app.tab = Tab::History;
    app.history_scroll = 999; // renderer clamps to the real extent
    let _ = render(&app); // first pass populates view_max
    let out = render(&app);
    assert!(out.contains("run-00"), "oldest run reachable: {out}");
    insta::assert_snapshot!(out);
}

#[test]
fn dashboard_statusline_spend_sparkline() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join(".ritual/runs")).unwrap();
    // Several costed runs → the statusline sparkline draws their burn shape.
    for (i, cost) in [0.10, 0.45, 0.22, 0.80, 0.33].iter().enumerate() {
        std::fs::write(
            tmp.path()
                .join(format!(".ritual/runs/20260712T00000{i}Z-x.meta.json")),
            format!(
                r#"{{"run_id":"r{i}","stage":"plan-review","ok":true,"total_cost_usd":{cost},"started_at":"{}"}}"#,
                chrono::Utc::now().to_rfc3339()
            ),
        )
        .unwrap();
    }
    let mut app = setup_app(&tmp);
    app.cfg.budget_daily_usd = Some(5.0);
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

/// A settings overlay with real provenance: one project-config key so its
/// row carries `(project)` against `(default)` siblings.
fn setup_settings_app(tmp: &tempfile::TempDir) -> App {
    std::fs::create_dir_all(tmp.path().join(".ritual")).unwrap();
    std::fs::write(
        tmp.path().join(".ritual/config.toml"),
        "budget_finding_fix_usd = 3.0\n",
    )
    .unwrap();
    let mut app = setup_app(tmp);
    app.cfg = Config::load(tmp.path(), None, false).unwrap();
    let project = tmp.path().join(".ritual/config.toml");
    let sources: Vec<&'static str> = ritual::settings::CATALOG
        .iter()
        .map(|d| ritual::settings::source_of(None, &project, d.key).tag())
        .collect();
    app.settings = Some(ritual::ui::app::SettingsState {
        selected: 0,
        edit: None,
        sources,
    });
    app
}

fn settings_idx(key: &str) -> usize {
    ritual::settings::CATALOG
        .iter()
        .position(|d| d.key == key)
        .expect(key)
}

#[test]
fn dashboard_implement_hint() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = setup_app(&tmp);
    app.implement_hint = Some(ritual::ui::app::ImplementHint {
        req: ritual::ui::app::AttachedRequest {
            stage: None,
            argv: vec![
                "claude".into(),
                "--resume".into(),
                "11111111-1111-4111-8111-111111111111".into(),
            ],
            cwd: tmp.path().to_path_buf(),
        },
        resuming: true,
        copied: true,
    });
    insta::assert_snapshot!(render_at(&app, 90, 24));
}

#[test]
fn dashboard_settings_overlay() {
    let tmp = tempfile::tempdir().unwrap();
    let app = setup_settings_app(&tmp);
    insta::assert_snapshot!(render_at(&app, 90, 30));
}

#[test]
fn dashboard_settings_edit_error() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = setup_settings_app(&tmp);
    let s = app.settings.as_mut().unwrap();
    s.selected = settings_idx("budget_finding_fix_usd");
    s.edit = Some(ritual::ui::app::SettingsEdit {
        input: "abc".into(),
        error: Some("must be a number > 0".into()),
    });
    insta::assert_snapshot!(render_at(&app, 90, 30));
}

#[test]
fn dashboard_settings_scrolled_to_last_entry() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = setup_settings_app(&tmp);
    app.settings.as_mut().unwrap().selected = ritual::settings::CATALOG.len() - 1;
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

/// Rendering must never panic, not on a 1×1 terminal, not on absurd aspect
/// ratios, not in any tab or overlay. ratatui panics on out-of-bounds Rect
/// math, so this is the guard against a resize crashing the whole TUI.
#[test]
fn rendering_survives_hostile_sizes_in_every_state() {
    // A representative app for each distinct layout path.
    type Make = Box<dyn Fn() -> (tempfile::TempDir, App)>;
    let states: Vec<(&str, Make)> = vec![
        (
            "empty",
            Box::new(|| {
                let t = tempfile::tempdir().unwrap();
                let a = setup_app(&t);
                (t, a)
            }),
        ),
        (
            "findings",
            Box::new(|| {
                let t = tempfile::tempdir().unwrap();
                std::fs::create_dir_all(t.path().join(".ritual/findings")).unwrap();
                std::fs::write(
                    t.path().join(".ritual/findings/20260712T000000Z-dual-review.json"),
                    r#"{"stage":"dual-review","findings":[
                        {"id":1,"severity":"critical","title":"a long finding title that will need clipping on narrow terminals","file":"src/state.rs","line":42,
                         "snippet":"let st = load(&path)?;","scenario":"two writers race","sources":["claude","codex"],"verdict":"confirmed","action":"pending"}]}"#,
                )
                .unwrap();
                let mut a = setup_app(&t); // App::new loads the findings dir
                a.tab = Tab::Findings;
                (t, a)
            }),
        ),
        (
            "help-overlay",
            Box::new(|| {
                let t = tempfile::tempdir().unwrap();
                let mut a = setup_app(&t);
                a.show_help = true;
                (t, a)
            }),
        ),
        (
            "triage-confirm",
            Box::new(|| {
                let t = tempfile::tempdir().unwrap();
                let mut a = setup_app(&t);
                a.tab = Tab::Findings;
                a.triage_confirm = Some(ritual::ui::app::TriageConfirm {
                    items: Vec::new(),
                    archive: 31,
                    queue_auto: 2,
                    queue_manual: 1,
                    dismiss: 1,
                    needs_you: 3,
                });
                (t, a)
            }),
        ),
        (
            "apply-confirm",
            Box::new(|| {
                let t = tempfile::tempdir().unwrap();
                let mut a = setup_app(&t);
                a.tab = Tab::Findings;
                a.apply_confirm = Some(ritual::ui::app::ApplyConfirm {
                    slug: "detached".into(),
                    count: 3,
                    plan_count: 3,
                    code_count: 0,
                    skipped_other_features: 1,
                    anchor_lost: 1,
                    unqueue: None,
                });
                (t, a)
            }),
        ),
        (
            "dismiss-prompt",
            Box::new(|| {
                let t = tempfile::tempdir().unwrap();
                std::fs::create_dir_all(t.path().join(".ritual/findings")).unwrap();
                std::fs::write(
                    t.path().join(".ritual/findings/20260712T000000Z-plan-review.json"),
                    r#"{"stage":"plan-review","findings":[
                        {"id":1,"severity":"minor","title":"a finding being dismissed with a very long title that must clip","plan_step":"Step 1","verdict":"confirmed"}]}"#,
                )
                .unwrap();
                let mut a = setup_app(&t);
                a.tab = Tab::Findings;
                a.dismiss_prompt = Some(ritual::ui::app::DismissPrompt {
                    findings_path: t
                        .path()
                        .join(".ritual/findings/20260712T000000Z-plan-review.json"),
                    pos: 0,
                    title: "a finding being dismissed with a very long title that must clip".into(),
                    input: "typed reason".into(),
                });
                (t, a)
            }),
        ),
        (
            "finding-detail",
            Box::new(|| {
                let t = tempfile::tempdir().unwrap();
                std::fs::create_dir_all(t.path().join(".ritual/findings")).unwrap();
                std::fs::write(
                    t.path().join(".ritual/findings/20260712T000000Z-plan-review.json"),
                    r#"{"stage":"plan-review","findings":[
                        {"id":1,"severity":"major","title":"plan finding with a long scenario for wrap math",
                         "plan_step":"Step 2 (deletion)","snippet":"2. Enumerate by FILENAME",
                         "scenario":"a scenario long enough to wrap across the detail panel on every hostile width we throw at it","sources":["claude"],"verdict":"confirmed","action":"pending"}]}"#,
                )
                .unwrap();
                let mut a = setup_app(&t);
                a.tab = Tab::Findings;
                a.finding_detail = true;
                (t, a)
            }),
        ),
        (
            "palette",
            Box::new(|| {
                let t = tempfile::tempdir().unwrap();
                let mut a = setup_app(&t);
                a.palette = Some(ritual::ui::app::PaletteState {
                    input: "run".into(),
                    selected: 3,
                });
                (t, a)
            }),
        ),
        (
            "chat",
            Box::new(|| {
                let t = tempfile::tempdir().unwrap();
                let a = setup_chat_app(&t);
                (t, a)
            }),
        ),
        (
            "settings-overlay",
            Box::new(|| {
                let t = tempfile::tempdir().unwrap();
                let a = setup_settings_app(&t);
                (t, a)
            }),
        ),
        (
            "implement-hint",
            Box::new(|| {
                let t = tempfile::tempdir().unwrap();
                let mut a = setup_app(&t);
                a.implement_hint = Some(ritual::ui::app::ImplementHint {
                    req: ritual::ui::app::AttachedRequest {
                        stage: None,
                        argv: vec!["claude".into(), "--resume".into(), "uuid".into()],
                        cwd: t.path().to_path_buf(),
                    },
                    resuming: true,
                    copied: false,
                });
                (t, a)
            }),
        ),
        (
            "settings-edit",
            Box::new(|| {
                let t = tempfile::tempdir().unwrap();
                let mut a = setup_settings_app(&t);
                let s = a.settings.as_mut().unwrap();
                s.selected = settings_idx("base_ref");
                s.edit = Some(ritual::ui::app::SettingsEdit {
                    input: "a very long base ref typed to test clipping".into(),
                    error: Some("a value is required (esc to cancel)".into()),
                });
                (t, a)
            }),
        ),
        (
            "live-stale-scroll",
            Box::new(|| {
                // A ring-buffer drain dropped 1000 events while the user was
                // scrolled near the old end: the stale Some(4990) must clamp,
                // not slice out of bounds.
                let t = tempfile::tempdir().unwrap();
                let mut a = setup_app(&t);
                a.tab = ritual::ui::app::Tab::Live;
                for i in 0..50 {
                    a.stream.push(ritual::runner::events::AgentEvent::Text {
                        text: format!("event {i}"),
                    });
                }
                a.stream_scroll = Some(4990);
                (t, a)
            }),
        ),
        (
            "stage-detail",
            Box::new(|| {
                let t = tempfile::tempdir().unwrap();
                let mut a = setup_app(&t);
                a.stage_detail = true;
                a.selected = 6;
                (t, a)
            }),
        ),
        (
            "guide",
            Box::new(|| {
                let t = tempfile::tempdir().unwrap();
                let mut a = setup_app(&t);
                a.tab = Tab::Guide;
                (t, a)
            }),
        ),
    ];

    // From degenerate (1×1) through narrow/short/tall to the sidebar/chat
    // breakpoints (28, 55, 70, 100) ±1 on each side.
    let sizes: &[(u16, u16)] = &[
        (1, 1),
        (1, 40),
        (40, 1),
        (2, 2),
        (10, 5),
        (27, 20),
        (28, 20),
        (54, 20),
        (55, 20),
        (69, 24),
        (70, 24),
        (99, 30),
        (100, 30),
        (200, 3),
        (200, 80),
    ];

    for (name, make) in &states {
        let (_tmp, app) = make();
        for &(w, h) in sizes {
            // The bare fact that draw() returns is the assertion: a panic in
            // any layout arithmetic would fail the test with the size + state.
            let out =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| render_at(&app, w, h)));
            assert!(out.is_ok(), "render panicked: state={name} size={w}x{h}");
        }
    }
}
