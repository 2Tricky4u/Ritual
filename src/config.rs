use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::keymap::Keymap;
use crate::theme::{IconSet, Theme};

/// Layered config: defaults <- ~/.config/ritual/config.toml
/// <- .ritual/config.toml <- env <- CLI flags.
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct FileConfig {
    pub theme: Option<String>,
    pub icons: Option<String>, // "nerd" | "ascii"
    pub base_ref: Option<String>,
    pub claude_cmd: Option<String>,
    pub codex_cmd: Option<String>,
    pub budget_plan_review_usd: Option<f64>,
    pub budget_dual_review_usd: Option<f64>,
    /// Per-message ceiling for a spec/plan chat edit (one small Edit each).
    pub budget_doc_chat_usd: Option<f64>,
    /// `[keys]` table: action name -> chord ("check-full = \"F\"").
    pub keys: Option<HashMap<String, String>>,
    /// Redact secrets from archives/streams/reports (default true).
    pub redaction: Option<bool>,
    /// Daily spend ceiling across all runs in this project (USD).
    pub budget_daily_usd: Option<f64>,
    /// Desktop notifications on stage completion (default true).
    pub notifications: Option<bool>,
    /// `[models]` table: stage label -> model override ("plan-review = \"opus\"").
    pub models: Option<HashMap<String, String>>,
    /// Fallback model(s) for headless claude runs, comma-separated — retryable
    /// API errors (overload) switch instead of failing the run.
    pub fallback_model: Option<String>,
    /// Hard ceiling on any check.sh invocation (hung boards, wedged builds).
    pub check_timeout_secs: Option<u64>,
    /// Air-gapped mode: skip cloud auth preflights/probes entirely.
    pub offline: Option<bool>,
    /// Terminal background shows through the main pane (chadrc parity).
    pub transparency: Option<bool>,
    /// Explicit nvim server socket ($NVIM / XDG discovery otherwise).
    pub nvim_server: Option<String>,
    /// `[commands]` table: name -> shell template with {{branch}}, {{run_id}},
    /// {{finding.file}}, {{finding.line}} placeholders (lazygit-style).
    pub commands: Option<HashMap<String, String>>,
    /// `[consensus]` table: the optional third-model arbitration tier.
    pub consensus: Option<ConsensusFileConfig>,
    /// argv for the GitHub CLI (pr-comment), e.g. "gh".
    pub gh_cmd: Option<String>,
    /// `[mutants]` table: the mutation-kill gate (`ritual mutants`).
    pub mutants: Option<MutantsFileConfig>,
    /// `[secrets]` table: the gitleaks gate over changed files.
    pub secrets: Option<SecretsFileConfig>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct SecretsFileConfig {
    /// Auto-scan changed files before dual-review (default true; silently
    /// skipped when gitleaks isn't installed).
    pub enabled: Option<bool>,
    /// gitleaks argv override, default "gitleaks".
    pub gitleaks_cmd: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct MutantsFileConfig {
    /// Runner argv, default "cargo mutants" (Stryker recipe: see the guide).
    pub cmd: Option<String>,
    /// Per-test-run timeout passed to the tool (default 300).
    pub timeout_secs: Option<u64>,
    /// Advisory flag for doctor + guide hints; the command always works.
    pub enabled: Option<bool>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct ConsensusFileConfig {
    /// Grant plan-review the mcp__pal__consensus tool (needs the pal MCP
    /// server + a Gemini key — see the guide). Default false.
    pub enabled: Option<bool>,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub theme: Theme,
    pub theme_name: String,
    pub base_ref: String,
    /// argv for the claude binary (overridable for tests via RITUAL_CLAUDE_CMD)
    pub claude_cmd: Vec<String>,
    pub codex_cmd: Vec<String>,
    pub budget_plan_review_usd: f64,
    pub budget_dual_review_usd: f64,
    pub budget_doc_chat_usd: f64,
    pub keymap: Keymap,
    pub redaction: bool,
    pub budget_daily_usd: Option<f64>,
    pub notifications: bool,
    pub models: HashMap<String, String>,
    pub fallback_model: Option<String>,
    pub check_timeout_secs: u64,
    pub offline: bool,
    pub commands: Vec<(String, String)>,
    pub nvim_server: Option<String>,
    pub consensus_enabled: bool,
    pub gh_cmd: Vec<String>,
    pub mutants_cmd: Vec<String>,
    pub mutants_timeout_secs: u64,
    pub mutants_enabled: bool,
    pub secrets_enabled: bool,
    pub gitleaks_cmd: Vec<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            theme: Theme::default(),
            theme_name: "eldritch".into(),
            base_ref: "main".into(),
            claude_cmd: vec!["claude".into()],
            codex_cmd: vec!["codex".into()],
            budget_plan_review_usd: 5.0,
            budget_dual_review_usd: 10.0,
            budget_doc_chat_usd: 0.50,
            keymap: Keymap::default(),
            redaction: true,
            budget_daily_usd: None,
            notifications: true,
            models: HashMap::new(),
            fallback_model: None,
            check_timeout_secs: 600,
            offline: false,
            commands: Vec::new(),
            nvim_server: None,
            consensus_enabled: false,
            gh_cmd: vec!["gh".into()],
            mutants_cmd: vec!["cargo".into(), "mutants".into()],
            mutants_timeout_secs: 300,
            mutants_enabled: false,
            secrets_enabled: true,
            gitleaks_cmd: vec!["gitleaks".into()],
        }
    }
}

fn load_file(path: &Path) -> Result<FileConfig> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading config {}", path.display()))?;
    toml::from_str(&text).with_context(|| format!("parsing config {}", path.display()))
}

