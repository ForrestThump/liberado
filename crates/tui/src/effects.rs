//! Side-effect execution runtime for the Liberado TUI.
//!
//! `EffectRunner` owns the shared state needed to execute `Effect` instructions produced
//! by `App::update()` and `App::handle_key()`. Each effect arm (SSE streaming, HTTP
//! polling, cancellation, etc.) is a named method, extracted from `main.rs` to keep the
//! event loop focused on orchestration.

use parking_lot::Mutex;
use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crossterm::execute;
use crossterm::terminal::SetTitle;
use futures::StreamExt;
use tokio::sync::mpsc;
use tokio::task::AbortHandle;

use crate::api;
use crate::app::{Action, App, Effect};
use crate::sse::{SseDecoder, ToAction};

/// Holds the `AbortHandle`s for in-flight SSE streaming tasks. `handle` is the chat turn stream;
/// `goal_handle` is a joined goal session's stream (independent — you can be joined to a session
/// while the chat is idle). Each is cleared on its own completion, error, or cancel.
#[derive(Default)]
pub struct StreamState {
    pub handle: Option<AbortHandle>,
    pub goal_handle: Option<AbortHandle>,
}

/// Executes [`Effect`] commands by spawning tokio tasks (SSE streaming, HTTP fetches)
/// or performing synchronous side-effects (cancel, quit, set title).
///
/// ```ignore
/// let runner = EffectRunner {
///     app: app.clone(),
///     action_tx: action_tx.clone(),
///     client: client.clone(),
///     stream_state: stream_state.clone(),
/// };
///
/// for effect in effects {
///     runner.run(effect).await;
/// }
/// ```
///
/// The runner reads the server URL and other context from `app` — effects that carry
/// data payloads (e.g. `StartChatStream { message, session }`) use those payloads
/// directly; the runner never re-reads App state for effect-specific data.
pub struct EffectRunner {
    pub app: Arc<Mutex<App>>,
    pub should_quit: Arc<AtomicBool>,
    pub action_tx: mpsc::Sender<Action>,
    pub client: reqwest::Client,
    pub stream_state: Arc<Mutex<StreamState>>,
}

impl EffectRunner {
    fn server_url(&self) -> String {
        self.app.lock().server.clone()
    }

    /// Execute one effect. Async effects (SSE, HTTP) spawn their work and return;
    /// the caller should `.await` the returned future.
    pub async fn run(&self, effect: Effect) {
        match effect {
            Effect::StartChatStream { message, session } => {
                self.start_chat_stream(message, session).await
            }
            Effect::CancelStream => self.cancel_stream(),
            Effect::RefreshConversations => self.refresh_conversations().await,
            Effect::LoadConversationHistory(id) => self.load_conversation_history(id).await,
            Effect::FetchModels => self.fetch_models().await,
            Effect::SelectModel(model) => self.select_model(model).await,
            Effect::ForkConversation(parent_id) => self.fork_conversation(parent_id),
            Effect::SetWindowTitle(title) => self.set_window_title(&title),
            Effect::RefreshGoalSessions => self.refresh_goal_sessions().await,
            Effect::JoinGoalSession(id) => self.join_goal_session(id).await,
            Effect::SendGoalMessage { id, text } => self.send_goal_message(id, text).await,
            Effect::LeaveGoalSession => self.leave_goal_session(),
            Effect::Quit => self.quit(),
            Effect::None => {}
        }
    }

    /// `GET /api/goals` → populate the session switcher.
    async fn refresh_goal_sessions(&self) {
        let client = self.client.clone();
        let tx = self.action_tx.clone();
        let server = self.server_url();
        tokio::spawn(async move {
            match api::fetch_goal_sessions(&client, &server).await {
                Ok(sessions) => {
                    if tx.try_send(Action::GoalSessionsUpdate(sessions)).is_err() {
                        tracing::warn!("action channel full, dropping GoalSessionsUpdate");
                    }
                }
                Err(e) => tracing::warn!(error = %e, "fetch goal sessions failed"),
            }
        });
    }

