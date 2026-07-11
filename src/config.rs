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
    /// Hard ceiling on any check.sh invocation (hung boards, wedged builds).
    pub check_timeout_secs: Option<u64>,
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
    pub keymap: Keymap,
    pub redaction: bool,
    pub budget_daily_usd: Option<f64>,
    pub notifications: bool,
    pub models: HashMap<String, String>,
    pub check_timeout_secs: u64,
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
            keymap: Keymap::default(),
            redaction: true,
            budget_daily_usd: None,
            notifications: true,
            models: HashMap::new(),
            check_timeout_secs: 600,
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
            if let Some(t) = fc.check_timeout_secs {
                cfg.check_timeout_secs = t;
            }
        }

        // Env overrides (also the test seam).
        if let Ok(c) = std::env::var("RITUAL_CLAUDE_CMD") {
            cfg.claude_cmd = split_cmd(&c)?;
        }
        if let Ok(c) = std::env::var("RITUAL_CODEX_CMD") {
            cfg.codex_cmd = split_cmd(&c)?;
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
