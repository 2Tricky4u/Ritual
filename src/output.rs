//! Styled non-TUI rendering shared by `ritual status/findings/history/init`
//! and (in M2) live `ritual run` streaming.

use owo_colors::OwoColorize;

use crate::config::Config;
use crate::findings::LoadedFindings;
use crate::history::{DaySummary, RunMeta};
use crate::runner::events::AgentEvent;
use crate::scaffold::InitReport;
use crate::state::{Feature, PIPELINE, StageStatus};
use crate::theme::Theme;

/// Colors only when stdout is a real terminal and NO_COLOR is unset —
/// piped output (scripts, tests) stays clean.
fn colors_enabled() -> bool {
    use std::io::IsTerminal;
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED
        .get_or_init(|| std::env::var_os("NO_COLOR").is_none() && std::io::stdout().is_terminal())
}

fn hex(t: &Theme, c: (u8, u8, u8), s: &str) -> String {
    if colors_enabled() {
        s.color(t.owo(c)).to_string()
    } else {
        s.to_string()
    }
}

pub fn stage_icon(t: &Theme, status: StageStatus) -> String {
    let p = t.palette;
    match status {
        StageStatus::Pending => hex(t, p.light_grey, t.icon_pending()),
        StageStatus::Running => hex(t, p.cyan, t.icon_running()),
        StageStatus::Done => hex(t, p.green, t.icon_done()),
        StageStatus::Failed => hex(t, p.red, t.icon_failed()),
        StageStatus::NeedsAttention => hex(t, p.yellow, t.icon_attention()),
        StageStatus::Skipped => hex(t, p.light_grey, t.icon_skipped()),
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
                p.light_grey,
                "no features yet — run `ritual new <title>` or just start a branch"
            )
        );
        return;
    }
    for (slug, feature) in features {
        let is_current = current_branch == Some(feature.branch.as_str());
        let branch = format!("{} {}", t.icon_branch(), feature.branch);
        let branch = if is_current {
            hex(t, p.baby_pink, &branch)
        } else {
            hex(t, p.light_grey, &branch)
        };
        println!(
            "  {} {}  {}",
            hex(t, p.white, &feature.title),
            hex(t, p.light_grey, slug),
            branch
        );
        print!("    ");
        for (i, id) in PIPELINE.iter().enumerate() {
            let st = feature.stage(*id);
            if i > 0 {
                print!("{}", hex(t, p.light_grey, "─"));
            }
            print!(
                "{} {}",
                stage_icon(t, st.status),
                hex(t, p.light_grey, id.label())
            );
            print!(" ");
        }
        println!();
        println!();
    }
}

pub fn render_findings(cfg: &Config, loaded: &[LoadedFindings], json: bool, show_resolved: bool) {
    let t = &cfg.theme;
    let p = t.palette;
    if json {
        // Scripting contract: raw and complete, never filtered.
        let all: Vec<_> = loaded.iter().map(|l| &l.file).collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&all).unwrap_or_else(|_| "[]".into())
        );
        return;
    }
    let agg = crate::findings::aggregate(loaded, show_resolved);
    if agg.is_empty() && crate::findings::resolved_count(loaded) == 0 {
        println!(
            "{}",
            hex(
                t,
                p.light_grey,
                "no findings recorded — run plan-review or dual-review first"
            )
        );
        return;
    }
    println!("{}", hex(t, p.purple, "ritual — findings"));
    println!();
    for af in &agg {
        let f = &af.finding;
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
        let stage = &loaded[af.file_idx].file.stage;
        println!(
            "  {} {} {}  {}  {}",
            t.icon_finding(),
            sev,
            badge,
            hex(t, p.cyan, &f.location()),
            hex(t, p.white, &f.title),
        );
        if !f.scenario.is_empty() {
            println!("      {}", hex(t, p.light_grey, &f.scenario));
        }
        println!(
            "      {}",
            hex(
                t,
                p.light_grey,
                &format!(
                    "verdict: {}  action: {}  stage: {}",
                    f.verdict, f.action, stage
                )
            )
        );
    }
    let hidden = crate::findings::resolved_count(loaded);
    if !show_resolved && hidden > 0 {
        println!();
        println!(
            "  {}",
            hex(
                t,
                p.light_grey,
                &format!("{hidden} resolved finding(s) hidden — `ritual findings --all`")
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
        println!("  {}", hex(t, p.light_grey, "no runs yet"));
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
            hex(t, p.light_grey, &when),
            hex(t, p.white, &format!("{:<12}", m.stage)),
            hex(t, p.cyan, &format!("{:<8}", m.agent)),
            hex(t, p.orange, &format!("{cost:>8}")),
            hex(t, p.light_grey, &format!("{tokens:>16}")),
            hex(t, p.light_grey, &dur),
        );
    }
    println!();
    println!(
        "  {}",
        hex(
            t,
            p.light_grey,
            &format!(
                "today: {} runs, ${:.2}, {} output tokens",
                summary.runs, summary.cost_usd, summary.output_tokens
            )
        )
    );
}

