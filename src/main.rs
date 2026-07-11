mod cli;
mod config;
mod findings;
mod history;
mod output;
mod scaffold;
mod state;
mod theme;

use anyhow::Result;
use clap::Parser;

use crate::cli::{Cli, Command};
use crate::config::Config;
use crate::state::{RitualDirs, State};

fn main() -> Result<()> {
    let cli = Cli::parse();
    let cwd = std::env::current_dir()?;
    let dirs = RitualDirs::discover(&cwd);
    let cfg = Config::load(&dirs.project_root, cli.theme.as_deref(), cli.ascii)?;

    match cli.command {
        Some(Command::Init { force }) => {
            let report = scaffold::init(&dirs.project_root, force)?;
            output::render_init(&cfg, &report);
        }
        Some(Command::Status) | None => {
            let state = State::load(&dirs)?;
            let features: Vec<_> = state
                .features
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            let branch = state::current_branch(&dirs.project_root);
            output::render_status(&cfg, &features, branch.as_deref());
        }
        Some(Command::Findings { json }) => {
            let loaded = findings::load_all(&dirs.findings_dir())?;
            output::render_findings(&cfg, &loaded, json);
        }
        Some(Command::History { limit }) => {
            let metas = history::load_all(&dirs.runs_dir())?;
            let summary = history::today_summary(&metas);
            output::render_history(&cfg, &metas, &summary, limit);
        }
        Some(Command::Run { stage, arg }) => {
            anyhow::bail!(
                "`ritual run {stage}{}` lands in M2 (runner milestone)",
                arg.map(|a| format!(" {a}")).unwrap_or_default()
            );
        }
        Some(Command::New { title }) => {
            let title = title.join(" ");
            anyhow::ensure!(!title.is_empty(), "usage: ritual new <title>");
            anyhow::ensure!(dirs.exists(), "no .ritual/ here — run `ritual init` first");
            let branch =
                state::current_branch(&dirs.project_root).unwrap_or_else(|| "detached".to_string());
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
