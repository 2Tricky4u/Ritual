//! Best-effort system clipboard write for the TUI. Selecting text inside a
//! float panel with the mouse grabs whole terminal rows (sidebar and all), so
//! anything worth copying (the implement prompt) is put on the clipboard
//! directly instead. Tries the common CLI tools first — reliable and
//! verifiable — then falls back to an OSC 52 escape sequence (terminal-native,
//! works over SSH or when no tool is installed).

use std::io::Write;
use std::process::{Command, Stdio};

/// argv for the clipboard writers, in preference order: `(bin, clipboard-args,
/// primary-args)`. `wl-copy` (Wayland) first, then the X11 tools, then macOS.
/// Empty `primary-args` = the tool has no separate primary selection. We set
/// BOTH selections so paste works whether the user hits Ctrl+Shift+V
/// (clipboard) or middle-clicks (primary).
#[cfg_attr(test, allow(dead_code))]
const CLI_TOOLS: &[(&str, &[&str], &[&str])] = &[
    ("wl-copy", &[], &["--primary"]),
    (
        "xclip",
        &["-selection", "clipboard"],
        &["-selection", "primary"],
    ),
    (
        "xsel",
        &["--clipboard", "--input"],
        &["--primary", "--input"],
    ),
    ("pbcopy", &[], &[]),
];

/// Copy `text` to the system clipboard (and primary selection where the tool
/// supports it). Returns true if a method reported (or, for OSC 52, plausibly
/// achieved) success.
#[cfg(not(test))]
pub fn copy(text: &str) -> bool {
    for (bin, clip_args, primary_args) in CLI_TOOLS {
        if try_tool(bin, clip_args, text) {
            if !primary_args.is_empty() {
                let _ = try_tool(bin, primary_args, text); // best-effort primary
            }
            return true;
        }
    }
    osc52(text)
}

/// Under `cargo test`, never spawn a clipboard tool or clobber the dev's
/// clipboard — the caller only needs a truthy result.
#[cfg(test)]
pub fn copy(_text: &str) -> bool {
    true
}

#[cfg_attr(test, allow(dead_code))]
fn try_tool(bin: &str, args: &[&str], text: &str) -> bool {
    let Ok(mut child) = Command::new(bin)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    else {
        return false;
    };
    {
        let Some(mut stdin) = child.stdin.take() else {
            let _ = child.kill();
            return false;
        };
        if stdin.write_all(text.as_bytes()).is_err() {
            return false;
        }
        // stdin dropped here → EOF, so the tool finishes and exits (wl-copy /
        // xclip fork a daemon to keep serving the selection; the parent exits).
    }
    matches!(child.wait(), Ok(s) if s.success())
}

/// Emit an OSC 52 clipboard-set sequence to the terminal. Non-rendering, so it
/// is safe to write while the alt-screen TUI is up; kitty/wezterm/foot/iTerm2
/// (and tmux with `set-clipboard on`) honor it. Best-effort: the terminal may
/// ignore it, which we can't detect.
#[cfg_attr(test, allow(dead_code))]
fn osc52(text: &str) -> bool {
    let seq = format!("\x1b]52;c;{}\x07", base64(text.as_bytes()));
    let mut out = std::io::stdout();
    out.write_all(seq.as_bytes()).is_ok() && out.flush().is_ok()
}

/// Minimal standard-alphabet base64 (no dependency), enough for OSC 52.
fn base64(input: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(T[((n >> 18) & 63) as usize] as char);
        out.push(T[((n >> 12) & 63) as usize] as char);
        out.push(if chunk.len() > 1 {
            T[((n >> 6) & 63) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            T[(n & 63) as usize] as char
        } else {
            '='
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_known_vectors() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foob"), "Zm9vYg==");
        assert_eq!(base64(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
        assert_eq!(base64(b"hi"), "aGk=");
    }

    #[test]
    fn copy_is_a_noop_under_test() {
        // Must never spawn a clipboard tool or clobber the dev's clipboard.
        assert!(copy("anything"));
    }
}
