use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(
    name = "ritual",
    version,
    about = "A TUI for the multi-LLM coding workflow"
)]
pub struct Cli {
    /// Theme: eldritch (default) or tokyonight
    #[arg(long, global = true)]
    pub theme: Option<String>,

    /// Use plain ASCII icons instead of Nerd Font glyphs
    #[arg(long, global = true)]
    pub ascii: bool,

    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Show the pipeline status for this project
    Status {
        /// Emit machine-readable JSON instead of styled output
        #[arg(long)]
        json: bool,
    },
    /// Scaffold .ritual/, check.sh and CLAUDE.md in this project
    Init {
        /// Overwrite an existing check.sh
        #[arg(long)]
        force: bool,
    },
    /// Browse recorded findings
    Findings {
        /// Emit raw JSON instead of styled output
        #[arg(long)]
        json: bool,
    },
    /// Show past agent runs (tokens, cost, duration)
    History {
        /// Max runs to display
        #[arg(long, default_value_t = 20)]
        limit: usize,
        /// Emit machine-readable JSON instead of styled output
        #[arg(long)]
        json: bool,
    },
    /// Run a pipeline stage (plan-review, dual-review, ...)
    Run {
        /// Stage name: spec | plan | plan-review | tests-red | implement | dual-review
        stage: String,
        /// Stage argument (plan path for plan-review, base ref for dual-review)
        arg: Option<String>,
        /// Override the daily budget ceiling for this run
        #[arg(long)]
        force: bool,
        /// CI mode: emit JUnit XML to .ritual/ci/, exit nonzero on blocking findings
        #[arg(long)]
        ci: bool,
    },
    /// Generate a Markdown report for a feature (spec, plan, findings, runs, costs)
    Report {
        /// Feature branch/slug (defaults to the current branch)
        feature: Option<String>,
        /// Also convert to PDF via pandoc
        #[arg(long)]
        pdf: bool,
    },
    /// Create/rename the feature for the current branch
    New {
        /// Feature title
        title: Vec<String>,
        /// Create a git worktree + branch for this feature (parallel work)
        #[arg(long, value_name = "BRANCH")]
        worktree: Option<String>,
    },
    /// Show the reproducibility bundle of a run and diff it against the current environment
    Repro {
        /// Run id (see `ritual history`)
        run_id: String,
    },
    /// Verify the tamper-evident hash chain over all recorded runs
    VerifyLog,
}