/// Live rendering of one agent event during `ritual run <stage>`.
pub fn render_event(cfg: &Config, ev: &AgentEvent) {
    let t = &cfg.theme;
    let p = t.palette;
    match ev {
        AgentEvent::SessionStart {
            model, mcp_servers, ..
        } => {
            let servers: Vec<String> = mcp_servers
                .iter()
                .map(|(n, s)| format!("{n}:{s}"))
                .collect();
            println!(
                "{} {}  {}",
                hex(t, p.purple, t.icon_agent()),
                hex(t, p.purple, model),
                hex(t, p.light_grey, &servers.join(" "))
            );
        }
        AgentEvent::Thinking { text } => {
            println!("  {}", hex(t, p.light_grey, &clip(text, 160)));
        }
        AgentEvent::Text { text } => {
            for line in text.lines() {
                println!("  {}", hex(t, p.white, line));
            }
        }
        AgentEvent::ToolUse { name, summary } => {
            println!(
                "  {} {} {}",
                hex(t, p.cyan, "▸"),
                hex(t, p.cyan, name),
                hex(t, p.light_grey, summary)
            );
        }
        AgentEvent::ToolResult { is_error, summary } => {
            let c = if *is_error { p.red } else { p.light_grey };
            println!("    {} {}", hex(t, c, "↳"), hex(t, c, &clip(summary, 140)));
        }
        AgentEvent::RateLimit(info) => {
            if info.status.as_deref() != Some("allowed") {
                println!("  {} rate limit: {:?}", hex(t, p.orange, "!"), info.status);
            }
        }
        AgentEvent::Completed {
            ok,
            total_cost_usd,
            num_turns,
            duration_ms,
            ..
        } => {
            let (c, icon) = if *ok {
                (p.green, t.icon_done())
            } else {
                (p.red, t.icon_failed())
            };
            let cost = total_cost_usd
                .map(|c| format!("${c:.3}"))
                .unwrap_or_default();
            let dur = duration_ms
                .map(|d| format!("{:.1}s", d as f64 / 1000.0))
                .unwrap_or_default();
            let turns = num_turns.map(|n| format!("{n} turns")).unwrap_or_default();
            println!(
                "{} {}",
                hex(t, c, icon),
                hex(t, p.light_grey, &format!("{cost} {turns} {dur}"))
            );
        }
        AgentEvent::Stderr { line } => {
            println!("  {}", hex(t, p.light_grey, &clip(line, 160)));
        }
        AgentEvent::Raw { value } => {
            let kind = value
                .get("type")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("?");
            println!("  {}", hex(t, p.light_grey, &format!("· {kind}")));
        }
    }
}

pub fn render_run_summary(cfg: &Config, meta: &RunMeta, new_findings: &[String]) {
    let t = &cfg.theme;
    let p = t.palette;
    println!();
    let (c, verdict) = if meta.ok {
        (p.green, "ok")
    } else {
        (p.red, "failed")
    };
    println!(
        "{} {} {}  {}",
        hex(t, c, t.icon_check()),
        hex(t, p.white, &meta.stage),
        hex(t, c, verdict),
        hex(t, p.light_grey, &meta.run_id)
    );
    for f in new_findings {
        println!(
            "  {} {}",
            hex(t, p.baby_pink, t.icon_finding()),
            hex(
                t,
                p.white,
                &format!("findings: .ritual/findings/{f} — browse with `ritual findings`")
            )
        );
    }
    if let Some(err) = &meta.error {
        println!("  {}", hex(t, p.red, &clip(err, 300)));
    }
}