    /// Abort any prior goal stream and open a fresh SSE subscription to `GET /api/goals/{id}/stream`,
    /// mapping each frame to a [`Action::GoalStreamEvent`]. Catch-up history arrives first, then live
    /// events (the server replays the transcript before tailing).
    async fn join_goal_session(&self, id: String) {
        // Replace any existing subscription (re-`/join` or switching sessions).
        if let Some(prev) = self.stream_state.lock().goal_handle.take() {
            prev.abort();
        }
        let client = self.client.clone();
        let tx = self.action_tx.clone();
        let server = self.server_url();
        let state = self.stream_state.clone();

        let handle = tokio::spawn(async move {
            let response = match api::open_goal_stream(&client, &server, &id).await {
                Ok(r) => r,
                Err(e) => {
                    let _ = tx
                        .try_send(Action::GoalStreamClosed(Some(format!(
                            "could not reach daemon: {e}"
                        ))));
                    state.lock().goal_handle = None;
                    return;
                }
            };
            if !response.status().is_success() {
                let status = response.status();
                let _ = tx.try_send(Action::GoalStreamClosed(Some(format!(
                    "server returned {status}"
                ))));
                state.lock().goal_handle = None;
                return;
            }

            let mut decoder = SseDecoder::default();
            let mut stream = response.bytes_stream();
            loop {
                match tokio::time::timeout(crate::tuning::SSE_STREAM_TIMEOUT, stream.next()).await {
                    Ok(Some(Ok(chunk))) => {
                        let text = String::from_utf8_lossy(&chunk);
                        for event in decoder.push(&text) {
                            match crate::sse::to_goal_event(&event) {
                                Ok(Some(ui)) => {
                                    let terminal =
                                        matches!(ui, crate::app::GoalUiEvent::Finished { .. });
                                    if tx.try_send(Action::GoalStreamEvent(ui)).is_err() {
                                        tracing::warn!("action channel full, dropping goal event");
                                    }
                                    if terminal {
                                        state.lock().goal_handle = None;
                                        return;
                                    }
                                }
                                Ok(None) => {}
                                Err(e) => tracing::warn!(error = %e, "goal stream decode error"),
                            }
                        }
                    }
                    Ok(Some(Err(e))) => {
                        let _ = tx.try_send(Action::GoalStreamClosed(Some(format!(
                            "stream error: {e}"
                        ))));
                        state.lock().goal_handle = None;
                        return;
                    }
                    Ok(None) => break, // stream ended (server closed after a terminal event)
                    Err(_elapsed) => {
                        let _ = tx.try_send(Action::GoalStreamClosed(Some(
                            "stream timeout — no data for 60s".into(),
                        )));
                        state.lock().goal_handle = None;
                        return;
                    }
                }
            }
            let _ = tx.try_send(Action::GoalStreamClosed(None));
            state.lock().goal_handle = None;
        });

        self.stream_state.lock().goal_handle = Some(handle.abort_handle());
    }

    /// `POST /api/goals/{id}/message` — deliver a human reply; report the outcome so 404/409 surface.
    async fn send_goal_message(&self, id: String, text: String) {
        let client = self.client.clone();
        let tx = self.action_tx.clone();
        let server = self.server_url();
        tokio::spawn(async move {
            let outcome = api::post_goal_message(&client, &server, &id, &text).await;
            if tx.try_send(Action::GoalMessageOutcome(outcome)).is_err() {
                tracing::warn!("action channel full, dropping GoalMessageOutcome");
            }
        });
    }

    /// Abort the joined session's SSE stream (on `/back`).
    fn leave_goal_session(&self) {
        if let Some(handle) = self.stream_state.lock().goal_handle.take() {
            handle.abort();
        }
    }

