//! Terminal lifecycle management for the Liberado TUI.
//!
//! `TerminalGuard` enables raw mode, alternate screen, and mouse capture
//! on construction and restores the terminal on drop (even on panic).

use std::io::{self, Stdout};

use crossterm::{
    execute,
    event::{DisableMouseCapture, EnableMouseCapture},
    terminal::{
        disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
        SetTitle,
    },
};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;

/// RAII guard that enters raw mode + alternate screen + mouse capture on
/// construction and restores the terminal on drop (even during panic).
///
/// ```ignore
/// let (_guard, mut terminal) = TerminalGuard::enter()?;
/// // ... run event loop ...
/// // Terminal is restored when `_guard` goes out of scope.
/// ```
pub struct TerminalGuard {
    _private: (),
}

impl TerminalGuard {
    /// Enter raw mode, enable alternate screen and mouse capture, and return
    /// a guard plus a `ratatui::Terminal` ready for drawing.
    pub fn enter() -> io::Result<(Self, Terminal<CrosstermBackend<Stdout>>)> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        execute!(
            stdout,
            EnterAlternateScreen,
            EnableMouseCapture,
            SetTitle("Liberado TUI")
        )?;
        let backend = CrosstermBackend::new(stdout);
        let terminal = Terminal::new(backend)?;
        Ok((Self { _private: () }, terminal))
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen, DisableMouseCapture);
    }
}
