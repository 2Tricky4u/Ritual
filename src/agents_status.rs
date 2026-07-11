//! Auth/health probes for the sidebar. All probes run off the UI thread and
//! report back as one AppMsg; every parse is tolerant (None = unknown).

use serde::Deserialize;
use tokio::sync::mpsc;

use crate::config::Config;
use crate::ui::app::AppMsg;

#[derive(Debug, Clone, Default)]
pub struct AgentsStatus {
    pub claude: Option<ClaudeAuth>,
    pub codex_cli_ok: Option<bool>,
    /// (server name, connected) from `claude mcp list`.
    pub mcp_codex_connected: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ClaudeAuth {
    #[serde(rename = "loggedIn", default)]
    pub logged_in: bool,
    #[serde(rename = "subscriptionType", default)]
    pub subscription_type: Option<String>,
}

fn run_capture(argv: &[String], extra: &[&str]) -> Option<std::process::Output> {
    let (bin, args) = argv.split_first()?;
    std::process::Command::new(bin)
        .args(args)
        .args(extra)
        .output()
        .ok()
}

fn probe_claude_auth(cfg: &Config) -> Option<ClaudeAuth> {
    let out = run_capture(&cfg.claude_cmd, &["auth", "status"])?;
    serde_json::from_slice(&out.stdout).ok()
}

fn probe_codex_cli(cfg: &Config) -> Option<bool> {
    let (bin, args) = cfg.codex_cmd.split_first()?;
    std::process::Command::new(bin)
        .args(args)
        .args(["login", "status"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .ok()
        .map(|s| s.success())
}

/// `claude mcp list` is plain text and slow (live health checks) — parse the
/// codex line tolerantly; anything unexpected reads as unknown, not error.
fn probe_mcp_codex(cfg: &Config) -> Option<bool> {
    let out = run_capture(&cfg.claude_cmd, &["mcp", "list"])?;
    let text = String::from_utf8_lossy(&out.stdout);
    for line in text.lines() {
        let lower = line.to_lowercase();
        if lower.starts_with("codex:") {
            return Some(lower.contains("connected") && !lower.contains("needs"));
        }
    }
    None
}

/// Fire all probes on a blocking thread; one message when done.
pub fn spawn_probe(cfg: &Config, tx: mpsc::Sender<AppMsg>) {
    if cfg.offline {
        // Air-gapped: report all-unknown instead of probing cloud auth.
        let _ = tx.try_send(AppMsg::AgentsStatus(Box::default()));
        return;
    }
    let cfg = cfg.clone();
    tokio::task::spawn_blocking(move || {
        let status = AgentsStatus {
            claude: probe_claude_auth(&cfg),
            codex_cli_ok: probe_codex_cli(&cfg),
            mcp_codex_connected: probe_mcp_codex(&cfg),
        };
        let _ = tx.blocking_send(AppMsg::AgentsStatus(Box::new(status)));
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_auth_parses_real_shape() {
        let json = r#"{"loggedIn":true,"authMethod":"claude.ai","subscriptionType":"max"}"#;
        let auth: ClaudeAuth = serde_json::from_str(json).unwrap();
        assert!(auth.logged_in);
        assert_eq!(auth.subscription_type.as_deref(), Some("max"));
    }

    #[test]
    fn claude_auth_tolerates_junk() {
        assert!(serde_json::from_str::<ClaudeAuth>("not json").is_err());
        let minimal: ClaudeAuth = serde_json::from_str("{}").unwrap();
        assert!(!minimal.logged_in);
    }
}
