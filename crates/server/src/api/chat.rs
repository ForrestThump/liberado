//! Chat + conversation history endpoints.

use std::convert::Infallible;
use std::pin::Pin;
use std::sync::Arc;

use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
    response::{
        IntoResponse,
        sse::{Event, Sse},
    },
};
use futures::{Stream, StreamExt};
use liberado_conversation_store::Ulid;
use liberado_executor::AgentEvent;
use serde::Deserialize;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

use chat_client_contract::{ApiError, ChatMessage, ConversationHistoryResponse};

use crate::state::AppState;
#[derive(Deserialize)]
pub struct ChatRequest {
    pub message: String,
    /// The conversation to continue. Absent on the first message of a chat (a new conversation is
    /// created and its id returned to the client). `Ulid` deserializes from its string form, so this
    /// works for both the JSON body and the `?session=â€¦` query.
    #[serde(default)]
    pub session: Option<Ulid>,
    /// Open a RAM-only session for this chat: nothing written to disk, nothing in any listing.
    ///
    /// Only consulted when `session` is absent — it describes how to *create* one, so passing it
    /// alongside an existing id cannot retroactively make that conversation private and is simply
    /// ignored rather than quietly half-honored.
    #[serde(default)]
    pub incognito: bool,
}

/// Streaming chat â€” the shared client contract (see `docs/reference/api.md`). Returns
/// `text/event-stream`; events use the converged session-event vocabulary: `token` (answer
/// delta), `tool_started` (`{name,args_preview}`), `tool_finished` (`{name,ok,result_preview}`),
/// `session_finished` (`{status,summary}`), `failed` (`{message}`). Available as both `POST`
/// (JSON body â€” native clients) and `GET` (`?message=â€¦` â€” browser
/// `EventSource`); both funnel through [`chat_stream_core`].
pub async fn chat_stream_post(
    State(state): State<Arc<AppState>>,
    Json(req): Json<ChatRequest>,
) -> Sse<SseBody> {
    chat_stream_core(state, req.message, req.session, req.incognito).await
}

/// `GET /api/chat/stream?message=â€¦` â€” the `EventSource`-friendly variant (browsers can't `POST` an
/// `EventSource`). Same SSE contract as the POST form, including `?session=â€¦` to continue a chat.
pub async fn chat_stream_get(
    State(state): State<Arc<AppState>>,
    Query(req): Query<ChatRequest>,
) -> Sse<SseBody> {
    chat_stream_core(state, req.message, req.session, req.incognito).await
}

/// The SSE item stream `chat_stream_core` returns. Boxed because the function has several early
/// returns (chat disabled, create failed, the live turn) whose `impl Stream` types would otherwise
/// differ â€” one named type lets them share a return.
type SseBody = Pin<Box<dyn Stream<Item = Result<Event, Infallible>> + Send>>;

