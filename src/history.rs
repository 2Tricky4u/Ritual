use std::path::Path;

use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Summary of one agent run, written next to the raw `.jsonl` archive.
/// Everything defaulted/optional: old or partial meta files must still load.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RunMeta {
    #[serde(default)]
    pub run_id: String,
    #[serde(default)]
    pub stage: String,
    #[serde(default)]
    pub feature: String,
    #[serde(default)]
    pub branch: String,
    #[serde(default)]
    pub agent: String,
    #[serde(default)]
    pub argv: Vec<String>,
    #[serde(default)]
    pub started_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub finished_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub exit_code: Option<i32>,
    #[serde(default)]
    pub ok: bool,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub usage: Option<Usage>,
    #[serde(default)]
    pub total_cost_usd: Option<f64>,
    #[serde(default)]
    pub num_turns: Option<u32>,
    #[serde(default)]
    pub duration_ms: Option<u64>,
    #[serde(default)]
    pub rate_limit: Option<RateLimitInfo>,
    #[serde(default)]
    pub permission_denials: Vec<serde_json::Value>,
    #[serde(default)]
    pub error: Option<String>,
    /// Reproducibility bundle (git commit, tool versions, skill hashes).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repro: Option<crate::provenance::ReproBundle>,
    /// Tamper-evident chain link (see provenance::verify_log).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chain: Option<crate::provenance::Chain>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Usage {
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default)]
    pub cache_read_input_tokens: u64,
    #[serde(default)]
    pub cache_creation_input_tokens: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RateLimitInfo {
    #[serde(default)]
    pub resets_at: Option<i64>,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
}

/// All run metas, newest first.
pub fn load_all(runs_dir: &Path) -> Result<Vec<RunMeta>> {
    let mut out = Vec::new();
    if !runs_dir.is_dir() {
        return Ok(out);
    }
    let mut paths: Vec<_> = std::fs::read_dir(runs_dir)?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .is_some_and(|n| n.to_string_lossy().ends_with(".meta.json"))
        })
        .collect();
    paths.sort();
    paths.reverse();
    for path in paths {
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        if let Ok(meta) = serde_json::from_str::<RunMeta>(&text) {
            out.push(meta);
        }
    }
    Ok(out)
}

#[derive(Debug, Clone, Default)]
pub struct DaySummary {
    pub runs: usize,
    pub cost_usd: f64,
    pub output_tokens: u64,
    pub input_tokens: u64,
    pub cache_read: u64,
    pub latest_rate_limit: Option<RateLimitInfo>,
}

impl DaySummary {
    /// Share of today's prompt tokens served from cache.
    pub fn cache_hit_pct(&self) -> Option<f64> {
        let prompt = self.input_tokens + self.cache_read;
        (prompt > 0).then(|| 100.0 * self.cache_read as f64 / prompt as f64)
    }
}

/// Today's recorded spend for a project (budget preflights, status bar).
pub fn today_spend(runs_dir: &Path) -> f64 {
    load_all(runs_dir)
        .map(|metas| today_summary(&metas).cost_usd)
        .unwrap_or(0.0)
}

/// Rollup for "today" (UTC) plus the most recent rate-limit info seen at all.
pub fn today_summary(metas: &[RunMeta]) -> DaySummary {
    let today = Utc::now().date_naive();
    let mut s = DaySummary::default();
    for m in metas {
        if s.latest_rate_limit.is_none() {
            s.latest_rate_limit = m.rate_limit.clone();
        }
        let Some(t) = m.started_at else { continue };
        if t.date_naive() != today {
            continue;
        }
        s.runs += 1;
        s.cost_usd += m.total_cost_usd.unwrap_or(0.0);
        if let Some(u) = &m.usage {
            s.output_tokens += u.output_tokens;
            s.input_tokens += u.input_tokens;
            s.cache_read += u.cache_read_input_tokens;
        }
    }
    s
}

