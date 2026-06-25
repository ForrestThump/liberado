//! Binary entry point for the Liberado TUI.
//!
//! Initializes the terminal (raw mode, alternate screen), spawns background tasks for
//! HTTP polling, SSE streaming, and keyboard input, then drives the ratatui draw loop
//! against the shared `App` state.

use std::sync::{Arc, Mutex};

use crossterm::{
    event::{self, Event as CEvent, KeyCode},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::backend::{Backend, CrosstermBackend};
use ratatui::Terminal;
use tokio::sync::mpsc;

use liberado_tui::app::{Action, App, Effect};
use liberado_tui::ui;

/// The server base URL, loaded from `LIBERADO_SERVER` env or the default.
const DEFAULT_SERVER: &str = "http://127.0.0.1:4201";

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    todo!("main: initialize terminal, spawn tasks, run draw loop")
}

/// Spawn a background task that polls `/api/status` and `/api/reactions` on a ticker
/// and sends `Action::StatusUpdate` / `Action::ReactionsUpdate` into the main loop.
fn spawn_poller(tx: mpsc::UnboundedSender<Action>, server: String) {
    todo!("spawn poller: tokio::spawn an interval loop calling api::fetch_status and api::fetch_reactions")
}

/// Spawn a background task that reads crossterm key events and sends
/// `Action::Input(key)` into the main loop.
fn spawn_input(tx: mpsc::UnboundedSender<Action>) {
    todo!("spawn input: tokio::task::spawn_blocking reading crossterm events")
}

/// Map a crossterm key event to an `Action::Input` variant.
fn key_to_action(key: KeyCode) -> Option<Action> {
    todo!("key_to_action: map Enter/Esc/Tab/j/k/PgUp/PgDn/char to Action variants")
}

/// Execute an `Effect` instruction returned by `App::update()`. This is where I/O
/// happens — the app state machine itself is pure.
async fn execute_effect(
    effect: Effect,
    app: Arc<Mutex<App>>,
    action_tx: mpsc::UnboundedSender<Action>,
    client: &reqwest::Client,
) {
    todo!("execute_effect: match on Effect variant — spawn SSE, fetch conversations, quit")
}

/// Restore the terminal to its original state.
fn cleanup_terminal() -> Result<(), Box<dyn std::error::Error>> {
    disable_raw_mode()?;
    execute!(std::io::stderr(), LeaveAlternateScreen)?;
    Ok(())
}