    async fn fetch_models(&self) {
        let server = self.server_url();
        let client = self.client.clone();
        let tx = self.action_tx.clone();
        match api::fetch_models(&client, &server).await {
            Ok(resp) => {
                let _ = tx
                    .send(Action::ModelsLoaded {
                        models: resp.models,
                        error: resp.error,
                    })
                    .await;
            }
            Err(e) => {
                let _ = tx
                    .send(Action::ModelsLoaded {
                        models: Vec::new(),
                        error: Some(format!("failed to fetch models: {e}")),
                    })
                    .await;
            }
        }
    }

    async fn select_model(&self, model: String) {
        let server = self.server_url();
        let client = self.client.clone();
        let tx = self.action_tx.clone();
        match api::select_model(&client, &server, &model).await {
            Ok(resp) => {
                let chosen = resp.current.unwrap_or(model);
                let _ = tx
                    .send(Action::ModelSelected {
                        model: chosen,
                        error: resp.error,
                    })
                    .await;
            }
            Err(e) => {
                let _ = tx
                    .send(Action::ModelSelected {
                        model,
                        error: Some(e.to_string()),
                    })
                    .await;
            }
        }
    }

    fn quit(&self) {
        self.should_quit.store(true, Ordering::Relaxed);
    }

    fn set_window_title(&self, title: &str) {
        let _ = execute!(io::stdout(), SetTitle(title));
    }

    fn fork_conversation(&self, parent_id: String) {
        tracing::info!(%parent_id, "fork requested (server support not yet available)");
    }

    fn cancel_stream(&self) {
        if let Some(handle) = self.stream_state.lock().handle.take() {
            handle.abort();
        }
    }

    async fn start_chat_stream(&self, message: String, session: Option<String>) {
        let client = self.client.clone();
        let tx = self.action_tx.clone();
        let server = self.server_url();
        let state = self.stream_state.clone();

        let handle = tokio::spawn(async move {
            let response =
                match api::post_chat_stream(&client, &server, &message, session.as_deref()).await {
                    Ok(r) => r,
                    Err(e) => {
                        if tx
                            .try_send(Action::SseFailed(format!(
                                "could not reach daemon at {server}: {e}"
                            )))
                            .is_err()
                        {
                            tracing::warn!("action channel full, dropping SseFailed");
                        }
                        state.lock().handle = None;
                        return;
                    }
                };

            if !response.status().is_success() {
                let status = response.status();
                let body = response.text().await.unwrap_or_default();
                if tx
                    .try_send(Action::SseFailed(format!(
                        "server returned {status}: {body}"
                    )))
                    .is_err()
                {
                    tracing::warn!("action channel full, dropping SseFailed");
                }
                state.lock().handle = None;
                return;
            }

            let mut decoder = SseDecoder::default();
            let mut stream = response.bytes_stream();
            loop {
                match tokio::time::timeout(crate::tuning::SSE_STREAM_TIMEOUT, stream.next()).await {
                    Ok(Some(Ok(chunk))) => {
                        let text = String::from_utf8_lossy(&chunk);
                        for event in decoder.push(&text) {
                            let action = event.to_action().unwrap_or_else(Action::SseFailed);
                            let is_terminal =
                                matches!(action, Action::SseDone | Action::SseFailed(_));
                            if tx.try_send(action).is_err() {
                                tracing::warn!("action channel full, dropping SSE action");
                                // SseDone/SseFailed are important — skip is_terminal check since the
                                // message may not have been delivered; the next chunk will retry
                                // (or timeout). For terminal events, continue to clean up handle.
                            }
                            if is_terminal {
                                state.lock().handle = None;
                                return;
                            }
                        }
                    }
                    Ok(Some(Err(e))) => {
                        if tx
                            .try_send(Action::SseFailed(format!("stream error: {e}")))
                            .is_err()
                        {
                            tracing::warn!("action channel full, dropping SseFailed");
                        }
                        state.lock().handle = None;
                        return;
                    }
                    Ok(None) => {
                        // stream ended naturally
                        break;
                    }
                    Err(_elapsed) => {
                        if tx
                            .try_send(Action::SseFailed(
                                "stream timeout — no data for 60s".to_string(),
                            ))
                            .is_err()
                        {
                            tracing::warn!("action channel full, dropping SseFailed");
                        }
                        state.lock().handle = None;
                        return;
                    }
                }
            }

            if tx.try_send(Action::SseDone).is_err() {
                tracing::warn!("action channel full, dropping SseDone");
            }
            state.lock().handle = None;
        });

        self.stream_state.lock().handle = Some(handle.abort_handle());
    }

