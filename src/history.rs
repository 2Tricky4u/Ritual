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
    pub latest_rate_limit: Option<RateLimitInfo>,
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
        }
    }
    s
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
}
