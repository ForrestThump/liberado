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

/// Active model id: prefer live provider state (hot-swappable) over boot-time snapshot.
fn active_model(state: &AppState) -> Option<String> {
    state
        .provider
        .as_ref()
        .map(|p| p.model())
        .or_else(|| state.model_name.clone())
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
        model_name: active_model(&state),
        token_usage_total: None,
        context_window: None,
        chat_tools: state.chat_tools,
        chat_tool_names: state.chat_tool_names.clone(),
    })
}

/// `GET /api/models` — live model catalog from the provider (`GET /models` upstream) plus the
/// currently configured model. Soft-fails: always 200 with `error` set when the provider list
/// cannot be fetched so the TUI can still show `current`.
pub async fn models(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    use chat_client_contract::ModelsResponse;

    let current = active_model(&state);
    let Some(provider) = state.provider.as_ref() else {
        return Json(ModelsResponse {
            models: Vec::new(),
            current,
            error: Some("no inference provider configured".into()),
        });
    };

    match provider.list_models().await {
        Ok(mut models) => {
            // Ensure the active model appears even if the catalog omitted it.
            if let Some(cur) = current.as_ref()
                && !models.iter().any(|m| m == cur)
            {
                models.insert(0, cur.clone());
            }
            models.sort();
            models.dedup();
            Json(ModelsResponse {
                models,
                current,
                error: None,
            })
        }
        Err(e) => {
            tracing::warn!(error = %e, "GET /api/models: provider list_models failed");
            let mut models = Vec::new();
            if let Some(cur) = current.as_ref() {
                models.push(cur.clone());
            }
            Json(ModelsResponse {
                models,
                current,
                error: Some(e.to_string()),
            })
        }
    }
}

/// `POST /api/models/select` — hot-swap the active model for subsequent completions without
/// restarting the daemon. Body: `{"model":"…"}`. Same base URL / credentials; only the model
/// field of the next chat-completions request changes.
pub async fn select_model(
    State(state): State<Arc<AppState>>,
    Json(body): Json<SelectModelRequest>,
) -> impl IntoResponse {
    use chat_client_contract::ModelsResponse;

    let model = body.model.trim().to_string();
    if model.is_empty() {
        return (
            axum::http::StatusCode::BAD_REQUEST,
            Json(ModelsResponse {
                models: Vec::new(),
                current: active_model(&state),
                error: Some("model must be a non-empty string".into()),
            }),
        );
    }

    let Some(provider) = state.provider.as_ref() else {
        return (
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            Json(ModelsResponse {
                models: Vec::new(),
                current: None,
                error: Some("no inference provider configured".into()),
            }),
        );
    };

    let previous = provider.model();
    provider.set_model(model.clone());
    tracing::info!(%previous, current = %model, "hot-swapped active model");

    (
        axum::http::StatusCode::OK,
        Json(ModelsResponse {
            models: Vec::new(),
            current: Some(provider.model()),
            error: None,
        }),
    )
}

#[derive(Deserialize)]
pub struct SelectModelRequest {
    model: String,
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
            let consequence = serde_json::to_value(d.consequence)
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

// ── Goal sessions (scratchpad F) — surfaces are clients; packs own the loop ──

/// `GET /api/goals/domains` — which domain packs are registered (coding, life, …).
pub async fn goals_domains(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    Json(serde_json::json!({
        "domains": state.goals.registered_domains(),
    }))
}

/// `GET /api/goals` — list goal sessions, newest first.
pub async fn goals_list(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    Json(state.goals.list().await)
}

/// `POST /api/goals` — start a goal session. Body: [`liberado_session::GoalSpec`].
pub async fn goals_start(
    State(state): State<Arc<AppState>>,
    Json(goal): Json<liberado_session::GoalSpec>,
) -> impl IntoResponse {
    match state.goals.start(goal).await {
        Ok(id) => (
            StatusCode::ACCEPTED,
            Json(serde_json::json!({ "session_id": id, "status": "running" })),
        )
            .into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, Json(ApiError { error: e })).into_response(),
    }
}

/// `GET /api/goals/{id}` — session record + event history so far.
pub async fn goals_get(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    match state.goals.snapshot(&id).await {
        Some(snap) => Json(snap).into_response(),
        None => (
            StatusCode::NOT_FOUND,
            Json(ApiError {
                error: format!("goal session '{id}' not found"),
            }),
        )
            .into_response(),
    }
}

/// `POST /api/goals/{id}/cancel` — cooperative cancel of a running goal session.
pub async fn goals_cancel(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    match state.goals.cancel(&id).await {
        Ok(()) => StatusCode::ACCEPTED.into_response(),
        Err(e) => (StatusCode::NOT_FOUND, Json(ApiError { error: e })).into_response(),
    }
}

/// `GET /api/goals/{id}/stream` — SSE: catch-up history then live events.
/// Events use `event:` names matching [`liberado_session::SessionEventKind`] type tags
/// (`session_started`, `tool_started`, `session_finished`, …); `data` is full JSON.
pub async fn goals_stream(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let Some((history, mut rx)) = state.goals.store().subscribe(&id).await else {
        return (
            StatusCode::NOT_FOUND,
            Json(ApiError {
                error: format!("goal session '{id}' not found"),
            }),
        )
            .into_response();
    };

    let stream = async_stream::stream! {
        for ev in history {
            yield Ok::<Event, Infallible>(session_event_to_sse(&ev));
        }
        loop {
            match rx.recv().await {
                Ok(ev) => {
                    let terminal = matches!(
                        ev.kind,
                        liberado_session::SessionEventKind::SessionFinished { .. }
                    );
                    yield Ok(session_event_to_sse(&ev));
                    if terminal {
                        break;
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    };

    Sse::new(Box::pin(stream) as SseBody).into_response()
}

fn session_event_to_sse(ev: &liberado_session::SessionEvent) -> Event {
    let name = match &ev.kind {
        liberado_session::SessionEventKind::SessionStarted { .. } => "session_started",
        liberado_session::SessionEventKind::RoleStarted { .. } => "role_started",
        liberado_session::SessionEventKind::RoleFinished { .. } => "role_finished",
        liberado_session::SessionEventKind::ToolStarted { .. } => "tool_started",
        liberado_session::SessionEventKind::ToolFinished { .. } => "tool_finished",
        liberado_session::SessionEventKind::Progress { .. } => "progress",
        liberado_session::SessionEventKind::ValidationFinished { .. } => "validation_finished",
        liberado_session::SessionEventKind::LoopGuard { .. } => "loop_guard",
        liberado_session::SessionEventKind::SessionFinished { .. } => "session_finished",
        liberado_session::SessionEventKind::Error { .. } => "goal_error",
    };
    let data = serde_json::to_string(ev).unwrap_or_else(|_| "{}".into());
    Event::default().event(name).data(data)
}