    async fn refresh_conversations(&self) {
        let client = self.client.clone();
        let tx = self.action_tx.clone();
        let server = self.server_url();
        tokio::spawn(async move {
            match api::fetch_conversations(&client, &server).await {
                Ok(convs) => {
                    if tx.try_send(Action::ConversationsUpdate(convs)).is_err() {
                        tracing::warn!("action channel full, dropping ConversationsUpdate");
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, "refresh conversations failed");
                }
            }
        });
    }

    async fn load_conversation_history(&self, id: String) {
        let client = self.client.clone();
        let tx = self.action_tx.clone();
        let server = self.server_url();
        tokio::spawn(async move {
            match api::fetch_conversation_history(&client, &server, &id).await {
                Ok(Some(messages)) => {
                    if tx.try_send(Action::HistoryLoaded { id, messages }).is_err() {
                        tracing::warn!("action channel full, dropping HistoryLoaded");
                    }
                }
                Ok(None) => {
                    if tx
                        .try_send(Action::SseFailed("conversation not found".to_string()))
                        .is_err()
                    {
                        tracing::warn!("action channel full, dropping SseFailed");
                    }
                }
                Err(e) => {
                    if tx
                        .try_send(Action::SseFailed(format!("failed to load history: {e}")))
                        .is_err()
                    {
                        tracing::warn!("action channel full, dropping SseFailed");
                    }
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use liberado_theme::ThemeRegistry;
    use parking_lot::Mutex;
    use std::sync::Arc;
    use std::time::Duration;
    use tokio::sync::mpsc;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn make_app(server: &str) -> Arc<Mutex<App>> {
        Arc::new(Mutex::new(App::new(
            server.to_string(),
            ThemeRegistry::new(),
        )))
    }

    fn make_runner(app: Arc<Mutex<App>>, action_tx: mpsc::Sender<Action>) -> EffectRunner {
        EffectRunner {
            app,
            should_quit: Arc::new(AtomicBool::new(false)),
            action_tx,
            client: reqwest::Client::new(),
            stream_state: Arc::new(Mutex::new(StreamState::default())),
        }
    }

    #[tokio::test]
    async fn refresh_conversations_success() {
        let mock_server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/conversations"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {
                    "id": "c1",
                    "title": "test",
                    "created_at": "2025-06-25T12:00:00Z"
                }
            ])))
            .mount(&mock_server)
            .await;

        let app = make_app(&mock_server.uri());
        let (action_tx, mut action_rx) = mpsc::channel(256);
        let runner = make_runner(app, action_tx);

        runner.run(Effect::RefreshConversations).await;

        let action = tokio::time::timeout(Duration::from_secs(2), action_rx.recv())
            .await
            .expect("timeout waiting for action")
            .expect("channel closed");

        match action {
            Action::ConversationsUpdate(convs) => {
                assert_eq!(convs.len(), 1);
                assert_eq!(convs[0].id, "c1");
                assert_eq!(convs[0].title, Some("test".into()));
            }
            other => panic!("expected ConversationsUpdate, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn refresh_conversations_error() {
        let mock_server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/conversations"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&mock_server)
            .await;

        let app = make_app(&mock_server.uri());
        let (action_tx, mut action_rx) = mpsc::channel(256);
        let runner = make_runner(app, action_tx);

        runner.run(Effect::RefreshConversations).await;

        // The error is only traced, no action sent
        let result = tokio::time::timeout(Duration::from_millis(500), action_rx.recv()).await;
        assert!(result.is_err(), "expected no action but got one");
    }

    #[tokio::test]
    async fn load_conversation_history_success() {
        let mock_server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/conversations/c1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "messages": [
                    {"role": "user", "content": "hi"}
                ]
            })))
            .mount(&mock_server)
            .await;

        let app = make_app(&mock_server.uri());
        let (action_tx, mut action_rx) = mpsc::channel(256);
        let runner = make_runner(app, action_tx);

        runner
            .run(Effect::LoadConversationHistory("c1".into()))
            .await;

        let action = tokio::time::timeout(Duration::from_secs(2), action_rx.recv())
            .await
            .expect("timeout waiting for action")
            .expect("channel closed");

        match action {
            Action::HistoryLoaded { id, messages } => {
                assert_eq!(id, "c1");
                assert_eq!(messages.len(), 1);
                assert_eq!(messages[0].role, "user");
                assert_eq!(messages[0].content, "hi");
            }
            other => panic!("expected HistoryLoaded, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn load_conversation_history_not_found() {
        let mock_server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/conversations/c1"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&mock_server)
            .await;

        let app = make_app(&mock_server.uri());
        let (action_tx, mut action_rx) = mpsc::channel(256);
        let runner = make_runner(app, action_tx);

        runner
            .run(Effect::LoadConversationHistory("c1".into()))
            .await;

        let action = tokio::time::timeout(Duration::from_secs(2), action_rx.recv())
            .await
            .expect("timeout waiting for action")
            .expect("channel closed");

        match action {
            Action::SseFailed(msg) => {
                assert_eq!(msg, "conversation not found");
            }
            other => panic!("expected SseFailed('conversation not found'), got {other:?}"),
        }
    }

    #[tokio::test]
    async fn cancel_stream() {
        let app = make_app("http://localhost:0");
        let (action_tx, _action_rx) = mpsc::channel(256);
        let stream_state = Arc::new(Mutex::new(StreamState::default()));
        let runner = EffectRunner {
            app,
            should_quit: Arc::new(AtomicBool::new(false)),
            action_tx,
            client: reqwest::Client::new(),
            stream_state: stream_state.clone(),
        };

        // Store a real abort handle
        let handle = tokio::spawn(async {}).abort_handle();
        stream_state.lock().handle = Some(handle);

        runner.run(Effect::CancelStream).await;

        assert!(
            stream_state.lock().handle.is_none(),
            "handle should be taken after CancelStream"
        );
    }

    #[tokio::test]
    async fn fork_conversation_is_noop() {
        let app = make_app("http://localhost:0");
        let (action_tx, mut action_rx) = mpsc::channel(256);
        let runner = make_runner(app, action_tx);

        runner.run(Effect::ForkConversation("c1".into())).await;

        let result = tokio::time::timeout(Duration::from_millis(500), action_rx.recv()).await;
        assert!(result.is_err(), "expected no action but got one");
    }

    #[tokio::test]
    async fn quit_sets_should_quit() {
        let should_quit = Arc::new(AtomicBool::new(false));
        let app = make_app("http://localhost:0");
        let (action_tx, _action_rx) = mpsc::channel(256);
        let runner = EffectRunner {
            app,
            should_quit: should_quit.clone(),
            action_tx,
            client: reqwest::Client::new(),
            stream_state: Arc::new(Mutex::new(StreamState::default())),
        };

        runner.run(Effect::Quit).await;

        assert!(
            should_quit.load(Ordering::Relaxed),
            "should_quit should be true after Quit"
        );
    }
}