async fn chat_stream_core(
    state: Arc<AppState>,
    message: String,
    session: Option<Ulid>,
    incognito: bool,
) -> Sse<SseBody> {
    let (tx, rx) = mpsc::channel::<AgentEvent>(64);

    let Some(sessions) = state.chat.clone() else {
        // No chat configured: a single `failed` event, and no `session` head (there's no session).
        tokio::spawn(async move {
            let _ = tx
                .send(AgentEvent::Error(
                    "chat is disabled â€” set DEEPSEEK_API_KEY".into(),
                ))
                .await;
        });
        return Sse::new(stream_with_session(None, rx));
    };

    // Resolve the session up front (creating one on the first message), so we can announce it to the
    // client *before* the agent events. A creation failure becomes a single `failed` event.
    let session = match session {
        Some(id) => id,
        None => match if incognito {
            sessions.create_incognito(None).await
        } else {
            sessions.create(None).await
        } {
            Ok(id) => id,
            Err(e) => {
                tracing::warn!(error = %e, "chat stream could not create a conversation");
                tokio::spawn(async move {
                    let _ = tx.send(AgentEvent::Error(e.to_string())).await;
                });
                return Sse::new(stream_with_session(None, rx));
            }
        },
    };

    // `turn_tx` drives the turn; `tx` is kept to detect the client leaving and to send the terminal
    // event. Cloning is cheap and keeps the two uses from tangling borrows below.
    let turn_tx = tx.clone();
    tokio::spawn(async move {
        // Race the turn against the client disconnecting (`tx.closed()` resolves when the SSE
        // receiver is dropped â€” the browser called `EventSource.close()` or the connection dropped).
        // On disconnect we drop the turn future, which cancels the in-flight model/tool work *and*
        // (inside `ChatSessions`/`Conversation`) rolls the partial turn back and persists nothing, so
        // a stopped turn leaves the store clean and the per-session lock is released.
        tokio::select! {
            // Tag the face turn's inference with the chat session id so its latency records join the
            // dispatch work it triggers (the dispatch pack keys the same space via correlation_id).
            result = liberado_provider::latency::with_correlation(
                session.to_string(),
                sessions.turn_stream(session, &message, &turn_tx),
            ) => {
                let terminal = match result {
                    Ok(()) => AgentEvent::Done,
                    Err(e) => {
                        tracing::warn!(error = %e, "chat stream turn failed");
                        AgentEvent::Error(e.to_string())
                    }
                };
                let _ = tx.send(terminal).await;
            }
            _ = tx.closed() => {
                tracing::info!("chat stream cancelled by client; persisted nothing");
            }
        }
    });

    Sse::new(stream_with_session(Some(session), rx))
}

/// Prepend a `session` SSE event (the conversation id) ahead of the agent event stream, so the
/// client records the id and sends it back as `?session=â€¦` on the next turn. `None` means no session
/// was resolved (chat disabled or creation failed) â€” only the body's `failed` event is emitted.
fn stream_with_session(session: Option<Ulid>, rx: mpsc::Receiver<AgentEvent>) -> SseBody {
    let head = futures::stream::once(async move {
        match session {
            Some(id) => Ok(Event::default().event("session").data(id.to_string())),
            // No session to announce; emit a benign comment so the head stream has a consistent type.
            None => Ok(Event::default().comment("no session")),
        }
    });
    let body = ReceiverStream::new(rx).map(|event| Ok(to_sse(event)));
    Box::pin(head.chain(body))
}

/// Map the executor's in-process [`AgentEvent`] tap onto the **converged** session-event wire
/// vocabulary (2026-07-11) â€” the same SSE names/shapes `session_event_to_sse` emits for goal
/// sessions, so every surface renders chat turns and goal sessions with one decoder
/// (`chat_client_contract::SessionEvent::from_sse_data`). This boundary mapping is the chat
/// counterpart of the coding pack's `CoderEvent` â†’ `SessionEvent` translation.
fn to_sse(event: AgentEvent) -> Event {
    match event {
        AgentEvent::Token(text) => Event::default().event("token").data(text),
        // Structured events carry JSON so the payload stays a single SSE line (newlines in a
        // preview would otherwise split into multiple `data:` lines).
        AgentEvent::ToolStarted { name, args } => Event::default()
            .event("tool_started")
            .data(serde_json::json!({ "name": name, "args_preview": args }).to_string()),
        AgentEvent::ToolFinished { name, ok, preview } => {
            Event::default().event("tool_finished").data(
                serde_json::json!({ "name": name, "ok": ok, "result_preview": preview })
                    .to_string(),
            )
        }
        AgentEvent::Done => Event::default()
            .event("session_finished")
            .data(serde_json::json!({ "status": "done", "summary": "" }).to_string()),
        // Named `failed`, not `error`: browser `EventSource` reserves the `error` event for its own
        // connection errors, so a custom `error` event can't be listened for cleanly.
        AgentEvent::Error(msg) => Event::default()
            .event("failed")
            .data(serde_json::json!({ "message": msg }).to_string()),
    }
}

