use anyhow::Result;
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
            rt.block_on(ui::app::run(cfg, dirs))?;
        }
        Some(Command::Init { force }) => {
            let report = scaffold::init(&dirs.project_root, force)?;
            output::render_init(&cfg, &report);
        }
        Some(Command::Status) => {
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
            run_cmd::execute(&cfg, &dirs, &stage, arg.as_deref())?;
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
