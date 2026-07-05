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

use chat_client_contract::{
    ApiError, CatalogResponse, ChatMessage, ConversationHistoryResponse,
    ConversationSearchResponse, ConversationSearchResult, DaemonStatus, McpInfo,
    SearchMessageMatch, VaultInfo,
};
use liberado_chat_search::ParsedQuery;

use crate::state::AppState;

#[derive(Deserialize)]
pub struct ChatRequest {
    pub message: String,
    /// The conversation to continue. Absent on the first message of a chat (a new conversation is
    /// created and its id returned to the client). `Ulid` deserializes from its string form, so this
    /// works for both the JSON body and the `?session=…` query.
    #[serde(default)]
    pub session: Option<Ulid>,
}

/// Streaming chat — the shared client contract (see `docs/reference/api.md`). Returns
/// `text/event-stream`; events: `token` (answer delta), `tool` (a call starting, `{name,args}`),
/// `tool_result` (its outcome, `{name,ok,preview}`), `done`, `failed`. Available as both `POST`
/// (JSON body — native clients) and `GET` (`?message=…` — browser
/// `EventSource`); both funnel through [`chat_stream_core`].
pub async fn chat_stream_post(
    State(state): State<Arc<AppState>>,
    Json(req): Json<ChatRequest>,
) -> Sse<SseBody> {
    chat_stream_core(state, req.message, req.session).await
}

/// `GET /api/chat/stream?message=…` — the `EventSource`-friendly variant (browsers can't `POST` an
/// `EventSource`). Same SSE contract as the POST form, including `?session=…` to continue a chat.
pub async fn chat_stream_get(
    State(state): State<Arc<AppState>>,
    Query(req): Query<ChatRequest>,
) -> Sse<SseBody> {
    chat_stream_core(state, req.message, req.session).await
}

