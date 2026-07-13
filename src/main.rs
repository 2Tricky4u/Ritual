use anyhow::{Context, Result};
use clap::Parser;

use ritual::cli::{Cli, Command};
use ritual::config::Config;
use ritual::state::{self, RitualDirs, State};
use ritual::{findings, history, output, run_cmd, scaffold, ui};

fn main() -> Result<()> {
    let cli = Cli::parse();
    let cwd = std::env::current_dir()?;
    let dirs = RitualDirs::discover(&cwd);
    let cfg = Config::load(&dirs.project_root, cli.theme.as_deref(), cli.ascii)?;

    match cli.command {
        None => {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()?;
            rt.block_on(ui::app::run(cfg, dirs, cli.theme.clone(), cli.ascii))?;
        }
        Some(Command::Init { force, skills }) => {
            let report = scaffold::init(&dirs.project_root, force)?;
            output::render_init(&cfg, &report);
            if skills {
                let home = match std::env::var_os("RITUAL_CLAUDE_HOME") {
                    Some(h) => std::path::PathBuf::from(h), // test seam
                    None => dirs::home_dir()
                        .context("no home directory")?
                        .join(".claude"),
                };
                let r = ritual::workbench::install(&home, force)?;
                println!(
                    "workbench → {}: {} created, {} updated, {} unchanged",
                    home.display(),
                    r.created.len(),
                    r.updated.len(),
                    r.identical.len()
                );
                for s in &r.skipped {
                    println!("  skipped {s} (local file differs, --force to overwrite)");
                }
                if !r.created.is_empty() || !r.updated.is_empty() {
                    println!(
                        "  settings.json blocks are NOT auto-merged; see workbench/settings-snippet.json"
                    );
                }
            }
        }
        Some(Command::Status { json }) => {
            let state = State::load(&dirs)?;
            let branch = state::current_branch(&dirs.work_root);
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "current_branch": branch,
                        "features": state.features,
                    }))?
                );
            } else {
                let features: Vec<_> = state
                    .features
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect();
                output::render_status(&cfg, &features, branch.as_deref());
            }
        }
        Some(Command::Findings { json, all }) => {
            let loaded = findings::load_all(&dirs.findings_dir())?;
            output::render_findings(&cfg, &loaded, json, all);
            // Scriptability contract: UNRESOLVED confirmed critical findings
            // -> exit 1. A human marking one fixed/dismissed unblocks it.
            let blocking = loaded.iter().flat_map(|l| &l.file.findings).any(|f| {
                f.severity == findings::Severity::Critical
                    && f.verdict == "confirmed"
                    && !f.resolved()
            });
            if blocking {
                std::process::exit(1);
            }
        }
        Some(Command::Lessons { stdout }) => match ritual::lessons::refresh(&dirs)? {
            Some(path) => {
                if stdout {
                    print!("{}", std::fs::read_to_string(&path)?);
                } else {
                    println!("lessons → {}", path.display());
                }
            }
            None => {
                println!("no dispositions yet. Mark findings fixed (f) or dismissed (d) first")
            }
        },
        Some(Command::Mutants { base }) => {
            let r = ritual::mutants::run(&cfg, &dirs, base.as_deref())?;
            if r.empty_diff {
                println!(
                    "no diff against {}, nothing to mutate",
                    base.as_deref().unwrap_or(&cfg.base_ref)
                );
            } else {
                println!(
                    "mutants: {} caught, {} missed, {} unviable, {} timeout",
                    r.caught, r.missed, r.unviable, r.timeout
                );
                match r.findings_path {
                    Some(p) => println!(
                        "{} surviving mutant(s) → {}: test gaps; review with `ritual findings` or the TUI (f/d)",
                        r.missed,
                        p.display()
                    ),
                    None => println!(
                        "no surviving mutants: the tests discriminate every mutation in the diff"
                    ),
                }
            }
        }
        Some(Command::Secrets) => {
            anyhow::ensure!(
                ritual::secrets::available(&cfg),
                "`{}` not runnable; install gitleaks (pacman -S gitleaks)",
                cfg.gitleaks_cmd.join(" ")
            );
            let r = ritual::secrets::scan(&cfg, &dirs)?;
            if r.scanned_files == 0 {
                println!("no changed files to scan");
            } else if r.leaks == 0 {
                println!(
                    "{} changed file(s) scanned, no secrets found",
                    r.scanned_files
                );
            } else {
                println!(
                    "{} leak(s) in {} changed file(s) → {}",
                    r.leaks,
                    r.scanned_files,
                    r.findings_path
                        .as_deref()
                        .map(|p| p.display().to_string())
                        .unwrap_or_default()
                );
                println!(
                    "critical findings block CI until dismissed (d) or fingerprinted in .gitleaksignore"
                );
                std::process::exit(1);
            }
        }
        Some(Command::Skills { cmd }) => match cmd {
            ritual::cli::SkillsCmd::Diff => {
                let home = match std::env::var_os("RITUAL_CLAUDE_HOME") {
                    Some(h) => std::path::PathBuf::from(h), // test seam
                    None => dirs::home_dir()
                        .context("no home directory")?
                        .join(".claude"),
                };
                let mut divergent = 0usize;
                for (name, status) in ritual::workbench::diff(&home) {
                    use ritual::workbench::SkillDiff;
                    match status {
                        SkillDiff::Identical => println!("  {name:<14} identical"),
                        SkillDiff::Missing => {
                            divergent += 1;
                            println!("  {name:<14} MISSING (ritual init --skills installs it)");
                        }
                        SkillDiff::Differs {
                            repo_lines,
                            installed_lines,
                            first: (line, repo, installed),
                        } => {
                            divergent += 1;
                            println!(
                                "  {name:<14} differs at line {line} (repo {repo_lines} lines, installed {installed_lines})"
                            );
                            println!("    repo:      {repo}");
                            println!("    installed: {installed}");
                        }
                    }
                }
                if divergent > 0 {
                    println!(
                        "\n{divergent} divergent; `ritual init --skills --force` pushes repo → {}",
                        home.display()
                    );
                }
            }
        },
        Some(Command::Costs { json }) => {
            let metas = history::load_all(&dirs.runs_dir())?;
            if json {
                let val = serde_json::json!({
                    "today": history::by_stage(&metas, history::CostWindow::Today),
                    "week": history::by_stage(&metas, history::CostWindow::Week),
                    "all_time": history::by_stage(&metas, history::CostWindow::All),
                });
                println!("{}", serde_json::to_string_pretty(&val)?);
            } else {
                output::render_costs(&cfg, &metas);
            }
        }
        Some(Command::History { limit, json }) => {
            let metas = history::load_all(&dirs.runs_dir())?;
            if json {
                let capped: Vec<_> = metas.iter().take(limit).collect();
                println!("{}", serde_json::to_string_pretty(&capped)?);
            } else {
                let summary = history::today_summary(&metas);
                output::render_history(&cfg, &metas, &summary, limit);
            }
        }
        Some(Command::Run {
            stage,
            arg,
            force,
            ci,
            model,
        }) => {
            run_cmd::execute(
                &cfg,
                &dirs,
                &stage,
                arg.as_deref(),
                force,
                ci,
                model.as_deref(),
            )?;
        }
        Some(Command::Repro { run_id }) => {
            let metas = history::load_all(&dirs.runs_dir())?;
            let meta = metas
                .iter()
                .find(|m| m.run_id == run_id)
                .with_context(|| format!("no run '{run_id}'; see `ritual history`"))?;
            let recorded = meta.repro.clone().unwrap_or_default();
            println!("{}", serde_json::to_string_pretty(&recorded)?);
            let current = ritual::provenance::collect(&cfg, &dirs);
            if current == recorded {
                println!("\nenvironment matches the recorded bundle");
            } else {
                println!("\nDIFFERS from current environment:");
                if current.git_commit != recorded.git_commit {
                    println!(
                        "  git_commit: {:?} -> {:?}",
                        recorded.git_commit, current.git_commit
                    );
                }
                if current.claude_version != recorded.claude_version {
                    println!(
                        "  claude: {:?} -> {:?}",
                        recorded.claude_version, current.claude_version
                    );
                }
                if current.codex_version != recorded.codex_version {
                    println!(
                        "  codex: {:?} -> {:?}",
                        recorded.codex_version, current.codex_version
                    );
                }
                if current.skill_hashes != recorded.skill_hashes {
                    println!("  skill files changed");
                }
                if current.config_snapshot != recorded.config_snapshot {
                    println!("  config changed");
                }
            }
        }
        Some(Command::Bench {
            stage,
            runs,
            golden,
        }) => {
            ritual::bench::bench(&cfg, &dirs, &stage, runs, golden.as_deref())?;
        }
        Some(Command::Export { out, audit_trail }) => {
            if audit_trail {
                ritual::export::audit_trail(&dirs, out.as_deref())?;
            } else {
                ritual::export::otlp_json(&dirs, out.as_deref())?;
            }
        }
        Some(Command::Chat {
            message,
            plan,
            section,
            force,
        }) => {
            run_cmd::run_doc_chat(&cfg, &dirs, &message.join(" "), plan, section, force)?;
        }
        Some(Command::PrComment { pr, inline }) => {
            ritual::pr_comment::pr_comment(&cfg, &dirs, pr, inline)?;
        }
        Some(Command::Doctor { deep }) => {
            let results = ritual::doctor::run(&cfg, &dirs, deep);
            output::render_doctor(&cfg, &results);
            if results
                .iter()
                .any(|r| r.status == ritual::doctor::CheckStatus::Fail)
            {
                std::process::exit(1);
            }
        }
        Some(Command::Ps) => {
            run_cmd::ps(&dirs)?;
        }
        Some(Command::Attach { run_id, kill }) => {
            run_cmd::attach(&cfg, &dirs, &run_id, kill)?;
        }
        Some(Command::Clean { keep, dry_run }) => {
            let report = ritual::clean::clean(&dirs, keep, dry_run)?;
            output::render_clean(&cfg, &report);
            if !report.failures.is_empty() {
                std::process::exit(1);
            }
        }
        Some(Command::InternalSpawn { run_id }) => {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()?;
            rt.block_on(ritual::runner::daemon_main(&dirs, &run_id))?;
        }
        Some(Command::VerifyLog) => match ritual::provenance::verify_log(&dirs.runs_dir())? {
            ritual::provenance::VerifyOutcome::Ok { runs, checkpoint } => match checkpoint {
                Some(cp) => println!(
                    "chain intact: checkpoint({}, {} pruned) + {runs} run(s) verified",
                    cp.created_at.format("%Y-%m-%d"),
                    cp.pruned_runs
                ),
                None => println!("chain intact: {runs} chained run(s) verified"),
            },
            ritual::provenance::VerifyOutcome::Broken { run_id, reason } => {
                eprintln!("CHAIN BROKEN at {run_id}: {reason}");
                std::process::exit(1);
            }
        },
        Some(Command::Report { feature, pdf }) => {
            let out = ritual::report::generate(&cfg, &dirs, feature.as_deref(), pdf)?;
            println!("report: {}", out.markdown.display());
            if let Some(p) = out.pdf {
                println!("pdf:    {}", p.display());
            }
        }
        Some(Command::New { title, worktree }) => {
            let title = title.join(" ");
            anyhow::ensure!(!title.is_empty(), "usage: ritual new <title>");
            anyhow::ensure!(dirs.exists(), "no .ritual/ here, run `ritual init` first");
            let branch = match &worktree {
                Some(branch) => {
                    // Parallel feature: own worktree, shared .ritual state.
                    let slug = state::branch_slug(branch);
                    let name = dirs
                        .project_root
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_else(|| "repo".into());
                    let wt_path = dirs
                        .project_root
                        .parent()
                        .map(|p| p.join(format!("{name}-{slug}")))
                        .context("project root has no parent for a worktree")?;
                    let status = std::process::Command::new("git")
                        .args(["worktree", "add", "-b", branch])
                        .arg(&wt_path)
                        .current_dir(&dirs.project_root)
                        .status()
                        .context("running git worktree add")?;
                    anyhow::ensure!(status.success(), "git worktree add failed");
                    println!("worktree: {}", wt_path.display());
                    branch.clone()
                }
                None => {
                    state::current_branch(&dirs.work_root).unwrap_or_else(|| "detached".to_string())
                }
            };
            let mut state = State::load(&dirs)?;
            let feature = state.feature_for_branch_mut(&branch);
            feature.title = title.clone();
            feature.updated_at = chrono::Utc::now();
            let slug = state::branch_slug(&branch);
            state.save(&dirs)?;

            // Seed the spec file from the template.
            let spec = dirs.spec_file(&slug);
            if !spec.exists() {
                std::fs::create_dir_all(dirs.feature_dir(&slug))?;
                std::fs::write(&spec, scaffold::SPEC_TEMPLATE.replace("<title>", &title))?;
            }
            println!(
                "feature '{title}' ready on branch '{branch}' (spec: {})",
                spec.display()
            );
        }
    }
    Ok(())
}
