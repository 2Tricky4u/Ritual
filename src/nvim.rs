//! Remote control of a RUNNING nvim instance — no TUI suspend, no nested
//! editors. Discovery order: config `nvim_server` → `$NVIM` (set inside
//! nvim terminals) → newest `$XDG_RUNTIME_DIR/nvim.*.0` socket.
//! All commands go through `nvim --server <sock> --remote-expr execute(...)`,
//! which works regardless of the remote instance's current mode.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

/// Locate a live nvim server socket.
pub fn discover(config_override: Option<&str>) -> Option<PathBuf> {
    if let Some(p) = config_override {
        let p = PathBuf::from(p);
        if p.exists() {
            return Some(p);
        }
    }
    if let Ok(sock) = std::env::var("NVIM") {
        let p = PathBuf::from(sock);
        if p.exists() {
            return Some(p);
        }
    }
    // Default socket location since nvim 0.10: $XDG_RUNTIME_DIR/nvim.<pid>.0
    let runtime = std::env::var("XDG_RUNTIME_DIR").ok()?;
    let mut candidates: Vec<(std::time::SystemTime, PathBuf)> = std::fs::read_dir(runtime)
        .ok()?
        .filter_map(|e| e.ok())
        .filter(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            name.starts_with("nvim.") && name.ends_with(".0")
        })
        .filter_map(|e| {
            let meta = e.metadata().ok()?;
            Some((meta.modified().ok()?, e.path()))
        })
        .collect();
    candidates.sort();
    candidates.pop().map(|(_, p)| p)
}

/// Single-quote escape for vimscript string literals: ' -> ''.
fn vim_escape(s: &str) -> String {
    s.replace('\'', "''")
}

fn remote_expr(server: &Path, expr: &str) -> Result<String> {
    let out = std::process::Command::new("nvim")
        .arg("--server")
        .arg(server)
        .arg("--remote-expr")
        .arg(expr)
        .output()
        .context("running nvim --remote-expr (is nvim on PATH?)")?;
    if !out.status.success() {
        bail!(
            "nvim remote call failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Open `file` at `line` in the remote nvim (current window).
pub fn open_at(server: &Path, file: &Path, line: Option<u32>) -> Result<()> {
    let file = std::fs::canonicalize(file).unwrap_or_else(|_| file.to_path_buf());
    let cmd = match line {
        Some(l) => format!(
            "execute('edit +{l} ' . fnameescape('{}'))",
            vim_escape(&file.display().to_string())
        ),
        None => format!(
            "execute('edit ' . fnameescape('{}'))",
            vim_escape(&file.display().to_string())
        ),
    };
    remote_expr(server, &cmd).map(|_| ())
}

/// One quickfix entry.
pub struct QfEntry {
    pub file: String,
    pub line: u32,
    pub text: String,
}

/// Replace the remote quickfix list with `entries` and open :copen.
pub fn send_quickfix(server: &Path, entries: &[QfEntry], title: &str) -> Result<usize> {
    if entries.is_empty() {
        bail!("no findings with file:line locations to send");
    }
    let items: Vec<String> = entries
        .iter()
        .map(|e| {
            format!(
                "{{'filename':'{}','lnum':{},'text':'{}'}}",
                vim_escape(&e.file),
                e.line,
                vim_escape(&e.text)
            )
        })
        .collect();
    let expr = format!(
        "setqflist([{}], 'r') + execute('call setqflist([], \"a\", {{\"title\": ''{}''}})') + execute('copen')",
        items.join(","),
        vim_escape(title),
    );
    remote_expr(server, &expr)?;
    Ok(entries.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escaping_single_quotes() {
        assert_eq!(vim_escape("it's a 'test'"), "it''s a ''test''");
    }

    #[test]
    fn discover_prefers_override_then_env() {
        let tmp = tempfile::tempdir().unwrap();
        let sock = tmp.path().join("custom.sock");
        std::fs::write(&sock, "").unwrap();
        assert_eq!(discover(Some(sock.to_str().unwrap())), Some(sock.clone()));
        // Missing override falls through (env/xdg may or may not resolve here;
        // just assert it doesn't return the bogus path).
        assert_ne!(
            discover(Some("/nonexistent/sock")),
            Some(PathBuf::from("/nonexistent/sock"))
        );
    }

    /// Full round-trip against a real headless nvim, when available.
    #[test]
    fn quickfix_roundtrip_headless_nvim() {
        if std::process::Command::new("nvim")
            .arg("--version")
            .output()
            .is_err()
        {
            eprintln!("nvim not installed — skipping");
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let sock = tmp.path().join("it.sock");
        let mut child = std::process::Command::new("nvim")
            .args(["--headless", "--clean", "--listen"])
            .arg(&sock)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        // Wait for the socket.
        for _ in 0..50 {
            if sock.exists() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        assert!(sock.exists(), "headless nvim never opened its socket");

        let entries = vec![
            QfEntry {
                file: "/tmp/a.rs".into(),
                line: 42,
                text: "critical: it's broken".into(),
            },
            QfEntry {
                file: "/tmp/b.rs".into(),
                line: 7,
                text: "minor: meh".into(),
            },
        ];
        send_quickfix(&sock, &entries, "ritual findings").unwrap();
        let len = remote_expr(&sock, "len(getqflist())").unwrap();
        assert_eq!(len, "2");
        let text = remote_expr(&sock, "getqflist()[0].text").unwrap();
        assert!(text.contains("it's broken"));

        let _ = child.kill();
        let _ = child.wait();
    }
}
