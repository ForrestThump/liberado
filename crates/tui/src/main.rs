//! Binary entry point for the Liberado TUI.
//!
//! Initializes the terminal (raw mode, alternate screen), spawns background tasks for
//! HTTP polling and SSE streaming, reads keyboard input, and drives the ratatui draw
//! loop against the shared `App` state.

use parking_lot::Mutex;
use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use crossterm::event::{self, Event as CEvent, KeyEventKind};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use tokio::sync::mpsc;

use liberado_theme::ThemeRegistry;
use liberado_tui::api;
use liberado_tui::app::{Action, App, Effect};
use liberado_tui::effects::{EffectRunner, StreamState};
use liberado_tui::terminal::TerminalGuard;
use liberado_tui::tuning::*;
use liberado_tui::ui;

const DEFAULT_SERVER: &str = "http://127.0.0.1:4201";

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let server = std::env::var("LIBERADO_SERVER").unwrap_or_else(|_| DEFAULT_SERVER.to_string());

    let _ = reqwest::Url::parse(&server).unwrap_or_else(|e| {
        eprintln!("invalid LIBERADO_SERVER URL '{server}': {e}");
        eprintln!("Expected format: http://127.0.0.1:4201");
        std::process::exit(1);
    });

    // Only emit tracing output when the user explicitly opts in via RUST_LOG.
    // The default ("off") keeps the terminal clean — log lines mixed into a
    // TUI alternate screen corrupt the display.
    tracing_subscriber::fmt()
        .with_writer(io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "off".into()),
        )
        .init();

    let (_guard, mut terminal) = TerminalGuard::enter()?;

    let client = reqwest::Client::new();
    let stream_state = Arc::new(Mutex::new(StreamState::default()));

    let mut theme_registry = ThemeRegistry::new();
    if let Some(dir) = liberado_theme::user_themes_dir() {
        let errors = theme_registry.load_user_themes(&dir);
        for e in &errors {
            tracing::warn!("theme load error: {e}");
        }
    }

    let app = Arc::new(Mutex::new(App::new(server.clone(), theme_registry)));
    let should_quit = Arc::new(AtomicBool::new(false));

    let quit_flag = should_quit.clone();
    ctrlc::set_handler(move || {
        quit_flag.store(true, Ordering::Relaxed);
    })
    .expect("failed to set Ctrl+C/SIGTERM handler");

    let (action_tx, mut action_rx) = mpsc::channel::<Action>(ACTION_CHANNEL_CAPACITY);

    spawn_poller(action_tx.clone(), server, client.clone());

    let runner = EffectRunner {
        app: app.clone(),
        should_quit: should_quit.clone(),
        action_tx: action_tx.clone(),
        client: client.clone(),
        stream_state: stream_state.clone(),
    };

    run_loop(&mut terminal, &runner, &mut action_rx).await
}

async fn run_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    runner: &EffectRunner,
    action_rx: &mut mpsc::Receiver<Action>,
) -> Result<(), Box<dyn std::error::Error>> {
    // Spinner phase is wall-clock based so reconnect/stream glyphs advance at a fixed
    // human-readable rate (~SPINNER_FRAME_MS each), not once per ~16 ms redraw.
    let spinner_origin = Instant::now();
    loop {
        if runner.should_quit.load(Ordering::Relaxed) {
            break;
        }

        if event::poll(POLL_INTERVAL)? {
            handle_terminal_event(runner, event::read()?).await;
        }

        drain_actions(runner, action_rx).await;

        // T1.3: skip full redraw when state is unchanged and nothing is animating.
        draw_if_needed(terminal, runner, &spinner_origin).await?;
    }

    Ok(())
}

/// Dispatch one terminal event into the app and run the effects it produced.
async fn handle_terminal_event(runner: &EffectRunner, event: CEvent) {
    match event {
        CEvent::Key(key) => {
            if key.kind == KeyEventKind::Press || key.kind == KeyEventKind::Repeat {
                let effects = {
                    let mut app_guard = runner.app.lock();
                    app_guard.handle_key(key)
                };
                for effect in effects {
                    runner.run(effect).await;
                }
            }
        }
        CEvent::Mouse(mouse) => {
            let effects = {
                let mut app_guard = runner.app.lock();
                app_guard.handle_mouse(mouse)
            };
            for effect in effects {
                runner.run(effect).await;
            }
        }
        CEvent::Resize(_, _) => {
            // Layout is recomputed from frame.area() on the next draw.
            runner.app.lock().mark_dirty();
        }
        _ => {}
    }
}

