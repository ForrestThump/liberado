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
/// Decode one SSE text chunk into the actions to forward, stopping at a terminal action
/// (`SseDone` / `SseFailed`). Returns the actions and whether a terminal was seen.
fn send_or_warn(tx: &mpsc::Sender<Action>, action: Action, label: &str) -> bool {
    let failed = tx.try_send(action).is_err();
    if failed {
        tracing::warn!("action channel full, dropping {label}");
    }
    failed
}

fn sse_actions_from_text(decoder: &mut SseDecoder, text: &str) -> (Vec<Action>, bool) {
    let mut actions = Vec::new();
    let mut terminal = false;
    for event in decoder.push(text) {
        let action = event.to_action().unwrap_or_else(Action::SseFailed);
        if matches!(action, Action::SseDone | Action::SseFailed(_)) {
            terminal = true;
        }
        actions.push(action);
        if terminal {
            break;
        }
    }
    (actions, terminal)
}

pub struct EffectRunner {
    pub app: Arc<Mutex<App>>,
    pub should_quit: Arc<AtomicBool>,
    pub action_tx: mpsc::Sender<Action>,
    pub client: reqwest::Client,
    pub stream_state: Arc<Mutex<StreamState>>,
}

/// The (id, action, past-tense label) for the park/cancel goal actions.
fn goal_action_args(effect: Effect) -> (String, &'static str, &'static str) {
    match effect {
        Effect::ParkGoalSession(id) => (id, "park", "parked"),
        Effect::CancelGoalSession(id) => (id, "cancel", "cancelled"),
        _ => unreachable!("goal_action_args only receives park/cancel"),
    }
}

impl EffectRunner {
    fn server_url(&self) -> String {
        self.app.lock().server.clone()
    }

    /// Execute one effect. Async effects (SSE, HTTP) spawn their work and return;
    /// the caller should `.await` the returned future.
    pub async fn run(&self, effect: Effect) {
        match effect {
            Effect::SetWindowTitle(title) => self.set_window_title(&title),
            Effect::Quit => self.quit(),
            Effect::None => {}
            _ => self.run_async(effect).await,
        }
    }

    /// Dispatch the async (network / streaming) effects. The terminal and sync side-effects live
    /// in [`run`](Self::run), which routes everything else here.
    async fn run_async(&self, effect: Effect) {
        match effect {
            Effect::StartChatStream { message, session } => {
                self.start_chat_stream(message, session).await
            }
            Effect::CancelStream { conversation } => self.cancel_stream(conversation).await,
            Effect::RefreshConversations => self.refresh_conversations().await,
            Effect::LoadConversationHistory(id) => self.load_conversation_history(id).await,
            Effect::FetchModels => self.fetch_models().await,
            Effect::SelectModel {
                model,
                conversation,
            } => self.select_model(model, conversation).await,
            Effect::AttachConversationStream(id) => self.attach_conversation_stream(id).await,
            Effect::ForkConversation {
                parent_id,
                after_turn,
            } => self.fork_conversation(parent_id, after_turn).await,
            Effect::RefreshSessions => self.refresh_sessions().await,
            Effect::JoinGoalSession(id) => self.join_goal_session(id).await,
            Effect::SendGoalMessage { id, text } => self.send_goal_message(id, text).await,
            Effect::SpawnGoalSession {
                domain,
                goal,
                origin_conversation,
            } => {
                self.spawn_goal_session(domain, goal, origin_conversation)
                    .await
            }
            Effect::StartCodingGoal {
                project,
                text,
                mode,
                origin_conversation,
            } => {
                self.start_coding_goal(project, text, mode, origin_conversation)
                    .await
            }
            Effect::ParkGoalSession(_) | Effect::CancelGoalSession(_) => {
                let (id, action, past_tense) = goal_action_args(effect);
                self.goal_action(id, action, past_tense).await
            }
            Effect::ResumeGoalSession { id, answer } => self.resume_goal_session(id, answer).await,
            Effect::LeaveGoalSession => self.leave_goal_session(),
            _ => unreachable!("run_async only receives the async effect kinds"),
        }
    }

