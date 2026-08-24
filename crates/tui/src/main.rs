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

/// Fold one status-poll result into the connection state machine and return the actions to
/// enqueue. Pure, so the failure threshold and reconnect logic are unit-testable without tokio.
fn poll_step<E>(
    connected: bool,
    failures: u32,
    status: Result<Option<api::DaemonStatus>, E>,
) -> (bool, u32, Vec<Action>) {
    match status {
        Ok(Some(status)) => {
            let mut actions = Vec::new();
            if !connected {
                actions.push(Action::ConnectionStatus(true));
            }
            actions.push(Action::StatusUpdate(status));
            (true, 0, actions)
        }
        Ok(None) | Err(_) => {
            let failures = failures + 1;
            if failures >= MAX_POLL_FAILURES && connected {
                (false, failures, vec![Action::ConnectionStatus(false)])
            } else {
                (connected, failures, Vec::new())
            }
        }
    }
}

fn spawn_poller(tx: mpsc::Sender<Action>, server: String, client: reqwest::Client) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(BACKEND_POLL_INTERVAL);
        let mut connected = false;
        let mut failures: u32 = 0;
        loop {
            interval.tick().await;

            let status_result = api::fetch_status(&client, &server).await;
            let (new_connected, new_failures, status_actions) =
                poll_step(connected, failures, status_result);
            for action in status_actions {
                if tx.try_send(action).is_err() {
                    tracing::warn!("action channel full, dropping status action");
                }
            }
            connected = new_connected;
            failures = new_failures;

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
    use liberado_tui::api::DaemonStatus;
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

    fn test_status(running: bool) -> DaemonStatus {
        DaemonStatus {
            running,
            vault_path: "/vault".into(),
            uptime_seconds: 0,
            watcher_active: false,
            dispatcher_attached: false,
            orchestrator_attached: false,
            reactions_seen: 0,
            model_name: None,
            token_usage_total: None,
            context_window: None,
            chat_tools: 0,
            chat_tool_names: Vec::new(),
            enter_sends: true,
        }
    }

    #[test]
    fn poll_step_connects_and_reports_status() {
        let (connected, failures, actions) =
            poll_step::<std::io::Error>(false, 0, Ok(Some(test_status(true))));
        assert!(connected);
        assert_eq!(failures, 0);
        assert_eq!(actions.len(), 2);
        assert!(matches!(actions[0], Action::ConnectionStatus(true)));
        assert!(matches!(actions[1], Action::StatusUpdate(_)));
    }

    #[test]
    fn poll_step_stays_connected_without_duplicate_connect_event() {
        let (connected, failures, actions) =
            poll_step::<std::io::Error>(true, 0, Ok(Some(test_status(false))));
        assert!(connected);
        assert_eq!(failures, 0);
        assert_eq!(actions.len(), 1);
        assert!(matches!(actions[0], Action::StatusUpdate(_)));
    }

    #[test]
    fn poll_step_ignores_a_single_missed_poll() {
        let (connected, failures, actions) = poll_step::<std::io::Error>(true, 0, Ok(None));
        assert!(connected);
        assert_eq!(failures, 1);
        assert!(actions.is_empty());
    }

    #[test]
    fn poll_step_disconnects_at_the_failure_threshold() {
        let (connected, failures, actions) = poll_step::<std::io::Error>(true, 1, Ok(None));
        assert!(!connected);
        assert_eq!(failures, 2);
        assert_eq!(actions.len(), 1);
        assert!(matches!(actions[0], Action::ConnectionStatus(false)));
    }

    #[test]
    fn poll_step_error_counts_like_a_missed_poll() {
        let err = std::io::Error::other("poll failed");
        let (connected, failures, actions) = poll_step(true, 1, Err(err));
        assert!(!connected);
        assert_eq!(failures, 2);
        assert_eq!(actions.len(), 1);
        assert!(matches!(actions[0], Action::ConnectionStatus(false)));
    }

    #[test]
    fn poll_step_reconnects_after_being_disconnected() {
        let (connected, failures, actions) =
            poll_step::<std::io::Error>(false, 2, Ok(Some(test_status(true))));
        assert!(connected);
        assert_eq!(failures, 0);
        assert_eq!(actions.len(), 2);
        assert!(matches!(actions[0], Action::ConnectionStatus(true)));
    }

    #[test]
    fn poll_step_disconnected_none_does_not_resend_disconnect() {
        let (connected, failures, actions) = poll_step::<std::io::Error>(false, 0, Ok(None));
        assert!(!connected);
        assert_eq!(failures, 1);
        assert!(actions.is_empty());
    }
    // ── handle_terminal_event ───────────────────────────────────────────

    use crossterm::event::{KeyModifiers, MouseEvent, MouseEventKind};

    #[tokio::test]
    async fn ctrl_c_key_press_runs_the_quit_effect() {
        let (runner, _action_tx, mut action_rx) = test_runner();
        let key = crossterm::event::KeyEvent::new_with_kind(
            crossterm::event::KeyCode::Char('c'),
            KeyModifiers::CONTROL,
            KeyEventKind::Press,
        );
        handle_terminal_event(&runner, CEvent::Key(key)).await;
        assert!(
            runner.should_quit.load(Ordering::Relaxed),
            "Ctrl+C must run the Quit effect, which sets should_quit"
        );
        assert!(action_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn a_key_release_is_ignored() {
        let (runner, _action_tx, mut action_rx) = test_runner();
        let key = crossterm::event::KeyEvent::new_with_kind(
            crossterm::event::KeyCode::Char('q'),
            KeyModifiers::empty(),
            KeyEventKind::Release,
        );
        handle_terminal_event(&runner, CEvent::Key(key)).await;
        assert!(!runner.should_quit.load(Ordering::Relaxed));
        assert!(action_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn a_resize_marks_the_frame_dirty() {
        let (runner, _action_tx, mut action_rx) = test_runner();
        handle_terminal_event(&runner, CEvent::Resize(80, 24)).await;
        assert!(
            runner.app.lock().should_draw(),
            "resize forces a redraw on the next tick"
        );
        assert!(action_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn a_mouse_click_dispatches_into_the_app() {
        use crossterm::event::MouseButton;

        let (runner, _action_tx, mut action_rx) = test_runner();
        runner.app.lock().clear_dirty();
        let mouse = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 10,
            row: 5,
            modifiers: KeyModifiers::empty(),
        };
        handle_terminal_event(&runner, CEvent::Mouse(mouse)).await;
        // The exact pane hit is App's business; here we pin that mouse events reach it and
        // leave an observable state change (dirty flag or queued effect) behind.
        let dirty_or_action = runner.app.lock().should_draw() || action_rx.try_recv().is_ok();
        assert!(dirty_or_action, "the click was delivered to the app");
    }
}
