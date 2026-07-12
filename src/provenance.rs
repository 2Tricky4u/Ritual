//! Run provenance: reproducibility bundles (what exactly ran) and a
//! tamper-evident hash chain over the run archive (21 CFR Part 11-style
//! append-only audit trail, minus the ceremony).

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::config::Config;
use crate::history::RunMeta;
use crate::state::RitualDirs;

pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    to_hex(&h.finalize())
}

fn to_hex(digest: &[u8]) -> String {
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// Everything needed to answer "what exactly produced this run?".
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ReproBundle {
    #[serde(default)]
    pub git_commit: Option<String>,
    #[serde(default)]
    pub git_dirty_diff_sha256: Option<String>,
    #[serde(default)]
    pub claude_version: Option<String>,
    #[serde(default)]
    pub codex_version: Option<String>,
    /// skill name -> sha256 of its SKILL.md
    #[serde(default)]
    pub skill_hashes: BTreeMap<String, String>,
    #[serde(default)]
    pub config_snapshot: BTreeMap<String, String>,
}

fn cmd_line(bin: &str, args: &[&str], cwd: &Path) -> Option<String> {
    let out = std::process::Command::new(bin)
        .args(args)
        .current_dir(cwd)
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Best-effort collection — a missing tool yields None, never an error.
pub fn collect(cfg: &Config, dirs: &RitualDirs) -> ReproBundle {
    let root = &dirs.work_root;
    let git_commit = cmd_line("git", &["rev-parse", "HEAD"], root);
    let git_dirty_diff_sha256 = cmd_line("git", &["diff", "HEAD"], root)
        .filter(|d| !d.is_empty())
        .map(|d| sha256_hex(d.as_bytes()));
    let claude_version = cmd_line(&cfg.claude_cmd[0], &["--version"], root);
    let codex_version = cmd_line(&cfg.codex_cmd[0], &["--version"], root);

    let mut skill_hashes = BTreeMap::new();
    if let Some(home) = dirs::home_dir() {
        for skill in ["plan-review", "tdd", "dual-review", "spec"] {
            let p = home.join(format!(".claude/skills/{skill}/SKILL.md"));
            if let Ok(bytes) = std::fs::read(&p) {
                skill_hashes.insert(skill.to_string(), sha256_hex(&bytes));
            }
        }
    }

    let mut config_snapshot = BTreeMap::new();
    config_snapshot.insert("base_ref".into(), cfg.base_ref.clone());
    config_snapshot.insert("redaction".into(), cfg.redaction.to_string());
    config_snapshot.insert(
        "budget_plan_review_usd".into(),
        cfg.budget_plan_review_usd.to_string(),
    );
    config_snapshot.insert(
        "budget_dual_review_usd".into(),
        cfg.budget_dual_review_usd.to_string(),
    );
    for (stage, model) in &cfg.models {
        config_snapshot.insert(format!("model.{stage}"), model.clone());
    }

    ReproBundle {
        git_commit,
        git_dirty_diff_sha256,
        claude_version,
        codex_version,
        skill_hashes,
        config_snapshot,
    }
}

/// Chain entry stored in each run meta.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Chain {
    pub prev: String,
    pub this: String,
}

pub const GENESIS: &str = "0000000000000000000000000000000000000000000000000000000000000000";

/// this = sha256(prev ‖ sha256(archive bytes) ‖ canonical(meta minus chain)).
pub fn compute_link(prev: &str, archive_bytes: &[u8], meta: &RunMeta) -> Result<Chain> {
    let mut unchained = meta.clone();
    unchained.chain = None;
    let canonical = serde_json::to_vec(&unchained).context("serializing meta for chain")?;
    let mut h = Sha256::new();
    h.update(prev.as_bytes());
    h.update(sha256_hex(archive_bytes).as_bytes());
    h.update(&canonical);
    Ok(Chain {
        prev: prev.to_string(),
        this: to_hex(&h.finalize()),
    })
}

/// The `this` hash of the newest chained run, or GENESIS.
pub fn last_link(runs_dir: &Path) -> String {
    crate::history::load_all(runs_dir)
        .ok()
        .and_then(|metas| metas.into_iter().find_map(|m| m.chain.map(|c| c.this)))
        .unwrap_or_else(|| GENESIS.to_string())
}

#[derive(Debug, PartialEq)]
pub enum VerifyOutcome {
    Ok { runs: usize },
    Broken { run_id: String, reason: String },
}

/// Walk the chain oldest→newest, recomputing every link.
pub fn verify_log(runs_dir: &Path) -> Result<VerifyOutcome> {
    let mut metas = crate::history::load_all(runs_dir)?;
    metas.reverse(); // load_all is newest-first
    let chained: Vec<&RunMeta> = metas.iter().filter(|m| m.chain.is_some()).collect();
    let mut prev = GENESIS.to_string();
    for meta in &chained {
        let chain = meta.chain.as_ref().unwrap();
        if chain.prev != prev {
            return Ok(VerifyOutcome::Broken {
                run_id: meta.run_id.clone(),
                reason: format!("prev-link mismatch (expected {prev}, found {})", chain.prev),
            });
        }
        let archive = runs_dir.join(format!("{}.jsonl", meta.run_id));
        let bytes = std::fs::read(&archive).unwrap_or_default();
        let expected = compute_link(&prev, &bytes, meta)?;
        if expected.this != chain.this {
            return Ok(VerifyOutcome::Broken {
                run_id: meta.run_id.clone(),
                reason: "content hash mismatch (archive or meta was modified)".into(),
            });
        }
        prev = chain.this.clone();
    }
    Ok(VerifyOutcome::Ok {
        runs: chained.len(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mk_run(dir: &Path, run_id: &str, prev: &str) -> Chain {
        let archive = dir.join(format!("{run_id}.jsonl"));
        std::fs::write(&archive, format!("line-of-{run_id}\n")).unwrap();
        let mut meta = RunMeta {
            run_id: run_id.into(),
            stage: "test".into(),
            ok: true,
            ..Default::default()
        };
        let chain = compute_link(prev, &std::fs::read(&archive).unwrap(), &meta).unwrap();
        meta.chain = Some(chain.clone());
        std::fs::write(
            dir.join(format!("{run_id}.meta.json")),
            serde_json::to_string_pretty(&meta).unwrap(),
        )
        .unwrap();
        chain
    }

    #[test]
    fn chain_verifies_and_detects_tamper() {
        let tmp = tempfile::tempdir().unwrap();
        let c1 = mk_run(tmp.path(), "20260711T000001Z-a", GENESIS);
        let c2 = mk_run(tmp.path(), "20260711T000002Z-b", &c1.this);
        assert_eq!(last_link(tmp.path()), c2.this);
        assert_eq!(
            verify_log(tmp.path()).unwrap(),
            VerifyOutcome::Ok { runs: 2 }
        );

        // Tamper with the first archive: verification must break at run a.
        std::fs::write(tmp.path().join("20260711T000001Z-a.jsonl"), "edited!\n").unwrap();
        match verify_log(tmp.path()).unwrap() {
            VerifyOutcome::Broken { run_id, .. } => assert!(run_id.ends_with("-a")),
            other => panic!("expected broken chain, got {other:?}"),
        }
    }

    #[test]
    fn collect_is_best_effort() {
        let tmp = tempfile::tempdir().unwrap();
        let dirs = RitualDirs::new(tmp.path());
        let cfg = Config {
            claude_cmd: vec!["/nonexistent/claude".into()],
            codex_cmd: vec!["/nonexistent/codex".into()],
            ..Default::default()
        };
        let b = collect(&cfg, &dirs);
        assert!(b.claude_version.is_none());
        assert!(b.config_snapshot.contains_key("redaction"));
    }
}
