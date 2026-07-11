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
    Status,
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
    },
    /// Run a pipeline stage (plan-review, dual-review, ...)
    Run {
        /// Stage name: spec | plan | plan-review | tests-red | implement | dual-review
        stage: String,
        /// Stage argument (plan path for plan-review, base ref for dual-review)
        arg: Option<String>,
    },
    /// Create/rename the feature for the current branch
    New {
        /// Feature title
        title: Vec<String>,
    },
}