/// One conversational turn: resolve the session (creating one when the request carries none), run
/// the agent (model + tools, multi-turn context rehydrated from the store), persist on success, and
/// return the reply plus the session id so the client can continue the conversation.
pub async fn chat(
    State(state): State<Arc<AppState>>,
    Json(req): Json<ChatRequest>,
) -> impl IntoResponse {
    let Some(sessions) = &state.chat else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(ApiError {
                error: "chat is disabled â€” set DEEPSEEK_API_KEY".into(),
            }),
        )
            .into_response();
    };

    let session = match req.session {
        Some(id) => id,
        None => match sessions.create(None).await {
            Ok(id) => id,
            Err(e) => return chat_error(e),
        },
    };

    match sessions.turn(session, &req.message).await {
        Ok(reply) => Json(chat_client_contract::ChatResponse {
            reply,
            session: session.to_string(),
        })
        .into_response(),
        Err(e) => {
            tracing::warn!(error = %e, "chat turn failed");
            chat_error(e)
        }
    }
}

/// Map a session/store error to a 500 JSON body â€” the shared failure shape for the chat endpoints.
fn chat_error(e: liberado_main_agent::SessionError) -> axum::response::Response {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(ApiError {
            error: e.to_string(),
        }),
    )
        .into_response()
}

/// `GET /api/conversations` â€” the sidebar listing: every conversation header, newest first. A thin
/// passthrough to the store.
pub async fn list_conversations(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let Some(sessions) = &state.chat else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(ApiError {
                error: "chat is disabled â€” set DEEPSEEK_API_KEY".into(),
            }),
        )
            .into_response();
    };
    match sessions.list().await {
        Ok(headers) => Json(headers).into_response(),
        Err(e) => chat_error(e),
    }
}

/// Query parameters for [`delete_conversation`].
#[derive(Deserialize, Default)]
pub struct DeleteParams {
    /// Refuse the delete unless the target is an incognito session. See the handler's docs — this
    /// is a guard against automatic teardown destroying a saved conversation, not a convenience.
    #[serde(default)]
    pub ephemeral_only: bool,
}

/// `DELETE /api/conversations/{id}` — permanently delete a conversation.
///
/// Really deletes: the store removes the log from disk (see `ConversationStore::delete`), so this is
/// not a hide. There is no archive tier to fall back to, and offering one that only hid the
/// conversation would be worse than not offering it.
///
/// `404` when it is already gone, rather than a silent `204`. A client that just wants the row out
/// of its list can treat both the same; a client with a real bug gets told.
///
/// # `?ephemeral_only=true`
///
/// Refuses with `409` unless the session is incognito. **This exists because the unguarded version
/// destroyed a real conversation.** The WebUI's incognito teardown fires on its own, from an effect,
/// with no human in the loop — so a client-side mix-up about *which* session is the private one
/// silently deletes a saved chat with no undo and no backup. Two client bugs did exactly that.
///
/// The client fixes are in, but "the caller will pass the right id" is not a property worth resting
/// permanent data loss on. Every automatic teardown passes this flag, so the worst a future bug of
/// the same shape can do is fail to clean up — which the idle sweeper then handles. The sidebar's
/// Delete button deliberately does *not* pass it: a human clicked it and confirmed.
pub async fn delete_conversation(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Ulid>,
    Query(params): Query<DeleteParams>,
) -> impl IntoResponse {
    let Some(sessions) = &state.chat else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(ApiError {
                error: "chat is disabled".into(),
            }),
        )
            .into_response();
    };

    if params.ephemeral_only
        && let Some(header) = state.sessions.session(id).await
        && !header.ephemeral
    {
        tracing::warn!(
            conversation = %id,
            "refused an ephemeral-only delete of a durable conversation — a caller thought a saved \
             chat was its incognito session"
        );
        return (
            StatusCode::CONFLICT,
            Json(ApiError {
                error: "refusing to delete: this conversation is not incognito".into(),
            }),
        )
            .into_response();
    }

    match sessions.delete(id).await {
        Ok(()) => {
            tracing::info!(conversation = %id, "conversation deleted");
            StatusCode::NO_CONTENT.into_response()
        }
        Err(liberado_main_agent::SessionError::Store(
            liberado_conversation_store::StoreError::NotFound(_),
        )) => (
            StatusCode::NOT_FOUND,
            Json(ApiError {
                error: "conversation not found".into(),
            }),
        )
            .into_response(),
        Err(e) => chat_error(e),
    }
}