/// The SSE item stream `chat_stream_core` returns. Boxed because the function has several early
/// returns (chat disabled, create failed, the live turn) whose `impl Stream` types would otherwise
/// differ — one named type lets them share a return.
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
                    "chat is disabled — set DEEPSEEK_API_KEY".into(),
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
        // receiver is dropped — the browser called `EventSource.close()` or the connection dropped).
        // On disconnect we drop the turn future, which cancels the in-flight model/tool work *and*
        // (inside `ChatSessions`/`Conversation`) rolls the partial turn back and persists nothing, so
        // a stopped turn leaves the store clean and the per-session lock is released.
        tokio::select! {
            result = sessions.turn_stream(session, &message, &turn_tx) => {
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
/// client records the id and sends it back as `?session=…` on the next turn. `None` means no session
/// was resolved (chat disabled or creation failed) — only the body's `failed` event is emitted.
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

fn to_sse(event: AgentEvent) -> Event {
    match event {
        AgentEvent::Token(text) => Event::default().event("token").data(text),
        // Tool events carry structured fields, JSON-encoded so the payload stays a single SSE line
        // (newlines in a preview would otherwise split into multiple `data:` lines).
        AgentEvent::ToolStarted { name, args } => Event::default()
            .event("tool")
            .data(serde_json::json!({ "name": name, "args": args }).to_string()),
        AgentEvent::ToolFinished { name, ok, preview } => Event::default()
            .event("tool_result")
            .data(serde_json::json!({ "name": name, "ok": ok, "preview": preview }).to_string()),
        AgentEvent::Done => Event::default().event("done").data(""),
        // Named `failed`, not `error`: browser `EventSource` reserves the `error` event for its own
        // connection errors, so a custom `error` event can't be listened for cleanly.
        AgentEvent::Error(msg) => Event::default().event("failed").data(msg),
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
                error: "chat is disabled — set DEEPSEEK_API_KEY".into(),
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

/// Map a session/store error to a 500 JSON body — the shared failure shape for the chat endpoints.
fn chat_error(e: liberado_main_agent::SessionError) -> axum::response::Response {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(ApiError {
            error: e.to_string(),
        }),
    )
        .into_response()
}

/// `GET /api/conversations` — the sidebar listing: every conversation header, newest first. A thin
/// passthrough to the store.
pub async fn list_conversations(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let Some(sessions) = &state.chat else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(ApiError {
                error: "chat is disabled — set DEEPSEEK_API_KEY".into(),
            }),
        )
            .into_response();
    };
    match sessions.list().await {
        Ok(headers) => Json(headers).into_response(),
        Err(e) => chat_error(e),
    }
}

/// `PATCH /api/conversations/{id}` — update the title of an existing conversation.
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
                error: "chat is disabled — set DEEPSEEK_API_KEY".into(),
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

/// `GET /api/conversations/{id}` — the full message history of one conversation, for reopening it.
/// 404 when the conversation does not exist, 500 on any other store error.
pub async fn get_conversation(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Ulid>,
) -> impl IntoResponse {
    let Some(sessions) = &state.chat else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(ApiError {
                error: "chat is disabled — set DEEPSEEK_API_KEY".into(),
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
/// `ChatSessions::history` returns) into the wire `ChatMessage` — the single conversion point that
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

#[derive(Deserialize)]
pub struct ReactionsQuery {
    limit: Option<usize>,
}

pub async fn status(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let reactions_len = state.reactions.lock().await.len();

    Json(DaemonStatus {
        running: true,
        vault_path: state.vault_path.clone(),
        uptime_seconds: state.start_time.elapsed().as_secs(),
        watcher_active: true,
        dispatcher_attached: state.dispatcher_attached,
        orchestrator_attached: state.orchestrator_attached,
        reactions_seen: reactions_len as u64,
        model_name: state.model_name.clone(),
        token_usage_total: None,
        context_window: None,
        chat_tools: state.chat_tools,
        chat_tool_names: state.chat_tool_names.clone(),
    })
}

pub async fn catalog(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let descriptors = state.catalog.descriptors();

    // `chat_tool_names` is the connected runtime's real, flat `<mcp>:<tool>`-prefixed catalog
    // (built once at boot in `build_chat`) — group it by server name so each row below gets its
    // actual tool breakdown instead of the tool_count:0/tool_names:[] stub this used to return.
    let mut tools_by_mcp: std::collections::HashMap<&str, Vec<String>> =
        std::collections::HashMap::new();
    for tool_name in &state.chat_tool_names {
        let mcp = liberado_common::mcp_of(tool_name);
        let bare = tool_name
            .strip_prefix(&format!("{mcp}:"))
            .unwrap_or(tool_name);
        tools_by_mcp.entry(mcp).or_default().push(bare.to_string());
    }

    let mcps = descriptors
        .iter()
        .map(|d| {
            // Convert the Consequence enum to its snake_case string representation.
            // We avoid depending on liberado-common in the contract crate, so we serialize
            // through serde_json here on the server side.
            let consequence = serde_json::to_value(&d.consequence)
                .ok()
                .and_then(|v| v.as_str().map(String::from))
                .unwrap_or_default();
            let tool_names = tools_by_mcp
                .get(d.name.as_str())
                .cloned()
                .unwrap_or_default();

            McpInfo {
                name: d.name.clone(),
                description: d.description.clone(),
                consequence,
                tool_count: tool_names.len(),
                tool_names,
                provenance: d.provenance.clone(),
                visible_to_main_agent: state.main_agent_capabilities.grants_mcp(&d.name),
                visible_to_dispatcher: state.dispatcher_capabilities.grants_mcp(&d.name),
            }
        })
        .collect();

    Json(CatalogResponse { mcps })
}

#[derive(Deserialize)]
pub struct SearchQuery {
    pub q: String,
    #[serde(default)]
    pub regex: bool,
    #[serde(default = "default_search_limit")]
    pub limit: usize,
}

fn default_search_limit() -> usize {
    20
}

/// `GET /api/conversations/search?q=...&regex=false&limit=20`
///
/// Searches conversation history for messages matching `q`. In literal mode (default), `q` is
/// split on whitespace; `"quoted phrases"` are treated as single terms; ALL terms must appear in
/// **the same message** (case-insensitive AND) — narrows toward a topic from a few
/// half-remembered keywords rather than flooding results with an OR. This is per-message, not
/// per-conversation: a query like `"auth token"` will not match a conversation where "auth" and
/// "token" appear in two different messages, only one where a single message contains both. In
/// regex mode, `q` is a single Rust regex pattern applied case-insensitively (also per-message).
///
/// Returns at most `limit` matching conversations (newest first), each with every matching
/// message's snippet. 400 on an empty query or invalid regex; 500 on I/O error.
pub async fn search_conversations(
    State(state): State<Arc<AppState>>,
    Query(query): Query<SearchQuery>,
) -> impl IntoResponse {
    let parsed = if query.regex {
        ParsedQuery::parse_regex(&query.q)
    } else {
        ParsedQuery::parse_literal(&query.q)
    };
    let parsed = match parsed {
        Ok(p) => p,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(ApiError {
                    error: e.to_string(),
                }),
            )
                .into_response();
        }
    };

    let limit = query.limit.clamp(1, 200);
    match liberado_chat_search::search(&state.conversations_root, &parsed, limit).await {
        Ok(sr) => {
            let total_found = sr.total_found;
            let results = sr
                .matches
                .into_iter()
                .map(|m| ConversationSearchResult {
                    conversation_id: m.conversation_id,
                    title: m.title,
                    created_at: m.created_at,
                    matches: m
                        .matches
                        .into_iter()
                        .map(|mm| SearchMessageMatch {
                            node_id: mm.node_id,
                            author: mm.author,
                            content_snippet: mm.content_snippet,
                            created_at: mm.created_at,
                        })
                        .collect(),
                })
                .collect();
            Json(ConversationSearchResponse {
                results,
                total_found,
            })
            .into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiError {
                error: e.to_string(),
            }),
        )
            .into_response(),
    }
}

pub async fn reactions(
    State(state): State<Arc<AppState>>,
    Query(query): Query<ReactionsQuery>,
) -> impl IntoResponse {
    let limit = query.limit.unwrap_or(20);
    let guard = state.reactions.lock().await;
    let events: Vec<_> = guard.iter().rev().take(limit).cloned().collect();
    Json(events)
}

pub async fn vault(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    Json(VaultInfo {
        root: state.vault_path.clone(),
        note_count: 0,
        watcher_active: true,
    })
}