impl Config {
    /// `project_root` is where `.ritual/` lives (usually the git root or cwd).
    pub fn load(project_root: &Path, theme_flag: Option<&str>, ascii_flag: bool) -> Result<Self> {
        let mut cfg = Config::default();
        let mut theme_name = cfg.theme_name.clone();
        let mut icons = IconSet::Nerd;
        let mut key_overrides: HashMap<String, String> = HashMap::new();
        let mut transparency = true;

        let mut layers: Vec<PathBuf> = Vec::new();
        if let Some(dir) = dirs::config_dir() {
            layers.push(dir.join("ritual/config.toml"));
        }
        layers.push(project_root.join(".ritual/config.toml"));

        for path in layers {
            if !path.exists() {
                continue;
            }
            let fc = load_file(&path)?;
            if let Some(t) = fc.theme {
                theme_name = t;
            }
            if let Some(i) = fc.icons {
                icons = if i == "ascii" {
                    IconSet::Ascii
                } else {
                    IconSet::Nerd
                };
            }
            if let Some(b) = fc.base_ref {
                cfg.base_ref = b;
            }
            if let Some(c) = fc.claude_cmd {
                cfg.claude_cmd = split_cmd(&c)?;
            }
            if let Some(c) = fc.codex_cmd {
                cfg.codex_cmd = split_cmd(&c)?;
            }
            if let Some(b) = fc.budget_plan_review_usd {
                cfg.budget_plan_review_usd = b;
            }
            if let Some(b) = fc.budget_dual_review_usd {
                cfg.budget_dual_review_usd = b;
            }
            if let Some(b) = fc.budget_doc_chat_usd {
                cfg.budget_doc_chat_usd = b;
            }
            if let Some(keys) = fc.keys {
                key_overrides.extend(keys); // later layers win per-action
            }
            if let Some(r) = fc.redaction {
                cfg.redaction = r;
            }
            if let Some(b) = fc.budget_daily_usd {
                cfg.budget_daily_usd = Some(b);
            }
            if let Some(n) = fc.notifications {
                cfg.notifications = n;
            }
            if let Some(models) = fc.models {
                cfg.models.extend(models);
            }
            if let Some(f) = fc.fallback_model {
                cfg.fallback_model = Some(f);
            }
            if let Some(t) = fc.check_timeout_secs {
                cfg.check_timeout_secs = t;
            }
            if let Some(o) = fc.offline {
                cfg.offline = o;
            }
            if let Some(t) = fc.transparency {
                transparency = t;
            }
            if let Some(n) = fc.nvim_server {
                cfg.nvim_server = Some(n);
            }
            if let Some(commands) = fc.commands {
                for (name, template) in commands {
                    cfg.commands.retain(|(n, _)| *n != name);
                    cfg.commands.push((name, template));
                }
                cfg.commands.sort_by(|a, b| a.0.cmp(&b.0));
            }
            if let Some(c) = fc.consensus
                && let Some(enabled) = c.enabled
            {
                cfg.consensus_enabled = enabled;
            }
            if let Some(g) = fc.gh_cmd {
                cfg.gh_cmd = split_cmd(&g)?;
            }
            if let Some(m) = fc.mutants {
                if let Some(c) = m.cmd {
                    cfg.mutants_cmd = split_cmd(&c)?;
                }
                if let Some(t) = m.timeout_secs {
                    cfg.mutants_timeout_secs = t;
                }
                if let Some(e) = m.enabled {
                    cfg.mutants_enabled = e;
                }
            }
            if let Some(s) = fc.secrets {
                if let Some(e) = s.enabled {
                    cfg.secrets_enabled = e;
                }
                if let Some(c) = s.gitleaks_cmd {
                    cfg.gitleaks_cmd = split_cmd(&c)?;
                }
            }
        }

        // Env overrides (also the test seam).
        if let Ok(c) = std::env::var("RITUAL_CLAUDE_CMD") {
            cfg.claude_cmd = split_cmd(&c)?;
        }
        if let Ok(c) = std::env::var("RITUAL_CODEX_CMD") {
            cfg.codex_cmd = split_cmd(&c)?;
        }
        if let Ok(c) = std::env::var("RITUAL_GH_CMD") {
            cfg.gh_cmd = split_cmd(&c)?;
        }
        if let Ok(c) = std::env::var("RITUAL_MUTANTS_CMD") {
            cfg.mutants_cmd = split_cmd(&c)?;
        }
        if let Ok(c) = std::env::var("RITUAL_GITLEAKS_CMD") {
            cfg.gitleaks_cmd = split_cmd(&c)?;
        }

        // CLI flags win.
        if let Some(t) = theme_flag {
            theme_name = t.to_string();
        }
        if ascii_flag {
            icons = IconSet::Ascii;
        }

        cfg.theme = Theme::by_name(&theme_name, icons)
            .with_context(|| format!("unknown theme '{theme_name}' (eldritch, tokyonight)"))?;
        cfg.theme.transparency = transparency;
        cfg.theme_name = theme_name;
        cfg.keymap = Keymap::default().with_overrides(&key_overrides)?;
        Ok(cfg)
    }
}

