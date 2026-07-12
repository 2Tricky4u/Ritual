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
        /// Overwrite an existing check.sh (with --skills: overwrite locally
        /// modified skills/hooks too)
        #[arg(long)]
        force: bool,
        /// Also install the vendored workbench (skills, code-reviewer agent,
        /// hooks) into ~/.claude
        #[arg(long)]
        skills: bool,
    },
    /// Browse recorded findings
    Findings {
        /// Emit raw JSON instead of styled output
        #[arg(long)]
        json: bool,
        /// Also show findings already marked fixed/dismissed
        #[arg(long)]
        all: bool,
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
    /// Run a headless stage N times and score the results (model/prompt comparison)
    Bench {
        /// Stage: plan-review | dual-review
        stage: String,
        /// Number of repetitions
        #[arg(long, default_value_t = 3)]
        runs: usize,
        /// JSON array of expected finding titles to score recall against
        #[arg(long)]
        golden: Option<std::path::PathBuf>,
    },
    /// Export run history as OTLP-shaped JSON lines (one span per run)
    Export {
        /// Write to a file instead of stdout
        #[arg(long)]
        out: Option<std::path::PathBuf>,
    },
    /// Prune old run artifacts (protects live, state-referenced, and today's
    /// runs; chained runs are covered by a tamper-evident checkpoint)
    Clean {
        /// How many recent finished runs to keep (protected runs don't count)
        #[arg(long, default_value_t = 50)]
        keep: usize,
        /// Print what would be deleted/kept without touching anything
        #[arg(long)]
        dry_run: bool,
    },
    /// Chat one message to Claude to author/edit this feature's spec (or plan)
    Chat {
        /// The instruction, e.g. `ritual chat "tighten the goal to one sentence"`
        message: Vec<String>,
        /// Edit plan.md instead of spec.md
        #[arg(long)]
        plan: bool,
        /// Confine the edit to one `##` section (by its heading text)
        #[arg(long)]
        section: Option<String>,
        /// Override the daily budget ceiling for this run
        #[arg(long)]
        force: bool,
    },
    /// Internal: detached run executor (do not invoke by hand)
    #[command(name = "_spawn", hide = true)]
    InternalSpawn { run_id: String },
}
