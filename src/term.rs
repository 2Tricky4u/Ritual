//! Single owner of the terminal state. ALL raw-mode/altscreen transitions
//! live here; never call crossterm enable/disable anywhere else.
//!
//! Suspend/resume contract (interactive handoff to `claude` / $EDITOR):
//! 1. caller stops the input task (and AWAITS its join) BEFORE suspend()
//! 2. suspend(): leave altscreen, disable raw mode, show cursor
//! 3. child runs attached with Stdio::inherit, SIGINT ignored in ritual
//! 4. resume(): re-enter raw+altscreen, clear, force full redraw
//!
//! A panic hook restores the terminal best-effort so a crash never leaves
//! the shell in raw mode.

use std::io::Stdout;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context, Result};
use crossterm::event::{DisableBracketedPaste, EnableBracketedPaste};
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

static TERMINAL_ACTIVE: AtomicBool = AtomicBool::new(false);

fn restore_terminal_best_effort() {
    if TERMINAL_ACTIVE.swap(false, Ordering::SeqCst) {
        let _ = disable_raw_mode();
        let _ = crossterm::execute!(
            std::io::stdout(),
            DisableBracketedPaste,
            LeaveAlternateScreen,
            crossterm::cursor::Show
        );
    }
}

/// Install once at TUI startup, before entering the alternate screen.
pub fn install_panic_hook() {
    static INSTALLED: AtomicBool = AtomicBool::new(false);
    if INSTALLED.swap(true, Ordering::SeqCst) {
        return;
    }
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore_terminal_best_effort();
        previous(info);
    }));
}

pub struct Term {
    pub terminal: Terminal<CrosstermBackend<Stdout>>,
}

impl Term {
    pub fn enter() -> Result<Self> {
        install_panic_hook();
        enable_raw_mode().context("enabling raw mode")?;
        let mut stdout = std::io::stdout();
        // No mouse capture: ritual is keyboard-first and handles no mouse
        // events, so capturing them would only rob the terminal of its native
        // click-drag text selection (copy a finding's file:line, an error, a
        // run id) while giving nothing back. Bracketed paste IS enabled so a
        // multi-line paste into the chat input arrives as one Paste event
        // instead of newline key presses that would submit mid-paste.
        crossterm::execute!(stdout, EnterAlternateScreen, EnableBracketedPaste)
            .context("entering alternate screen")?;
        TERMINAL_ACTIVE.store(true, Ordering::SeqCst);
        let terminal = Terminal::new(CrosstermBackend::new(stdout))?;
        Ok(Self { terminal })
    }

    /// Hand the terminal to a child process. Caller must have stopped the
    /// input task first (crossterm's event reader is a process-global).
    pub fn suspend(&mut self) -> Result<()> {
        TERMINAL_ACTIVE.store(false, Ordering::SeqCst);
        disable_raw_mode()?;
        crossterm::execute!(
            self.terminal.backend_mut(),
            DisableBracketedPaste,
            LeaveAlternateScreen,
            crossterm::cursor::Show
        )?;
        self.terminal.show_cursor()?;
        Ok(())
    }

    /// Take the terminal back after a child exits. Clears and forces a full
    /// redraw so a resize-while-suspended can't corrupt the buffer.
    pub fn resume(&mut self) -> Result<()> {
        enable_raw_mode()?;
        crossterm::execute!(
            self.terminal.backend_mut(),
            EnterAlternateScreen,
            EnableBracketedPaste
        )?;
        TERMINAL_ACTIVE.store(true, Ordering::SeqCst);
        self.terminal.hide_cursor()?;
        self.terminal.clear()?;
        Ok(())
    }

    /// Run an attached child (claude, $EDITOR) with SIGINT ignored in ritual
    /// so Ctrl-C inside the child doesn't kill the TUI.
    pub fn run_attached(&mut self, argv: &[String], cwd: &std::path::Path) -> Result<bool> {
        use nix::sys::signal::{SigHandler, Signal, signal};

        let (bin, args) = argv.split_first().context("empty argv")?;
        self.suspend()?;

        let old = unsafe { signal(Signal::SIGINT, SigHandler::SigIgn) }?;
        let status = std::process::Command::new(bin)
            .args(args)
            .current_dir(cwd)
            .status();
        unsafe { signal(Signal::SIGINT, old) }?;

        self.resume()?;
        Ok(status.map(|s| s.success()).unwrap_or(false))
    }
}

impl Drop for Term {
    fn drop(&mut self) {
        restore_terminal_best_effort();
    }
}