/// `PATCH /api/conversations/{id}` â€” update the title of an existing conversation.
#[derive(Deserialize)]
pub struct TitleRequest {
    pub title: String,
}

pub async fn patch_conversation_title(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Ulid>,
    Json(req): Json<TitleRequest>,
) -> impl IntoResponse {
    let Some(sessions) = &state.chat else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(ApiError {
                error: "chat is disabled â€” set DEEPSEEK_API_KEY".into(),
            }),
        )
            .into_response();
    };
    match sessions.set_title(id, req.title).await {
        Ok(()) => StatusCode::OK.into_response(),
        Err(liberado_main_agent::SessionError::Store(
            liberado_conversation_store::StoreError::NotFound(_),
        )) => (
            StatusCode::NOT_FOUND,
            Json(ApiError {
                error: "conversation not found".into(),
            }),
        )
            .into_response(),
        Err(e) => {
            tracing::warn!(error = %e, "PATCH title failed for conversation {id}");
            chat_error(e)
        }
    }
}

/// `GET /api/conversations/{id}` â€” the full message history of one conversation, for reopening it.
/// 404 when the conversation does not exist, 500 on any other store error.
pub async fn get_conversation(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Ulid>,
) -> impl IntoResponse {
    let Some(sessions) = &state.chat else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(ApiError {
                error: "chat is disabled â€” set DEEPSEEK_API_KEY".into(),
            }),
        )
            .into_response();
    };
    match sessions.history(id).await {
        Ok(messages) => {
            let messages: Vec<ChatMessage> = messages
                .into_iter()
                .map(chat_message_from_provider)
                .collect();
            Json(ConversationHistoryResponse { messages }).into_response()
        }
        Err(liberado_main_agent::SessionError::Store(
            liberado_conversation_store::StoreError::NotFound(_),
        )) => (
            StatusCode::NOT_FOUND,
            Json(ApiError {
                error: "conversation not found".into(),
            }),
        )
            .into_response(),
        Err(e) => chat_error(e),
    }
}

/// Converts one stored `liberado_provider::Message` (the internal, richer type
/// `ChatSessions::history` returns) into the wire `ChatMessage` â€” the single conversion point that
/// keeps `GET /api/conversations/{id}` honoring `chat-client-contract` instead of leaking an
/// internal type through a hand-rolled `serde_json::json!` literal.
fn chat_message_from_provider(m: liberado_provider::Message) -> ChatMessage {
    let role = match m.role {
        liberado_provider::Role::System => "system",
        liberado_provider::Role::User => "user",
        liberado_provider::Role::Assistant => "assistant",
        liberado_provider::Role::Tool => "tool",
    };
    ChatMessage {
        role: role.to_string(),
        content: m.content,
        tool_calls: (!m.tool_calls.is_empty())
            .then(|| serde_json::to_value(&m.tool_calls).ok())
            .flatten(),
        tool_call_id: m.tool_call_id,
    }
}

#[cfg(test)]
mod tests {
    use super::ChatRequest;

    /// The WebUI reaches the chat stream through `EventSource`, which can only issue a `GET` — so
    /// `incognito` crosses the wire as a query parameter, and axum deserializes it with
    /// `serde_urlencoded`. That deserializer parses a `bool` through `FromStr`, which accepts
    /// **only** `true`/`false`.
    ///
    /// This is pinned because the failure is disproportionate to the mistake: `incognito=1` does not
    /// fall back to `false`, it fails the whole `Query` extraction, so the request 400s and the chat
    /// just does not answer. Nothing in the type signature hints at that.
    #[test]
    fn incognito_parses_from_the_query_string_as_true_not_one() {
        let ok: ChatRequest = serde_urlencoded::from_str("message=hi&incognito=true").unwrap();
        assert!(ok.incognito);

        let off: ChatRequest = serde_urlencoded::from_str("message=hi&incognito=false").unwrap();
        assert!(!off.incognito);

        // Absent is the overwhelmingly common case: every normal chat, and every other client.
        let absent: ChatRequest = serde_urlencoded::from_str("message=hi").unwrap();
        assert!(!absent.incognito);

        assert!(
            serde_urlencoded::from_str::<ChatRequest>("message=hi&incognito=1").is_err(),
            "if `1` ever starts parsing, the comment on the URL builder in webui/chat.rs is stale"
        );
    }