fn split_cmd(s: &str) -> Result<Vec<String>> {
    let argv = shlex::split(s).context("un-parseable command override")?;
    anyhow::ensure!(!argv.is_empty(), "empty command override");
    Ok(argv)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_load_without_files() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = Config::load(tmp.path(), None, false).unwrap();
        assert_eq!(cfg.theme_name, "eldritch");
        assert_eq!(cfg.claude_cmd, vec!["claude"]);
        assert!(!cfg.consensus_enabled, "consensus ships dark");
    }

    #[test]
    fn consensus_table_parses() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join(".ritual")).unwrap();
        std::fs::write(
            tmp.path().join(".ritual/config.toml"),
            "[consensus]\nenabled = true\n",
        )
        .unwrap();
        let cfg = Config::load(tmp.path(), None, false).unwrap();
        assert!(cfg.consensus_enabled);
    }

    #[test]
    fn fallback_model_parses() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join(".ritual")).unwrap();
        std::fs::write(
            tmp.path().join(".ritual/config.toml"),
            "fallback_model = \"claude-sonnet-5,claude-haiku-4-5\"\n",
        )
        .unwrap();
        let cfg = Config::load(tmp.path(), None, false).unwrap();
        assert_eq!(
            cfg.fallback_model.as_deref(),
            Some("claude-sonnet-5,claude-haiku-4-5")
        );
    }

    #[test]
    fn project_config_overrides() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join(".ritual")).unwrap();
        std::fs::write(
            tmp.path().join(".ritual/config.toml"),
            "theme = \"tokyonight\"\nbase_ref = \"develop\"\nclaude_cmd = \"tests/fake_agent.sh --flag\"\n",
        )
        .unwrap();
        let cfg = Config::load(tmp.path(), None, false).unwrap();
        assert_eq!(cfg.theme_name, "tokyonight");
        assert_eq!(cfg.base_ref, "develop");
        assert_eq!(cfg.claude_cmd, vec!["tests/fake_agent.sh", "--flag"]);
    }

    #[test]
    fn flag_beats_file() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join(".ritual")).unwrap();
        std::fs::write(
            tmp.path().join(".ritual/config.toml"),
            "theme = \"tokyonight\"\n",
        )
        .unwrap();
        let cfg = Config::load(tmp.path(), Some("eldritch"), true).unwrap();
        assert_eq!(cfg.theme_name, "eldritch");
        assert_eq!(cfg.theme.icons, IconSet::Ascii);
    }
}