/// Per-stage cost rollup for `ritual costs` and the report.
#[derive(Debug, Clone, Default, Serialize)]
pub struct StageCostSummary {
    pub stage: String,
    pub runs: usize,
    pub total_usd: f64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read: u64,
    pub cache_creation: u64,
}

impl StageCostSummary {
    /// Share of prompt tokens served from cache, the cache economics gauge.
    pub fn cache_hit_pct(&self) -> Option<f64> {
        let prompt = self.input_tokens + self.cache_read;
        (prompt > 0).then(|| 100.0 * self.cache_read as f64 / prompt as f64)
    }
}

/// Which runs a cost rollup covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CostWindow {
    Today,
    Week,
    All,
}

impl CostWindow {
    pub fn label(&self) -> &'static str {
        match self {
            CostWindow::Today => "today",
            CostWindow::Week => "7 days",
            CostWindow::All => "all time",
        }
    }

    fn contains(&self, m: &RunMeta) -> bool {
        let Some(t) = m.started_at else {
            return *self == CostWindow::All;
        };
        match self {
            CostWindow::Today => t.date_naive() == Utc::now().date_naive(),
            CostWindow::Week => Utc::now().signed_duration_since(t) <= chrono::Duration::days(7),
            CostWindow::All => true,
        }
    }
}