    /// `GET /api/goals` → populate the session switcher.
    async fn refresh_sessions(&self) {
        let client = self.client.clone();
        let tx = self.action_tx.clone();
        let server = self.server_url();
        tokio::spawn(async move {
            match api::fetch_sessions(&client, &server).await {
                Ok(sessions) => {
                    if tx.try_send(Action::SessionsUpdate(sessions)).is_err() {
                        tracing::warn!("action channel full, dropping SessionsUpdate");
                    }
                }
                Err(e) => tracing::warn!(error = %e, "fetch goal sessions failed"),
            }
        });
    }

    /// Abort any prior goal stream and open a fresh SSE subscription to `GET /api/goals/{id}/stream`,
    /// mapping each frame to a [`Action::GoalStreamEvent`]. Catch-up history arrives first, then live
    /// events (the server replays the transcript before tailing).
    /// Decode one goal-session SSE text chunk into the actions to forward, stopping at a
    /// terminal `Finished` event. Returns the actions and whether a terminal was seen.
    fn goal_actions_from_chunk(decoder: &mut SseDecoder, text: &str) -> (Vec<Action>, bool) {
        let mut actions = Vec::new();
        let mut terminal = false;
        for event in decoder.push(text) {
            match crate::sse::to_goal_event(&event) {
                Ok(Some(ui)) => {
                    if matches!(ui, crate::app::GoalUiEvent::Finished { .. }) {
                        terminal = true;
                    }
                    actions.push(Action::GoalStreamEvent(ui));
                }
                Ok(None) => {}
                Err(e) => tracing::warn!(error = %e, "goal stream decode error"),
            }
            if terminal {
                break;
            }
        }
        (actions, terminal)
    }

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
                    let _ = tx.try_send(Action::GoalStreamClosed(Some(format!(
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
                        let (actions, terminal) =
                            EffectRunner::goal_actions_from_chunk(&mut decoder, &text);
                        for action in actions {
                            if tx.try_send(action).is_err() {
                                tracing::warn!("action channel full, dropping goal event");
                            }
                        }
                        if terminal {
                            state.lock().goal_handle = None;
                            return;
                        }
                    }
                    Ok(Some(Err(e))) => {
                        let _ = tx
                            .try_send(Action::GoalStreamClosed(Some(format!("stream error: {e}"))));
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

    /// `/spawn` — `POST /api/goals` to create an interactive session, then hand the id back as
    /// `GoalSpawned` (the App focuses it and opens its stream). `GoalSpawnFailed` on error.
    /// `/goal <text>` — start a coding goal and focus it. Reuses the spawn actions so the new
    /// session lands in the same joined pane a `/spawn` would.
    async fn start_coding_goal(
        &self,
        project: Option<String>,
        text: String,
        mode: Option<liberado_commands::CodingGoalMode>,
        origin_conversation: Option<String>,
    ) {
        let client = self.client.clone();
        let tx = self.action_tx.clone();
        let server = self.server_url();
        tokio::spawn(async move {
            let action = match api::start_coding_goal(
                &client,
                &server,
                project.as_deref(),
                &text,
                mode,
                origin_conversation.as_deref(),
            )
            .await
            {
                Ok(session_id) => Action::GoalSpawned {
                    session_id,
                    domain: "coding".to_string(),
                    description: text,
                },
                Err(e) => Action::GoalSpawnFailed(e),
            };
            if tx.try_send(action).is_err() {
                tracing::warn!("action channel full, dropping coding-goal result");
            }
        });
    }

    /// `park` / `cancel` — bodiless lifecycle verbs. Reports through the same system-message
    /// channel either way, because "I asked and it refused" has to be as visible as success.
    async fn goal_action(&self, id: String, action: &'static str, past_tense: &'static str) {
        let client = self.client.clone();
        let tx = self.action_tx.clone();
        let server = self.server_url();
        tokio::spawn(async move {
            let msg = match api::post_goal_action(&client, &server, &id, action).await {
                Ok(()) => format!("Session {id} {past_tense}."),
                Err(e) => format!("Could not {action} session {id}: {e}"),
            };
            if tx.try_send(Action::SystemMessage(msg)).is_err() {
                tracing::warn!("action channel full, dropping goal-action result");
            }
        });
    }

    /// `/goal resume [answer]` — deliver the answer a parked session is waiting for.
    async fn resume_goal_session(&self, id: String, answer: String) {
        if answer.is_empty() {
            let _ = self.action_tx.try_send(Action::SystemMessage(
                "Usage: /goal resume <your answer to the question the session is holding>".into(),
            ));
            return;
        }
        self.send_goal_message(id, answer).await;
    }

    async fn spawn_goal_session(
        &self,
        domain: String,
        goal: String,
        origin_conversation: Option<String>,
    ) {
        let client = self.client.clone();
        let tx = self.action_tx.clone();
        let server = self.server_url();
        tokio::spawn(async move {
            let action = match api::spawn_goal(
                &client,
                &server,
                &domain,
                &goal,
                origin_conversation.as_deref(),
            )
            .await
            {
                Ok(session_id) => Action::GoalSpawned {
                    session_id,
                    domain,
                    description: goal,
                },
                Err(e) => Action::GoalSpawnFailed(e),
            };
            if tx.try_send(action).is_err() {
                tracing::warn!("action channel full, dropping spawn result");
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

    async fn select_model(&self, model: String, conversation: Option<String>) {
        let server = self.server_url();
        let client = self.client.clone();
        let tx = self.action_tx.clone();
        let conversation_scoped = conversation.is_some();
        match api::select_model(&client, &server, &model, conversation.as_deref()).await {
            Ok(resp) => {
                let chosen = resp.current.unwrap_or(model);
                let _ = tx
                    .send(Action::ModelSelected {
                        model: chosen,
                        error: resp.error,
                        conversation_scoped,
                    })
                    .await;
            }
            Err(e) => {
                let _ = tx
                    .send(Action::ModelSelected {
                        model,
                        error: Some(e.to_string()),
                        conversation_scoped,
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

    /// Branch `parent_id`, then **land the user in the branch**. The original is untouched and is
    /// still in the switcher — forking is not moving.
    async fn fork_conversation(&self, parent_id: String, after_turn: Option<u32>) {
        let client = self.client.clone();
        let tx = self.action_tx.clone();
        let server = self.server_url();
        tokio::spawn(async move {
            let action =
                match api::fork_conversation(&client, &server, &parent_id, after_turn).await {
                    Ok(fork) => Action::Forked(fork),
                    Err(e) => Action::SseFailed(format!("fork failed: {e}")),
                };
            if tx.try_send(action).is_err() {
                tracing::warn!("action channel full, dropping fork result");
            }
        });
    }

    /// Abort the local SSE reader and cancel the durable turn on the daemon when we know which
    /// conversation is open. Local abort alone is "stop showing me"; the POST is "stop doing this".
    async fn cancel_stream(&self, conversation: Option<String>) {
        if let Some(handle) = self.stream_state.lock().handle.take() {
            handle.abort();
        }
        let Some(id) = conversation else {
            return;
        };
        let client = self.client.clone();
        let server = self.server_url();
        if let Err(e) = api::cancel_conversation(&client, &server, &id).await {
            tracing::warn!(error = %e, conversation = %id, "conversation cancel request failed");
            let _ = self
                .action_tx
                .try_send(Action::SystemMessage(format!("cancel request failed: {e}")));
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
                        send_or_warn(
                            &tx,
                            Action::SseFailed(format!("could not reach daemon at {server}: {e}")),
                            "SseFailed",
                        );
                        state.lock().handle = None;
                        return;
                    }
                };

            if !response.status().is_success() {
                let status = response.status();
                let body = response.text().await.unwrap_or_default();
                send_or_warn(
                    &tx,
                    Action::SseFailed(format!("server returned {status}: {body}")),
                    "SseFailed",
                );
                state.lock().handle = None;
                return;
            }

            let mut decoder = SseDecoder::default();
            let mut stream = response.bytes_stream();
            loop {
                match tokio::time::timeout(crate::tuning::SSE_STREAM_TIMEOUT, stream.next()).await {
                    Ok(Some(Ok(chunk))) => {
                        let text = String::from_utf8_lossy(&chunk);
                        let (actions, terminal) = sse_actions_from_text(&mut decoder, &text);
                        for action in actions {
                            // SseDone/SseFailed are important — skip is_terminal check since the
                            // message may not have been delivered; the next chunk will retry
                            // (or timeout). For terminal events, continue to clean up handle.
                            send_or_warn(&tx, action, "SSE action");
                        }
                        if terminal {
                            state.lock().handle = None;
                            return;
                        }
                    }
                    Ok(Some(Err(e))) => {
                        send_or_warn(
                            &tx,
                            Action::SseFailed(format!("stream error: {e}")),
                            "SseFailed",
                        );
                        state.lock().handle = None;
                        return;
                    }
                    Ok(None) => {
                        // stream ended naturally
                        break;
                    }
                    Err(_elapsed) => {
                        send_or_warn(
                            &tx,
                            Action::SseFailed("stream timeout — no data for 60s".to_string()),
                            "SseFailed",
                        );
                        state.lock().handle = None;
                        return;
                    }
                }
            }

            send_or_warn(&tx, Action::SseDone, "SseDone");
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
                Ok(Some(history)) => {
                    if tx
                        .try_send(Action::HistoryLoaded {
                            id,
                            messages: history.messages,
                            turn_running: history.turn_running,
                            turn_unanswered: history.turn_unanswered,
                        })
                        .is_err()
                    {
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

    /// Rejoin a running turn via `GET /api/conversations/{id}/attach`. Uses the same SSE decoder
    /// and `SessionEvent::from_sse_data` path as a normal chat stream — no forked vocabulary.
    async fn attach_conversation_stream(&self, id: String) {
        let client = self.client.clone();
        let tx = self.action_tx.clone();
        let server = self.server_url();
        let state = self.stream_state.clone();

        let handle = tokio::spawn(async move {
            let response = match api::attach_conversation_stream(&client, &server, &id).await {
                Ok(r) => r,
                Err(e) => {
                    send_or_warn(
                        &tx,
                        Action::SseFailed(format!("could not attach to running turn: {e}")),
                        "SseFailed",
                    );
                    state.lock().handle = None;
                    return;
                }
            };

            // 409 is the expected race, not a fault: `turn_running` was read a moment ago and the
            // turn finished before the attach landed. The reply exists — it is simply already on
            // disk rather than on the wire — so end the stream state and reload the transcript.
            // Reporting it as `[error]` would blame the user's own answer arriving on time.
            if response.status() == reqwest::StatusCode::CONFLICT {
                state.lock().handle = None;
                send_or_warn(&tx, Action::SseDone, "SseDone");
                send_or_warn(
                    &tx,
                    Action::ReloadConversationHistory(id.clone()),
                    "history reload after 409",
                );
                return;
            }

            if !response.status().is_success() {
                let status = response.status();
                let body = response.text().await.unwrap_or_default();
                send_or_warn(
                    &tx,
                    Action::SseFailed(format!("attach refused ({status}): {body}")),
                    "SseFailed",
                );
                state.lock().handle = None;
                return;
            }

            let mut decoder = SseDecoder::default();
            let mut stream = response.bytes_stream();
            loop {
                match tokio::time::timeout(crate::tuning::SSE_STREAM_TIMEOUT, stream.next()).await {
                    Ok(Some(Ok(chunk))) => {
                        let text = String::from_utf8_lossy(&chunk);
                        let (actions, terminal) = sse_actions_from_text(&mut decoder, &text);
                        for action in actions {
                            // Shared decode: same `ToAction` / `from_sse_data` as chat stream.
                            let _ = send_or_warn(&tx, action, "attach SSE action");
                        }
                        if terminal {
                            state.lock().handle = None;
                            return;
                        }
                    }
                    Ok(Some(Err(e))) => {
                        send_or_warn(
                            &tx,
                            Action::SseFailed(format!("attach stream error: {e}")),
                            "SseFailed",
                        );
                        state.lock().handle = None;
                        return;
                    }
                    Ok(None) => break,
                    Err(_elapsed) => {
                        send_or_warn(
                            &tx,
                            Action::SseFailed(
                                "attach stream timeout — no data for 60s".to_string(),
                            ),
                            "SseFailed",
                        );
                        state.lock().handle = None;
                        return;
                    }
                }
            }

            send_or_warn(&tx, Action::SseDone, "SseDone");
            state.lock().handle = None;
        });

        self.stream_state.lock().handle = Some(handle.abort_handle());
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
    use wiremock::matchers::{body_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn make_app(server: &str) -> Arc<Mutex<App>> {
        let mut app = App::new(server.to_string(), ThemeRegistry::new());
        app.settings_path = None; // never touch the user's real config during tests
        Arc::new(Mutex::new(app))
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
            Action::HistoryLoaded {
                id,
                messages,
                turn_running,
                turn_unanswered,
            } => {
                assert_eq!(id, "c1");
                assert_eq!(messages.len(), 1);
                assert_eq!(messages[0].role, "user");
                assert_eq!(messages[0].content, "hi");
                assert!(!turn_running);
                assert!(!turn_unanswered);
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
    async fn cancel_stream_aborts_local_reader_without_conversation() {
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

        runner
            .run(Effect::CancelStream { conversation: None })
            .await;

        assert!(
            stream_state.lock().handle.is_none(),
            "handle should be taken after CancelStream"
        );
    }

    /// Stop must hit the daemon cancel endpoint — local SSE abort alone is not enough for durable turns.
    #[tokio::test]
    async fn cancel_stream_posts_conversation_cancel() {
        let mock_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/conversations/conv-abc/cancel"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .expect(1)
            .mount(&mock_server)
            .await;

        let app = make_app(&mock_server.uri());
        let (action_tx, _action_rx) = mpsc::channel(256);
        let stream_state = Arc::new(Mutex::new(StreamState::default()));
        let runner = EffectRunner {
            app,
            should_quit: Arc::new(AtomicBool::new(false)),
            action_tx,
            client: reqwest::Client::new(),
            stream_state: stream_state.clone(),
        };
        let handle = tokio::spawn(async { tokio::time::sleep(Duration::from_secs(60)).await })
            .abort_handle();
        stream_state.lock().handle = Some(handle);

        runner
            .run(Effect::CancelStream {
                conversation: Some("conv-abc".into()),
            })
            .await;

        assert!(stream_state.lock().handle.is_none());
        // wiremock expect(1) asserts the cancel POST was made when the mock drops.
    }

    #[tokio::test]
    async fn select_model_daemon_wide_omits_conversation_field() {
        let mock_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/models/select"))
            .and(body_json(serde_json::json!({ "model": "m1" })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "models": [],
                "current": "m1",
                "error": null
            })))
            .expect(1)
            .mount(&mock_server)
            .await;

        let app = make_app(&mock_server.uri());
        let (action_tx, mut action_rx) = mpsc::channel(256);
        let runner = make_runner(app, action_tx);

        runner
            .run(Effect::SelectModel {
                model: "m1".into(),
                conversation: None,
            })
            .await;

        let action = tokio::time::timeout(Duration::from_secs(2), action_rx.recv())
            .await
            .expect("timeout")
            .expect("channel closed");
        match action {
            Action::ModelSelected {
                model,
                error: None,
                conversation_scoped: false,
            } => assert_eq!(model, "m1"),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[tokio::test]
    async fn select_model_with_open_conversation_includes_conversation_field() {
        let mock_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/models/select"))
            .and(body_json(serde_json::json!({
                "model": "m2",
                "conversation": "conv-99"
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "models": [],
                "current": "m2",
                "error": null
            })))
            .expect(1)
            .mount(&mock_server)
            .await;

        let app = make_app(&mock_server.uri());
        let (action_tx, mut action_rx) = mpsc::channel(256);
        let runner = make_runner(app, action_tx);

        runner
            .run(Effect::SelectModel {
                model: "m2".into(),
                conversation: Some("conv-99".into()),
            })
            .await;

        let action = tokio::time::timeout(Duration::from_secs(2), action_rx.recv())
            .await
            .expect("timeout")
            .expect("channel closed");
        match action {
            Action::ModelSelected {
                model,
                error: None,
                conversation_scoped: true,
            } => assert_eq!(model, "m2"),
            other => panic!("unexpected {other:?}"),
        }
    }

    /// Reattach must actually reach the daemon and decode what comes back.
    ///
    /// The app-level tests assert only that `Effect::AttachConversationStream` is *emitted*.
    /// Gutting the effect's body left all 278 tests passing — so the feature whose absence was
    /// the regression had no coverage below the effect boundary at all.
    #[tokio::test]
    async fn attach_stream_replays_the_running_turn() {
        let mock_server = MockServer::start().await;
        // Replay-then-live, in the shared SSE vocabulary. `token` frames are what a live turn
        // emits; the attach endpoint replays them before continuing.
        let body = "event: token\ndata: re\n\n\
                    event: token\ndata: joined\n\n\
                    event: session_finished\ndata: {\"status\":\"done\",\"summary\":\"\"}\n\n";
        Mock::given(method("GET"))
            .and(path("/api/conversations/live-7/attach"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_string(body),
            )
            .expect(1)
            .mount(&mock_server)
            .await;

        let app = make_app(&mock_server.uri());
        let (action_tx, mut action_rx) = mpsc::channel(256);
        let runner = make_runner(app, action_tx);

        runner
            .run(Effect::AttachConversationStream("live-7".into()))
            .await;

        let mut tokens = String::new();
        let mut saw_done = false;
        for _ in 0..8 {
            match tokio::time::timeout(Duration::from_secs(2), action_rx.recv()).await {
                Ok(Some(Action::SseToken(t))) => tokens.push_str(&t),
                Ok(Some(Action::SseDone)) => {
                    saw_done = true;
                    break;
                }
                Ok(Some(Action::SseFailed(e))) => panic!("attach failed: {e}"),
                Ok(Some(_)) => {}
                Ok(None) => break,
                Err(_) => panic!("timed out waiting for attach actions; got {tokens:?}"),
            }
        }
        assert_eq!(tokens, "rejoined", "replayed tokens must reach the app");
        assert!(saw_done, "the attach stream must terminate the turn");
    }

    /// A turn that finishes between reading `turn_running` and the attach request is the expected
    /// race, not a fault. It must reload the transcript — the reply is on disk — rather than
    /// blaming the user with `[error] attach refused (409)`.
    #[tokio::test]
    async fn attach_409_reloads_history_instead_of_reporting_an_error() {
        let mock_server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/conversations/done-3/attach"))
            .respond_with(ResponseTemplate::new(409).set_body_json(serde_json::json!({
                "error": "no turn is running for this conversation"
            })))
            .expect(1)
            .mount(&mock_server)
            .await;

        let app = make_app(&mock_server.uri());
        let (action_tx, mut action_rx) = mpsc::channel(256);
        let runner = make_runner(app, action_tx);

        runner
            .run(Effect::AttachConversationStream("done-3".into()))
            .await;

        let mut saw_reload = false;
        for _ in 0..4 {
            match tokio::time::timeout(Duration::from_secs(2), action_rx.recv()).await {
                Ok(Some(Action::SseFailed(e))) => {
                    panic!("a finished turn must not surface as an error: {e}")
                }
                Ok(Some(Action::ReloadConversationHistory(id))) => {
                    assert_eq!(id, "done-3");
                    saw_reload = true;
                    break;
                }
                Ok(Some(_)) => {}
                Ok(None) => break,
                Err(_) => break,
            }
        }
        assert!(
            saw_reload,
            "409 must re-read the transcript so the reply that just landed is shown"
        );
    }

    #[tokio::test]
    async fn load_history_forwards_turn_lifecycle_flags() {
        let mock_server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/conversations/c-run"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "messages": [{"role": "user", "content": "still going?"}],
                "turn_running": true,
                "turn_unanswered": false
            })))
            .mount(&mock_server)
            .await;

        let app = make_app(&mock_server.uri());
        let (action_tx, mut action_rx) = mpsc::channel(256);
        let runner = make_runner(app, action_tx);

        runner
            .run(Effect::LoadConversationHistory("c-run".into()))
            .await;

        let action = tokio::time::timeout(Duration::from_secs(2), action_rx.recv())
            .await
            .expect("timeout")
            .expect("channel closed");
        match action {
            Action::HistoryLoaded {
                id,
                turn_running: true,
                turn_unanswered: false,
                ..
            } => assert_eq!(id, "c-run"),
            other => panic!("expected running HistoryLoaded, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn fork_conversation_posts_the_branch_point_and_reports_the_new_session() {
        // This effect used to log "server support not yet available" and return. It now actually
        // forks — the branch point rides in the body as a turn number, not a node id, because a turn
        // is the thing a human can see and point at.
        let mock_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/sessions/c1/fork"))
            .and(body_json(serde_json::json!({ "after_turn": 2 })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": "c2",
                "forked_from": "c1",
                "kept_turns": 2,
                "total_turns": 5
            })))
            .mount(&mock_server)
            .await;

        let app = make_app(&mock_server.uri());
        let (action_tx, mut action_rx) = mpsc::channel(256);
        let runner = make_runner(app, action_tx);

        runner
            .run(Effect::ForkConversation {
                parent_id: "c1".into(),
                after_turn: Some(2),
            })
            .await;

        let action = tokio::time::timeout(Duration::from_secs(2), action_rx.recv())
            .await
            .expect("timeout waiting for the fork result")
            .expect("channel closed");

        match action {
            Action::Forked(fork) => {
                assert_eq!(fork.id, "c2");
                assert_eq!(fork.kept_turns, 2);
                assert_eq!(fork.total_turns, 5);
            }
            other => panic!("expected Forked, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_failed_fork_surfaces_the_servers_own_reason() {
        // "session has no message transcript to fork" is the useful message; a bare 400 is not.
        let mock_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/sessions/g1/fork"))
            .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
                "error": "session g1 has no message transcript to fork"
            })))
            .mount(&mock_server)
            .await;

        let app = make_app(&mock_server.uri());
        let (action_tx, mut action_rx) = mpsc::channel(256);
        let runner = make_runner(app, action_tx);

        runner
            .run(Effect::ForkConversation {
                parent_id: "g1".into(),
                after_turn: None,
            })
            .await;

        let action = tokio::time::timeout(Duration::from_secs(2), action_rx.recv())
            .await
            .expect("timeout")
            .expect("channel closed");

        match action {
            Action::SseFailed(msg) => assert!(
                msg.contains("no message transcript"),
                "the server's reason must reach the human, got: {msg}"
            ),
            other => panic!("expected SseFailed, got {other:?}"),
        }
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

    #[test]
    fn sse_text_chunk_decodes_events_into_actions() {
        let mut decoder = SseDecoder::default();
        let (actions, terminal) = sse_actions_from_text(
            &mut decoder,
            "event: token
data: hello

",
        );
        assert!(!terminal);
        assert_eq!(actions.len(), 1);
        assert!(matches!(actions[0], Action::SseToken(_)));
    }

    #[test]
    fn sse_text_chunk_stops_at_session_finished() {
        let mut decoder = SseDecoder::default();
        let text = "event: token\ndata: hi\n\nevent: session_finished\ndata: {\"status\":\"ok\",\"summary\":\"done\"}\n\n";
        let (actions, terminal) = sse_actions_from_text(&mut decoder, text);
        assert!(terminal);
        assert_eq!(actions.len(), 2);
        assert!(matches!(actions[0], Action::SseToken(_)));
        assert!(matches!(actions[1], Action::SseDone));
    }

    #[test]
    fn sse_text_chunk_flags_failed_as_terminal() {
        let mut decoder = SseDecoder::default();
        let (actions, terminal) = sse_actions_from_text(
            &mut decoder,
            "event: failed
data: {\"message\":\"boom\"}

",
        );
        assert!(terminal);
        assert_eq!(actions.len(), 1);
        assert!(matches!(actions[0], Action::SseFailed(_)));
    }

    #[test]
    fn sse_text_chunk_unknown_kind_is_a_benign_noop() {
        let mut decoder = SseDecoder::default();
        let (actions, terminal) = sse_actions_from_text(
            &mut decoder,
            "event: no_such_event
data: x

",
        );
        assert!(!terminal);
        assert_eq!(actions.len(), 1);
        assert!(matches!(actions[0], Action::SseToken(_)));
    }

    #[test]
    fn sse_text_chunk_malformed_known_event_yields_sse_failed() {
        let mut decoder = SseDecoder::default();
        let (actions, terminal) = sse_actions_from_text(
            &mut decoder,
            "event: tool_started
data: not-json

",
        );
        assert!(terminal);
        assert_eq!(actions.len(), 1);
        assert!(matches!(actions[0], Action::SseFailed(_)));
    }

    #[test]
    fn goal_chunk_forwards_token_and_finished() {
        let mut decoder = SseDecoder::default();
        let text = "event: session_started\ndata: {\"domain\":\"coding\",\"description\":\"fix tests\"}\n\nevent: session_finished\ndata: {\"status\":\"ok\",\"summary\":\"done\"}\n\n";
        let (actions, terminal) = EffectRunner::goal_actions_from_chunk(&mut decoder, text);
        assert!(terminal);
        assert_eq!(actions.len(), 2);
        assert!(matches!(actions[0], Action::GoalStreamEvent(_)));
        assert!(matches!(actions[1], Action::GoalStreamEvent(_)));
    }

    #[test]
    fn goal_chunk_token_is_not_terminal() {
        let mut decoder = SseDecoder::default();
        let (actions, terminal) =
            EffectRunner::goal_actions_from_chunk(&mut decoder, "event: token\ndata: hello\n\n");
        assert!(!terminal);
        assert_eq!(actions.len(), 1);
        assert!(matches!(actions[0], Action::GoalStreamEvent(_)));
    }

    #[test]
    fn goal_chunk_unknown_kind_is_benign() {
        let mut decoder = SseDecoder::default();
        let (actions, terminal) = EffectRunner::goal_actions_from_chunk(
            &mut decoder,
            "event: no_such_event\ndata: x\n\n",
        );
        assert!(!terminal);
        // The chat wire decodes unknown event types to an empty Token no-op (never an error).
        assert_eq!(actions.len(), 1);
        assert!(matches!(actions[0], Action::GoalStreamEvent(_)));
    }
}
