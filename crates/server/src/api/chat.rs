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
    chat_stream_core(state, req.message, req.session).await
}

/// `GET /api/chat/stream?message=â€¦` â€” the `EventSource`-friendly variant (browsers can't `POST` an
/// `EventSource`). Same SSE contract as the POST form, including `?session=â€¦` to continue a chat.
pub async fn chat_stream_get(
    State(state): State<Arc<AppState>>,
    Query(req): Query<ChatRequest>,
) -> Sse<SseBody> {
    chat_stream_core(state, req.message, req.session).await
}

/// The SSE item stream `chat_stream_core` returns. Boxed because the function has several early
/// returns (chat disabled, create failed, the live turn) whose `impl Stream` types would otherwise
/// differ â€” one named type lets them share a return.
type SseBody = Pin<Box<dyn Stream<Item = Result<Event, Infallible>> + Send>>;

async fn chat_stream_core(
    state: Arc<AppState>,
    message: String,
    session: Option<Ulid>,
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
        None => match sessions.create(None).await {
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

/// `DELETE /api/conversations/{id}` — permanently delete a conversation.
///
/// Really deletes: the store removes the log from disk (see `ConversationStore::delete`), so this is
/// not a hide. There is no archive tier to fall back to, and offering one that only hid the
/// conversation would be worse than not offering it.
///
/// `404` when it is already gone, rather than a silent `204`. A client that just wants the row out
/// of its list can treat both the same; a client with a real bug gets told.
pub async fn delete_conversation(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Ulid>,
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