/// Group cost + token totals by stage (sorted by spend, biggest first).
pub fn by_stage(metas: &[RunMeta], window: CostWindow) -> Vec<StageCostSummary> {
    let mut out: Vec<StageCostSummary> = Vec::new();
    for m in metas.iter().filter(|m| window.contains(m)) {
        let entry = match out.iter_mut().find(|s| s.stage == m.stage) {
            Some(e) => e,
            None => {
                out.push(StageCostSummary {
                    stage: m.stage.clone(),
                    ..Default::default()
                });
                out.last_mut().unwrap()
            }
        };
        entry.runs += 1;
        entry.total_usd += m.total_cost_usd.unwrap_or(0.0);
        if let Some(u) = &m.usage {
            entry.input_tokens += u.input_tokens;
            entry.output_tokens += u.output_tokens;
            entry.cache_read += u.cache_read_input_tokens;
            entry.cache_creation += u.cache_creation_input_tokens;
        }
    }
    out.sort_by(|a, b| b.total_usd.total_cmp(&a.total_usd));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_and_sorts_newest_first() {
        let tmp = tempfile::tempdir().unwrap();
        for (name, stage) in [
            ("20260710T000000Z-a.meta.json", "old"),
            ("20260711T120000Z-b.meta.json", "new"),
        ] {
            std::fs::write(
                tmp.path().join(name),
                format!(r#"{{"run_id":"{name}","stage":"{stage}"}}"#),
            )
            .unwrap();
        }
        // A raw jsonl in the same dir must be ignored.
        std::fs::write(tmp.path().join("20260711T120000Z-b.jsonl"), "{}").unwrap();
        let metas = load_all(tmp.path()).unwrap();
        assert_eq!(metas.len(), 2);
        assert_eq!(metas[0].stage, "new");
    }

    #[test]
    fn today_summary_sums_costs() {
        let now = Utc::now();
        let metas = vec![
            RunMeta {
                started_at: Some(now),
                total_cost_usd: Some(0.25),
                usage: Some(Usage {
                    output_tokens: 100,
                    ..Default::default()
                }),
                ..Default::default()
            },
            RunMeta {
                started_at: Some(now - chrono::Duration::days(2)),
                total_cost_usd: Some(9.0),
                ..Default::default()
            },
        ];
        let s = today_summary(&metas);
        assert_eq!(s.runs, 1);
        assert!((s.cost_usd - 0.25).abs() < f64::EPSILON);
        assert_eq!(s.output_tokens, 100);
    }

    #[test]
    fn week_window_boundaries_cache_pct_and_rate_limit_capture() {
        let now = Utc::now();
        let mk = |offset: chrono::Duration| RunMeta {
            stage: "dual-review".into(),
            total_cost_usd: Some(1.0),
            started_at: Some(now - offset),
            ..Default::default()
        };
        // Just inside vs just outside the 7-day window.
        let inside = mk(chrono::Duration::days(7) - chrono::Duration::seconds(5));
        let outside = mk(chrono::Duration::days(7) + chrono::Duration::seconds(60));
        let metas = vec![inside, outside];
        assert_eq!(by_stage(&metas, CostWindow::Week)[0].runs, 1);
        assert_eq!(by_stage(&metas, CostWindow::All)[0].runs, 2);

        // DaySummary cache-hit rate over today's runs.
        let today = RunMeta {
            started_at: Some(now),
            usage: Some(Usage {
                input_tokens: 100,
                output_tokens: 5,
                cache_read_input_tokens: 900,
                cache_creation_input_tokens: 0,
            }),
            ..Default::default()
        };
        let s = today_summary(&[today]);
        assert_eq!(s.cache_hit_pct().unwrap().round(), 90.0);
        assert!(today_summary(&[]).cache_hit_pct().is_none());

        // latest_rate_limit: the FIRST one seen in (newest-first) order wins,
        // scanning past metas that carry none.
        let none = RunMeta {
            started_at: Some(now),
            ..Default::default()
        };
        let with = |kind: &str| RunMeta {
            started_at: Some(now),
            rate_limit: Some(RateLimitInfo {
                kind: Some(kind.into()),
                ..Default::default()
            }),
            ..Default::default()
        };
        let s = today_summary(&[none, with("newest"), with("older")]);
        assert_eq!(s.latest_rate_limit.unwrap().kind.as_deref(), Some("newest"));
    }

    #[test]
    fn by_stage_rolls_up_costs_and_cache() {
        let mk = |stage: &str, cost: f64, input: u64, cache: u64| RunMeta {
            stage: stage.into(),
            total_cost_usd: Some(cost),
            started_at: Some(Utc::now()),
            usage: Some(Usage {
                input_tokens: input,
                output_tokens: 10,
                cache_read_input_tokens: cache,
                cache_creation_input_tokens: 1,
            }),
            ..Default::default()
        };
        let metas = vec![
            mk("dual-review", 2.0, 100, 900),
            mk("dual-review", 1.0, 100, 900),
            mk("plan-review", 0.5, 50, 0),
        ];
        let rows = by_stage(&metas, CostWindow::All);
        assert_eq!(rows[0].stage, "dual-review", "biggest spend first");
        assert_eq!(rows[0].runs, 2);
        assert!((rows[0].total_usd - 3.0).abs() < 1e-9);
        assert_eq!(rows[0].cache_read, 1800);
        assert_eq!(rows[0].cache_hit_pct().unwrap().round(), 90.0);
        assert_eq!(rows[1].cache_hit_pct().unwrap(), 0.0);

        // No usage at all -> no cache gauge, not a panic.
        let bare = vec![RunMeta {
            stage: "x".into(),
            ..Default::default()
        }];
        assert!(
            by_stage(&bare, CostWindow::All)[0]
                .cache_hit_pct()
                .is_none()
        );
    }

    #[test]
    fn cost_windows_filter_by_started_at() {
        let now = Utc::now();
        let mut old = RunMeta {
            stage: "dual-review".into(),
            total_cost_usd: Some(5.0),
            started_at: Some(now - chrono::Duration::days(30)),
            ..Default::default()
        };
        let fresh = RunMeta {
            stage: "dual-review".into(),
            total_cost_usd: Some(1.0),
            started_at: Some(now),
            ..Default::default()
        };
        let metas = vec![old.clone(), fresh];
        assert_eq!(by_stage(&metas, CostWindow::Today)[0].runs, 1);
        assert_eq!(by_stage(&metas, CostWindow::Week)[0].runs, 1);
        assert_eq!(by_stage(&metas, CostWindow::All)[0].runs, 2);

        // A meta with no timestamp only counts toward the all-time window.
        old.started_at = None;
        let metas = vec![old];
        assert!(by_stage(&metas, CostWindow::Today).is_empty());
        assert_eq!(by_stage(&metas, CostWindow::All)[0].runs, 1);
    }
}
