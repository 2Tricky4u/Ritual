//! Styled non-TUI rendering shared by `ritual status/findings/history/init`
//! and (in M2) live `ritual run` streaming.

use owo_colors::OwoColorize;

use crate::config::Config;
use crate::findings::LoadedFindings;
use crate::history::{DaySummary, RunMeta};
use crate::scaffold::InitReport;
use crate::state::{Feature, PIPELINE, StageStatus};
use crate::theme::Theme;

fn hex(t: &Theme, c: (u8, u8, u8), s: &str) -> String {
    s.color(t.owo(c)).to_string()
}

pub fn stage_icon(t: &Theme, status: StageStatus) -> String {
    let p = t.palette;
    match status {
        StageStatus::Pending => hex(t, p.muted, t.icon_pending()),
        StageStatus::Running => hex(t, p.cyan, t.icon_running()),
        StageStatus::Done => hex(t, p.green, t.icon_done()),
        StageStatus::Failed => hex(t, p.red, t.icon_failed()),
        StageStatus::NeedsAttention => hex(t, p.yellow, t.icon_attention()),
        StageStatus::Skipped => hex(t, p.muted, t.icon_skipped()),
    }
}

pub fn render_status(cfg: &Config, features: &[(String, Feature)], current_branch: Option<&str>) {
    let t = &cfg.theme;
    let p = t.palette;
    println!("{}", hex(t, p.purple, "ritual — pipeline status"));
    println!();
    if features.is_empty() {
        println!(
            "  {}",
            hex(
                t,
                p.muted,
                "no features yet — run `ritual new <title>` or just start a branch"
            )
        );
        return;
    }
    for (slug, feature) in features {
        let is_current = current_branch == Some(feature.branch.as_str());
        let branch = format!("{} {}", t.icon_branch(), feature.branch);
        let branch = if is_current {
            hex(t, p.pink, &branch)
        } else {
            hex(t, p.muted, &branch)
        };
        println!(
            "  {} {}  {}",
            hex(t, p.fg, &feature.title),
            hex(t, p.muted, slug),
            branch
        );
        print!("    ");
        for (i, id) in PIPELINE.iter().enumerate() {
            let st = feature.stage(*id);
            if i > 0 {
                print!("{}", hex(t, p.muted, "─"));
            }
            print!(
                "{} {}",
                stage_icon(t, st.status),
                hex(t, p.muted, id.label())
            );
            print!(" ");
        }
        println!();
        println!();
    }
}

pub fn render_findings(cfg: &Config, loaded: &[LoadedFindings], json: bool) {
    let t = &cfg.theme;
    let p = t.palette;
    if json {
        let all: Vec<_> = loaded.iter().map(|l| &l.file).collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&all).unwrap_or_else(|_| "[]".into())
        );
        return;
    }
    let agg = crate::findings::aggregate(loaded);
    if agg.is_empty() {
        println!(
            "{}",
            hex(
                t,
                p.muted,
                "no findings recorded — run plan-review or dual-review first"
            )
        );
        return;
    }
    println!("{}", hex(t, p.purple, "ritual — findings"));
    println!();
    for (src_idx, f) in &agg {
        let sev = match f.severity {
            crate::findings::Severity::Critical => hex(t, p.red, "critical"),
            crate::findings::Severity::Major => hex(t, p.orange, "major   "),
            crate::findings::Severity::Minor => hex(t, p.yellow, "minor   "),
        };
        let badge = if f.cross_confirmed() {
            hex(t, p.green, "◆ both")
        } else {
            hex(t, p.orange, "◇ single")
        };
        let stage = &loaded[*src_idx].file.stage;
        println!(
            "  {} {} {}  {}  {}",
            t.icon_finding(),
            sev,
            badge,
            hex(t, p.cyan, &f.location()),
            hex(t, p.fg, &f.title),
        );
        if !f.scenario.is_empty() {
            println!("      {}", hex(t, p.muted, &f.scenario));
        }
        println!(
            "      {}",
            hex(
                t,
                p.muted,
                &format!(
                    "verdict: {}  action: {}  stage: {}",
                    f.verdict, f.action, stage
                )
            )
        );
    }
}

pub fn render_history(cfg: &Config, metas: &[RunMeta], summary: &DaySummary, limit: usize) {
    let t = &cfg.theme;
    let p = t.palette;
    println!("{}", hex(t, p.purple, "ritual — run history"));
    println!();
    if metas.is_empty() {
        println!("  {}", hex(t, p.muted, "no runs yet"));
        return;
    }
    for m in metas.iter().take(limit) {
        let status = if m.ok {
            hex(t, p.green, t.icon_done())
        } else {
            hex(t, p.red, t.icon_failed())
        };
        let when = m
            .started_at
            .map(|d| d.format("%m-%d %H:%M").to_string())
            .unwrap_or_else(|| "?".into());
        let cost = m
            .total_cost_usd
            .map(|c| format!("${c:.3}"))
            .unwrap_or_else(|| "-".into());
        let tokens = m
            .usage
            .as_ref()
            .map(|u| format!("{}↑ {}↓", u.input_tokens, u.output_tokens))
            .unwrap_or_else(|| "-".into());
        let dur = m
            .duration_ms
            .map(|d| format!("{:.1}s", d as f64 / 1000.0))
            .unwrap_or_else(|| "-".into());
        println!(
            "  {} {}  {}  {}  {}  {}  {}",
            status,
            hex(t, p.muted, &when),
            hex(t, p.fg, &format!("{:<12}", m.stage)),
            hex(t, p.cyan, &format!("{:<8}", m.agent)),
            hex(t, p.orange, &format!("{cost:>8}")),
            hex(t, p.muted, &format!("{tokens:>16}")),
            hex(t, p.muted, &dur),
        );
    }
    println!();
    println!(
        "  {}",
        hex(
            t,
            p.muted,
            &format!(
                "today: {} runs, ${:.2}, {} output tokens",
                summary.runs, summary.cost_usd, summary.output_tokens
            )
        )
    );
}

pub fn render_init(cfg: &Config, report: &InitReport) {
    let t = &cfg.theme;
    let p = t.palette;
    println!("{}", hex(t, p.purple, "ritual init"));
    if let Some(stack) = report.stack {
        println!(
            "  {} {}",
            hex(t, p.muted, "detected stack:"),
            hex(t, p.cyan, stack.label())
        );
    }
    for a in &report.actions {
        println!("  {} {}", hex(t, p.green, t.icon_done()), a);
    }
    for s in &report.skipped {
        println!("  {} {}", hex(t, p.yellow, t.icon_attention()), s);
    }
    if report.actions.is_empty() {
        println!(
            "  {}",
            hex(t, p.muted, "nothing to do — already initialized")
        );
    }
}
