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
    /// Regenerate .ritual/lessons.md — review memory from finding
    /// dispositions (dismissed = known noise, fixed = real-bug areas)
    Lessons {
        /// Print the generated markdown instead of only writing the file
        #[arg(long)]
        stdout: bool,
    },
    /// Mutation-kill gate: mutate the diff, run the tests, and record every
    /// SURVIVING mutant as a major finding (a test gap)
    Mutants {
        /// Base ref to diff against (defaults to base_ref from config)
        #[arg(long)]
        base: Option<String>,
    },
    /// Scan changed files (tracked + untracked) for leaked secrets via
    /// gitleaks; hits become critical findings and exit nonzero
    Secrets,
    /// Per-stage cost analytics: today / 7 days / all time, cache-hit rates,
    /// daily-budget gauge
    Costs {
        /// Emit machine-readable JSON (per-stage rollups per window)
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
        /// Model override for this one run (beats the [models] routing table)
        #[arg(long)]
        model: Option<String>,
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
        /// Emit IETF draft-sharif-agent-audit-trail records (JCS-canonical,
        /// SHA-256 hash-chained JSONL) instead of OTLP spans
        #[arg(long)]
        audit_trail: bool,
    },
    /// Check every workflow prerequisite (agents, auth, MCP, skills, hooks,
    /// check.sh, disk); exits nonzero on hard failures
    Doctor {
        /// Also run `./check.sh fast`
        #[arg(long)]
        deep: bool,
    },
    /// List live detached runs (pipeline stages and chat edits)
    Ps,
    /// Follow a live detached run from this terminal (or --kill it)
    Attach {
        /// Run id (see `ritual ps`)
        run_id: String,
        /// SIGTERM the run's process group instead of following it
        #[arg(long)]
        kill: bool,
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
    /// Post the latest dual-review findings to a GitHub PR (via gh)
    PrComment {
        /// PR number (defaults to the PR for the current branch)
        pr: Option<u32>,
        /// Also attempt per-finding inline review comments (best-effort)
        #[arg(long)]
        inline: bool,
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