pub fn render_clean(cfg: &Config, report: &crate::clean::CleanReport) {
    let t = &cfg.theme;
    let p = t.palette;
    let verb = if report.dry_run {
        "would delete"
    } else {
        "deleted"
    };
    println!(
        "{}",
        hex(
            t,
            p.purple,
            &format!(
                "ritual clean{}",
                if report.dry_run { " (dry run)" } else { "" }
            )
        )
    );
    for id in &report.deleted_groups {
        println!("  {} {verb} {id}", hex(t, p.red, "✗"));
    }
    // Keep the noise down: summarize kept runs by reason.
    let mut by_reason: std::collections::BTreeMap<&str, usize> = Default::default();
    for (_, why) in &report.kept {
        *by_reason.entry(why.label()).or_default() += 1;
    }
    for (why, n) in by_reason {
        println!("  {} kept {n} ({why})", hex(t, p.green, "✓"));
    }
    for (id, err) in &report.failures {
        println!("  {} FAILED {id}: {err}", hex(t, p.red, "!"));
    }
    for n in &report.notices {
        println!("  {}", hex(t, p.yellow, n));
    }
    if let Some(cp) = &report.checkpoint {
        println!(
            "  {}",
            hex(
                t,
                p.light_grey,
                &format!(
                    "checkpoint{}: {} chained run(s) attested up to {}",
                    if report.dry_run { " (would write)" } else { "" },
                    cp.pruned_runs,
                    cp.as_of_run_id
                )
            )
        );
    }
    println!(
        "  {}",
        hex(
            t,
            p.light_grey,
            &format!(
                "{} group(s) {verb}, {} kept, {} failure(s)",
                report.deleted_groups.len(),
                report.kept.len(),
                report.failures.len()
            )
        )
    );
}

fn clip(s: &str, max: usize) -> String {
    let one_line = s.replace('\n', " ");
    let mut out: String = one_line.chars().take(max).collect();
    if one_line.chars().count() > max {
        out.push('…');
    }
    out
}

pub fn render_init(cfg: &Config, report: &InitReport) {
    let t = &cfg.theme;
    let p = t.palette;
    println!("{}", hex(t, p.purple, "ritual init"));
    if let Some(stack) = report.stack {
        println!(
            "  {} {}",
            hex(t, p.light_grey, "detected stack:"),
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
            hex(t, p.light_grey, "nothing to do — already initialized")
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clip_flattens_newlines_into_spaces() {
        assert_eq!(clip("a\nb\nc", 80), "a b c");
    }

    #[test]
    fn clip_truncates_with_ellipsis_past_max() {
        let out = clip(&"x".repeat(50), 10);
        assert_eq!(out.chars().count(), 11); // 10 + ellipsis
        assert!(out.ends_with('…'));
    }

    #[test]
    fn clip_leaves_short_strings_untouched() {
        assert_eq!(clip("short", 80), "short");
    }

    #[test]
    fn clip_counts_chars_not_bytes() {
        // No panic on a multi-byte boundary, and exact char count.
        assert_eq!(clip("ααααα", 3), "ααα…");
    }

    #[test]
    fn stage_icon_is_distinct_per_status_in_ascii() {
        // In ASCII mode every status still gets a distinct, non-empty glyph —
        // state is never color-only.
        let t = Theme {
            icons: crate::theme::IconSet::Ascii,
            ..Theme::default()
        };
        let icons: Vec<String> = [
            StageStatus::Pending,
            StageStatus::Running,
            StageStatus::Done,
            StageStatus::Failed,
            StageStatus::NeedsAttention,
            StageStatus::Skipped,
        ]
        .iter()
        .map(|s| stage_icon(&t, *s))
        .collect();
        assert!(icons.iter().all(|i| !i.trim().is_empty()));
        // Done and Failed must not look identical.
        assert_ne!(
            stage_icon(&t, StageStatus::Done),
            stage_icon(&t, StageStatus::Failed)
        );
    }
}
