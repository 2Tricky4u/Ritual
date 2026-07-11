//! `ritual bench` — run a stage N times and score the results, aider-style:
//! repeatable quality measurement for comparing models/prompts/skills.
//! Works token-free against the fake agent via RITUAL_CLAUDE_CMD.

use std::path::Path;

use anyhow::{Context, Result};
use tokio::sync::mpsc;

use crate::config::Config;
use crate::runner::{self, RunRequest};
use crate::stages::{self, Mode};
use crate::state::{self, RitualDirs, StageId, State};

#[derive(Debug, Default)]
struct RunScore {
    ok: bool,
    findings: usize,
    cross_confirmed: usize,
    matched_golden: usize,
    cost_usd: f64,
    duration_s: f64,
}

pub fn bench(
    cfg: &Config,
    dirs: &RitualDirs,
    stage_str: &str,
    runs: usize,
    golden: Option<&Path>,
) -> Result<()> {
    let stage =
        StageId::parse(stage_str).with_context(|| format!("unknown stage '{stage_str}'"))?;
    anyhow::ensure!(dirs.exists(), "no .ritual/ here — run `ritual init` first");

    // Golden file: JSON array of expected finding titles (substring match).
    let golden_titles: Vec<String> = match golden {
        Some(p) => serde_json::from_str(&std::fs::read_to_string(p)?)
            .context("golden file must be a JSON array of expected finding titles")?,
        None => Vec::new(),
    };

    let branch = state::current_branch(&dirs.work_root).unwrap_or_else(|| "detached".into());
    let slug = state::branch_slug(&branch);
    let st = State::load(dirs)?;
    let title = st
        .features
        .get(&slug)
        .map(|f| f.title.clone())
        .unwrap_or_default();

    let rt = tokio::runtime::Runtime::new()?;
    let mut scores = Vec::new();

    for i in 1..=runs {
        let cmd = stages::build(stage, cfg, dirs, &slug, None)?;
        anyhow::ensure!(
            cmd.mode == Mode::Headless,
            "bench only supports headless stages (plan-review, dual-review)"
        );
        let findings_before = list(&dirs.findings_dir());
        let req = RunRequest {
            agent: cmd.agent,
            argv: cmd.argv,
            env: cmd.env,
            stage: stage.label().into(),
            feature: title.clone(),
            branch: branch.clone(),
            redact: cfg.redaction,
            repro: None, // benched runs skip provenance probes for speed
            cwd: dirs.work_root.clone(),
        };
        let run_id = runner::new_run_id(&format!("bench{i}-{}", stage.label()));
        runner::spawn_detached(dirs, &req, &run_id)?;
        eprint!("run {i}/{runs}… ");

        let outcome = rt.block_on(async {
            let (tx, mut rx) = mpsc::channel(64);
            let dirs2 = dirs.clone();
            let rid = run_id.clone();
            let agent = req.agent;
            let handle =
                tokio::spawn(async move { runner::tail_run(&dirs2, agent, &rid, tx).await });
            while rx.recv().await.is_some() {} // drain quietly
            handle.await?
        })?;

        // Score the findings this run produced.
        let new_files: Vec<String> = list(&dirs.findings_dir())
            .into_iter()
            .filter(|f| !findings_before.contains(f))
            .collect();
        let mut score = RunScore {
            ok: outcome.meta.ok,
            cost_usd: outcome.meta.total_cost_usd.unwrap_or(0.0),
            duration_s: outcome.meta.duration_ms.unwrap_or(0) as f64 / 1000.0,
            ..Default::default()
        };
        for name in &new_files {
            let Ok(text) = std::fs::read_to_string(dirs.findings_dir().join(name)) else {
                continue;
            };
            let Ok(file) = serde_json::from_str::<crate::findings::FindingsFile>(&text) else {
                continue;
            };
            for f in &file.findings {
                score.findings += 1;
                if f.cross_confirmed() {
                    score.cross_confirmed += 1;
                }
                if golden_titles
                    .iter()
                    .any(|g| f.title.to_lowercase().contains(&g.to_lowercase()))
                {
                    score.matched_golden += 1;
                }
            }
        }
        eprintln!(
            "{} — {} findings",
            if score.ok { "ok" } else { "FAILED" },
            score.findings
        );
        scores.push(score);
    }

    // Markdown comparison table.
    println!("\n## bench: {} × {} run(s)\n", stage.label(), runs);
    println!("| run | ok | findings | cross-confirmed | golden hits | cost | duration |");
    println!("|---|---|---|---|---|---|---|");
    for (i, s) in scores.iter().enumerate() {
        println!(
            "| {} | {} | {} | {} | {} | ${:.3} | {:.1}s |",
            i + 1,
            if s.ok { "yes" } else { "NO" },
            s.findings,
            s.cross_confirmed,
            if golden_titles.is_empty() {
                "-".to_string()
            } else {
                format!("{}/{}", s.matched_golden, golden_titles.len())
            },
            s.cost_usd,
            s.duration_s,
        );
    }
    let ok_rate = scores.iter().filter(|s| s.ok).count() as f64 / runs.max(1) as f64;
    let avg_findings = scores.iter().map(|s| s.findings).sum::<usize>() as f64 / runs.max(1) as f64;
    let total_cost: f64 = scores.iter().map(|s| s.cost_usd).sum();
    println!(
        "\nok-rate {:.0}% · avg findings {:.1} · total cost ${:.2}",
        ok_rate * 100.0,
        avg_findings,
        total_cost
    );
    if !golden_titles.is_empty() {
        let avg_recall = scores
            .iter()
            .map(|s| s.matched_golden as f64 / golden_titles.len() as f64)
            .sum::<f64>()
            / runs.max(1) as f64;
        println!("golden recall {:.0}%", avg_recall * 100.0);
    }
    Ok(())
}

fn list(dir: &Path) -> Vec<String> {
    std::fs::read_dir(dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default()
}