/// Drain queued actions into the app until the queue is empty or a Quit effect stops the loop.
async fn drain_actions(runner: &EffectRunner, action_rx: &mut mpsc::Receiver<Action>) {
    'action_loop: while let Ok(action) = action_rx.try_recv() {
        let effects = {
            let mut app_guard = runner.app.lock();
            app_guard.update(action)
        };
        for effect in effects {
            if matches!(effect, Effect::Quit) {
                runner.should_quit.store(true, Ordering::Relaxed);
                break 'action_loop;
            }
            runner.run(effect).await;
        }
    }
}

/// Redraw when state changed or something is animating.
async fn draw_if_needed(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    runner: &EffectRunner,
    spinner_origin: &Instant,
) -> Result<(), Box<dyn std::error::Error>> {
    if runner.app.lock().should_draw() {
        let spinner_tick =
            (spinner_origin.elapsed().as_millis() / u128::from(SPINNER_FRAME_MS)) as u8;
        terminal.draw(|frame| {
            let mut app_guard = runner.app.lock();
            ui::draw(frame, &mut app_guard, spinner_tick);
            app_guard.clear_dirty();
        })?;
    }
    Ok(())
}

fn spawn_poller(tx: mpsc::Sender<Action>, server: String, client: reqwest::Client) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(BACKEND_POLL_INTERVAL);
        let mut connected = false;
        let mut failures: u32 = 0;
        loop {
            interval.tick().await;

            let status_result = api::fetch_status(&client, &server).await;
            match status_result {
                Ok(Some(status)) => {
                    if !connected && tx.try_send(Action::ConnectionStatus(true)).is_err() {
                        tracing::warn!("action channel full, dropping ConnectionStatus");
                    }
                    connected = true;
                    failures = 0;
                    if tx.try_send(Action::StatusUpdate(status)).is_err() {
                        tracing::warn!("action channel full, dropping StatusUpdate");
                    }
                }
                Ok(None) => {
                    failures += 1;
                    if failures >= MAX_POLL_FAILURES && connected {
                        connected = false;
                        if tx.try_send(Action::ConnectionStatus(false)).is_err() {
                            tracing::warn!("action channel full, dropping ConnectionStatus");
                        }
                    }
                }
                Err(_) => {
                    failures += 1;
                    if failures >= MAX_POLL_FAILURES && connected {
                        connected = false;
                        if tx.try_send(Action::ConnectionStatus(false)).is_err() {
                            tracing::warn!("action channel full, dropping ConnectionStatus");
                        }
                    }
                }
            }

            // Reactions feed is intentionally not shown in the sparse layout.
            // Conversations still poll so /session browser stays fresh.
            match api::fetch_conversations(&client, &server).await {
                Ok(convs) => {
                    if tx.try_send(Action::ConversationsUpdate(convs)).is_err() {
                        tracing::warn!("action channel full, dropping ConversationsUpdate");
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, "conversations poll failed");
                }
            }

            if tx.try_send(Action::Tick).is_err() {
                tracing::warn!("action channel full, dropping Tick");
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use liberado_theme::ThemeRegistry;
    use liberado_tui::effects::{EffectRunner, StreamState};

    fn test_runner() -> (EffectRunner, mpsc::Sender<Action>, mpsc::Receiver<Action>) {
        let app = Arc::new(Mutex::new(App::new(
            "http://127.0.0.1:4201".to_string(),
            ThemeRegistry::new(),
        )));
        let should_quit = Arc::new(AtomicBool::new(false));
        let (action_tx, action_rx) = mpsc::channel::<Action>(32);
        let runner = EffectRunner {
            app: app.clone(),
            should_quit: should_quit.clone(),
            action_tx: action_tx.clone(),
            client: reqwest::Client::new(),
            stream_state: Arc::new(Mutex::new(StreamState::default())),
        };
        (runner, action_tx, action_rx)
    }

    /// Queued actions are drained until the queue is empty.
    #[tokio::test]
    async fn drain_actions_empties_the_queue() {
        let (runner, action_tx, mut action_rx) = test_runner();
        action_tx.send(Action::SseDone).await.unwrap();
        action_tx.send(Action::SseDone).await.unwrap();
        drain_actions(&runner, &mut action_rx).await;
        assert!(
            action_rx.try_recv().is_err(),
            "queue should be empty after drain_actions"
        );
    }
}