    // ── The `?ephemeral_only=true` guard ─────────────────────────────────────────────────────
    //
    // Driven through the real router and the real store, because what is being asserted is that a
    // *request* cannot destroy data — and the parts that would let it (route wiring, extractor
    // order, the store's own notion of ephemerality) are precisely the parts a narrower test would
    // stub out.

    use std::sync::Arc;

    use axum::Router;
    use axum::body::Body;
    use axum::http::Request;
    use liberado_executor::{Budget, Executor};
    use liberado_main_agent::ChatSessions;
    use liberado_provider::MockProvider;
    use liberado_session_store::SessionStore;
    use liberado_test_support::NoopRuntime;
    use tower::ServiceExt;

    use super::*;

    struct Harness {
        app: Router,
        chat: Arc<ChatSessions>,
        _dir: tempfile::TempDir,
    }

    async fn harness() -> Harness {
        let dir = tempfile::tempdir().unwrap();
        let sessions = Arc::new(SessionStore::open(dir.path()).await);
        let executor = Executor::new(
            Arc::new(MockProvider::with_script("mock", vec![])),
            Budget::default(),
        );
        let chat = Arc::new(ChatSessions::new(
            sessions.clone(),
            executor,
            Arc::new(NoopRuntime),
        ));
        let state = Arc::new(crate::state::AppState::for_test(
            sessions.clone(),
            Some(chat.clone()),
            dir.path().to_path_buf(),
        ));
        let app = Router::new()
            .route(
                "/api/conversations/{id}",
                axum::routing::delete(super::delete_conversation),
            )
            .with_state(state);
        Harness {
            app,
            chat,
            _dir: dir,
        }
    }

    async fn delete(app: &Router, uri: &str) -> StatusCode {
        app.clone()
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri(uri)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap()
            .status()
    }

    /// The guard that turns "a client sent the wrong id" from permanent data loss into a no-op.
    ///
    /// This is not hypothetical. The WebUI's incognito teardown once mistook a saved conversation
    /// for its private session and deleted it — no confirmation, no undo, no backup. Every automatic
    /// teardown now passes this flag, so the same class of bug can only fail to clean up.
    #[tokio::test]
    async fn ephemeral_only_delete_refuses_a_durable_conversation() {
        let h = harness().await;
        let durable = h.chat.create(None).await.unwrap();

        let status = delete(
            &h.app,
            &format!("/api/conversations/{durable}?ephemeral_only=true"),
        )
        .await;

        assert_eq!(status, StatusCode::CONFLICT);
        assert!(
            h.chat.history(durable).await.is_ok(),
            "the conversation must still be there — a refused delete that deleted anyway is the \
             whole bug this guards"
        );
    }

    #[tokio::test]
    async fn ephemeral_only_delete_removes_an_incognito_session() {
        let h = harness().await;
        let ghost = h.chat.create_incognito(None).await.unwrap();

        let status = delete(
            &h.app,
            &format!("/api/conversations/{ghost}?ephemeral_only=true"),
        )
        .await;

        assert_eq!(status, StatusCode::NO_CONTENT);
        assert!(h.chat.history(ghost).await.is_err());
    }

    /// Without the flag the endpoint is unchanged — that is the path the sidebar's Delete button
    /// takes, where a human clicked and confirmed.
    #[tokio::test]
    async fn an_unguarded_delete_still_removes_a_durable_conversation() {
        let h = harness().await;
        let durable = h.chat.create(None).await.unwrap();

        let status = delete(&h.app, &format!("/api/conversations/{durable}")).await;

        assert_eq!(status, StatusCode::NO_CONTENT);
        assert!(h.chat.history(durable).await.is_err());
    }
}
