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
/// `text/event-stream`; events use the converged session-event vocabulary: `token` (answer
/// delta), `tool_started` (`{name,args_preview}`), `tool_finished` (`{name,ok,result_preview}`),
/// `session_finished` (`{status,summary}`), `failed` (`{message}`). Available as both `POST`
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

/// Map the executor's in-process [`AgentEvent`] tap onto the **converged** session-event wire
/// vocabulary (2026-07-11) — the same SSE names/shapes `session_event_to_sse` emits for goal
/// sessions, so every surface renders chat turns and goal sessions with one decoder
/// (`chat_client_contract::SessionEvent::from_sse_data`). This boundary mapping is the chat
/// counterpart of the coding pack's `CoderEvent` → `SessionEvent` translation.
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
    match liberado_chat_search::search(&state.sessions_root, &parsed, limit).await {
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

/// `GET /api/sessions` — **every** session, newest first: chats and goal sessions in one list (S5′).
///
/// This is the endpoint the unified switcher always wanted. Before convergence a surface had to poll
/// `/api/conversations` *and* `/api/goals`, invent a row type for each, and stitch them together —
/// which meant the client re-derived a distinction the model says does not exist. Here the
/// distinction is one field: `goal` is absent on a chat and present on a session that runs to a
/// terminal status.
///
/// The older two endpoints remain: they are the *lenses* (`/api/conversations` for the chat view,
/// `/api/goals` for the kernel view), and things like the chat sidebar legitimately want just one.
pub async fn sessions_list(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    Json(state.sessions.list_sessions().await)
}

/// `POST /api/sessions/{id}/fork` — branch a conversation, keeping the original.
///
/// Two things a human wants and could not do: *fork this and keep the original*, and *go back to
/// turn N and take a different path*. Both are the same operation over the message DAG — copy the
/// prefix up to a node — which is why forking was additive rather than a migration: the store has
/// carried `parent_id` and `leaf_path(conv, Some(node))` from day one, and nothing ever asked it to
/// reconstruct a prefix.
///
/// The fork is a **copy**, so it is a snapshot: continue the original afterwards and the fork does
/// not move (see `SessionStore::fork_session` for why copy and not reference).
///
/// The client names the branch point by **turn**, because that is the thing it can show a human;
/// resolving turn → node is this function's whole job.
pub async fn session_fork(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(req): Json<chat_client_contract::ForkRequest>,
) -> impl IntoResponse {
    let Ok(source) = id.parse::<Ulid>() else {
        return bad_request("session id is not a ULID");
    };

    use liberado_conversation_store::ConversationStore;
    let path = match state.sessions.leaf_path(source, None).await {
        Ok(p) => p,
        Err(liberado_conversation_store::StoreError::NotFound(_)) => {
            return (
                StatusCode::NOT_FOUND,
                Json(ApiError {
                    error: "session not found".into(),
                }),
            )
                .into_response();
        }
        Err(e) => return bad_request(&e.to_string()),
    };

    // Your turns are the anchors — the assistant's replies and the tool traffic between them hang
    // off whichever one they answered.
    let user_turns: Vec<usize> = path
        .iter()
        .enumerate()
        .filter(|(_, n)| n.author == liberado_conversation_store::Author::User)
        .map(|(i, _)| i)
        .collect();
    let total_turns = user_turns.len() as u32;

    let (at, kept_turns) = match req.after_turn {
        None => (None, total_turns), // the whole conversation, as it stands
        Some(0) => return bad_request("after_turn is 1-based; there is no turn 0"),
        Some(n) if n as usize >= user_turns.len() => {
            // Asking to keep every turn there is *is* forking the whole thing — not an error.
            (None, total_turns)
        }
        Some(n) => {
            // Keep turn `n` and everything that answered it: branch at the node immediately before
            // turn `n+1` began. That is exactly the context you had when you typed turn n+1 —
            // which is the moment the human is trying to go back to.
            let next_turn_start = user_turns[n as usize];
            (Some(path[next_turn_start - 1].id), n)
        }
    };

    match state.sessions.fork_session(source, at, req.title).await {
        Ok(header) => Json(chat_client_contract::ForkResponse {
            id: header.id.to_string(),
            forked_from: source.to_string(),
            kept_turns,
            total_turns,
        })
        .into_response(),
        Err(e) => bad_request(&e.to_string()),
    }
}

fn bad_request(message: &str) -> axum::response::Response {
    (
        StatusCode::BAD_REQUEST,
        Json(ApiError {
            error: message.to_string(),
        }),
    )
        .into_response()
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

/// `POST /api/goals` — start a goal session. Body: [`liberado_session::GoalSpec`]. When the spec
/// carries an `origin` (a chat turn spawned it, e.g. via `/spawn`), a **return-handoff** watcher is
/// spawned so the session's terminal summary folds back into the parent conversation (S4/D2).
///
/// The spec's optional `profile` selects a `[[session_profiles]]` hat (S6): the pack that runs it,
/// the capability grant that bounds it, and the pack's opaque overrides.
pub async fn goals_start(
    State(state): State<Arc<AppState>>,
    Json(mut goal): Json<liberado_session::GoalSpec>,
) -> impl IntoResponse {
    let origin = goal.origin.clone();

    // Resolve the session's "hat" (S6) into a concrete authority before the kernel sees it. A
    // profile picks the pack, the capability grant, and the pack's opaque overrides; no profile
    // falls back to the bare domain, keyed by the domain name (the pool rule). Whether this session
    // may interrupt a human is decided *here*, by the grant — not by the caller asserting it.
    let (domain, capabilities, overrides, profile_idle) = state
        .config
        .resolve_session_profile(goal.profile.as_deref(), goal.domain.as_str());
    if domain.as_str() != goal.domain.as_str() {
        goal.domain = liberado_session::DomainHint::from(domain.as_str());
    }
    // Per-goal idle wins; otherwise the profile default (E5 — hours for interactive coding).
    if goal.max_idle_secs.is_none() {
        goal.max_idle_secs = profile_idle;
    }
    let grant = liberado_session::SessionGrant {
        capabilities,
        profile: goal.profile.clone(),
        overrides: serde_json::to_value(&overrides).unwrap_or(serde_json::Value::Null),
    };

    match state.goals.start_with_grant(goal, grant).await {
        Ok(id) => {
            if let Some(origin) = origin {
                spawn_return_handoff(state.clone(), id.clone(), origin);
            }
            (
                StatusCode::ACCEPTED,
                Json(serde_json::json!({ "session_id": id, "status": "running" })),
            )
                .into_response()
        }
        Err(e) => (StatusCode::BAD_REQUEST, Json(ApiError { error: e })).into_response(),
    }
}

/// After an `origin`-linked session terminates, fold its summary back into the parent conversation
/// (session-focus S4 / D2). Spawned at start, it subscribes to the session's own event stream —
/// `subscribe()` returns catch-up history + a live receiver, so a finish between start and here is
/// not missed — waits for the terminal event, then appends a note via `ChatSessions::append_note`.
/// Best-effort: a missing conversation / disabled chat / append error is logged, never fatal.
fn spawn_return_handoff(
    state: Arc<AppState>,
    session_id: String,
    origin: liberado_session::SessionOrigin,
) {
    use liberado_session::SessionEventKind as K;
    tokio::spawn(async move {
        let Some((history, mut rx)) = state.goals.store().subscribe(&session_id).await else {
            return;
        };
        let already_done = history
            .iter()
            .any(|e| matches!(e.kind, K::SessionFinished { .. }));
        if !already_done {
            loop {
                match rx.recv().await {
                    Ok(ev) if matches!(ev.kind, K::SessionFinished { .. }) => break,
                    Ok(_) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        }

        let Some(chat) = state.chat.as_ref() else {
            return; // no conversation store to fold into (chat disabled — there was no parent anyway)
        };
        // The `SessionFinished` *event* broadcasts just before `store.finish()` records the terminal
        // status/result, so read the snapshot only once the *record* has actually settled terminal —
        // otherwise the note would say "running" with no outcome.
        let mut snap = None;
        for _ in 0..200 {
            match state.goals.snapshot(&session_id).await {
                Some(s) if s.session.status.is_terminal() => {
                    snap = Some(s);
                    break;
                }
                Some(_) => tokio::time::sleep(std::time::Duration::from_millis(5)).await,
                None => return,
            }
        }
        let Some(snap) = snap else {
            tracing::warn!(session = %session_id, "return handoff: record never settled terminal");
            return;
        };
        // No parent conversation ⇒ nothing to hand back to. That is the normal case for an
        // unattended session (a cron carries a correlation id but no parent), not an error.
        let Some(parent) = origin.conversation_id.as_deref() else {
            return;
        };
        let conv = match parent.parse::<Ulid>() {
            Ok(c) => c,
            Err(_) => {
                tracing::warn!(conversation = %parent, "return handoff: parent id is not a ULID");
                return;
            }
        };
        let note = format_handoff_note(&snap.session, &session_id);
        match chat.append_note(conv, note).await {
            Ok(()) => tracing::info!(
                session = %session_id,
                conversation = %parent,
                "return handoff: summary folded into parent conversation"
            ),
            Err(e) => tracing::warn!(error = %e, "return handoff: append_note failed"),
        }
    });
}

/// The note folded back into the parent conversation: session kind/id, terminal status, summary,
/// artifacts, and the `/join` hint so the human can reopen the full transcript. Compact by design —
/// the parent stays lean; the transcript lives in the session (D2).
fn format_handoff_note(record: &liberado_session::GoalSessionRecord, session_id: &str) -> String {
    let domain = record.goal.domain.as_str();
    let status = format!("{:?}", record.status).to_ascii_lowercase();
    let mut note = format!(
        "[{domain} session {status}] {}",
        record.goal.description.trim()
    );
    if let Some(result) = &record.result {
        if !result.summary.trim().is_empty() {
            note.push_str(&format!("\nOutcome: {}", result.summary.trim()));
        }
        if !result.artifacts.is_empty() {
            note.push_str(&format!("\nArtifacts: {}", result.artifacts.join(", ")));
        }
    }
    note.push_str(&format!(
        "\n(Rejoin the full transcript with /join {session_id}.)"
    ));
    note
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

/// Body for [`goals_message`]: the human's reply to an interactive session's `AwaitingInput`.
#[derive(Deserialize)]
pub struct GoalMessageRequest {
    pub text: String,
}

/// `POST /api/goals/{id}/message` — deliver a human message into a running interactive goal
/// session (the reply to an `awaiting_input` prompt). A thin wrapper over
/// [`liberado_session::GoalSessionHub::send_input`], which echoes the message into the transcript
/// as a `human_input` event. `202 Accepted` on delivery; `404` when the session is unknown; `409`
/// when it has already finished (a terminal session accepts no input); `403` when the session's
/// grant omits `AskHuman` and so may *never* receive human input (S6) — an authority answer, not a
/// timing one.
pub async fn goals_message(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(req): Json<GoalMessageRequest>,
) -> impl IntoResponse {
    use liberado_session::SendInputError;
    // A parked session has no live pack to deliver into, so `send_input` cannot take the answer —
    // but for a pack that can rebuild itself from its transcript, answering IS the resume (E6-c).
    // Try the live door first (the overwhelmingly common case), and fall through to resume only for
    // the one error that means "parked". A pack that says it cannot be resumed still returns
    // `Parked`, and the caller still gets an honest 409 saying so.
    let outcome = match state.goals.send_input(&id, req.text.clone()).await {
        Err(SendInputError::Parked) => state.goals.resume(&id, req.text).await,
        other => other,
    };
    match outcome {
        Ok(()) => StatusCode::ACCEPTED.into_response(),
        Err(e @ SendInputError::Unknown) => (
            StatusCode::NOT_FOUND,
            Json(ApiError {
                error: e.to_string(),
            }),
        )
            .into_response(),
        Err(e @ SendInputError::NotPermitted) => (
            StatusCode::FORBIDDEN,
            Json(ApiError {
                error: e.to_string(),
            }),
        )
            .into_response(),
        // `Parked` is a 409 like the others — the answer cannot be delivered — but its *message*
        // must not claim the session finished, because it has not. A client rendering "already
        // finished" over a session that is still holding a question for you is the difference
        // between "start over" and "wait".
        Err(e @ (SendInputError::Terminal | SendInputError::Parked | SendInputError::Closed)) => (
            StatusCode::CONFLICT,
            Json(ApiError {
                error: e.to_string(),
            }),
        )
            .into_response(),
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

/// Encode a kernel [`liberado_session::SessionEvent`] as SSE. Event names are the kind's serde
/// tags — the same converged vocabulary `to_sse` maps chat's `AgentEvent` onto, decoded
/// client-side by `chat_client_contract::SessionEvent::from_sse_data`. `token` frames carry the
/// bare text (they're high-frequency); everything else carries the full event JSON.
fn session_event_to_sse(ev: &liberado_session::SessionEvent) -> Event {
    use liberado_session::SessionEventKind as K;
    if let K::Token { text } = &ev.kind {
        return Event::default().event("token").data(text.clone());
    }
    let name = match &ev.kind {
        K::SessionStarted { .. } => "session_started",
        K::RoleStarted { .. } => "role_started",
        K::RoleFinished { .. } => "role_finished",
        K::Token { .. } => unreachable!("handled above"),
        K::ToolStarted { .. } => "tool_started",
        K::ToolFinished { .. } => "tool_finished",
        K::Progress { .. } => "progress",
        K::AwaitingInput { .. } => "awaiting_input",
        K::HumanInput { .. } => "human_input",
        K::ValidationFinished { .. } => "validation_finished",
        K::LoopGuard { .. } => "loop_guard",
        K::SessionFinished { .. } => "session_finished",
        K::Failed { .. } => "failed",
    };
    let data = serde_json::to_string(ev).unwrap_or_else(|_| "{}".into());
    Event::default().event(name).data(data)
}

#[cfg(test)]
mod goal_message_tests {
    //! HTTP-level integration tests for `POST /api/goals/{id}/message`, against a real `axum::Router`
    //! wired to a `GoalSessionHub` with the life-ops demo pack (the same pattern as `hooks.rs`).

    use super::*;
    use std::time::{Duration, Instant};

    use axum::Router;
    use axum::body::Body;
    use axum::http::Request;
    use liberado_session::{
        DomainHint, GoalSessionHub, GoalSessionStore, GoalSpec, LifeOpsDemoRunner, SessionSnapshot,
    };
    use tower::ServiceExt;

    /// Build a router exposing just the goal-session routes under test, plus a handle to the hub so
    /// a test can poll session state directly (start a session, wait for `awaiting_input`, …).
    fn goals_app() -> (Router, Arc<GoalSessionHub>) {
        let mut hub = GoalSessionHub::new(GoalSessionStore::new());
        hub.register_pack(Arc::new(LifeOpsDemoRunner));
        let goals = Arc::new(hub);

        let (hook_tx, _hook_rx) = tokio::sync::mpsc::unbounded_channel();
        let state = Arc::new(AppState {
            start_time: Instant::now(),
            reactions: Arc::new(tokio::sync::Mutex::new(Vec::new())),
            dispatcher_attached: false,
            orchestrator_attached: false,
            vault_path: "/tmp/vault".to_string(),
            goals: goals.clone(),
            chat: None,
            chat_tools: 0,
            chat_tool_names: Vec::new(),
            catalog: Arc::new(liberado_common::CapabilityCatalog::new()),
            sessions_root: std::path::PathBuf::from("/tmp/liberado/sessions"),
            main_agent_capabilities: liberado_common::CapabilitySet::empty(),
            dispatcher_capabilities: liberado_common::CapabilitySet::empty(),
            config: Arc::new(test_config_with_life_grants()),
            sessions: Arc::new(Default::default()),
            model_name: None,
            provider: None,
            hooks: std::collections::HashMap::new(),
            hook_tx,
            hook_idempotency: crate::hooks::IdempotencyCache::default(),
        });

        let app = Router::new()
            .route(
                "/api/goals/{id}/message",
                axum::routing::post(goals_message),
            )
            .with_state(state);
        (app, goals)
    }

    /// Like [`goals_app`] but with a **real** `ChatSessions` (temp JSONL store, `MockProvider` that
    /// is never actually called for completions) and the `/api/goals` start route mounted — so the
    /// return-handoff path can fold a summary into a genuine parent conversation. Returns the router,
    /// the hub, the chat handle, and a freshly-created conversation id to use as `origin`.
    async fn goals_app_with_chat() -> (
        Router,
        Arc<GoalSessionHub>,
        Arc<liberado_main_agent::ChatSessions>,
        String,
    ) {
        use liberado_executor::{Budget, Executor};
        use liberado_provider::MockProvider;

        let mut hub = GoalSessionHub::new(GoalSessionStore::new());
        hub.register_pack(Arc::new(LifeOpsDemoRunner));
        let goals = Arc::new(hub);

        let root = std::env::temp_dir().join(format!("liberado-server-test-{}", Ulid::new()));
        let store = Arc::new(liberado_session_store::SessionStore::open(&root).await);
        let executor = Executor::new(
            Arc::new(MockProvider::with_script("mock", vec![])),
            Budget::default(),
        );
        let chat = Arc::new(liberado_main_agent::ChatSessions::new(
            store,
            executor,
            Arc::new(crate::state::NoTools),
        ));
        let conv = chat.create(None).await.unwrap().to_string();

        let (hook_tx, _hook_rx) = tokio::sync::mpsc::unbounded_channel();
        let state = Arc::new(AppState {
            start_time: Instant::now(),
            reactions: Arc::new(tokio::sync::Mutex::new(Vec::new())),
            dispatcher_attached: false,
            orchestrator_attached: false,
            vault_path: "/tmp/vault".to_string(),
            goals: goals.clone(),
            chat: Some(chat.clone()),
            chat_tools: 0,
            chat_tool_names: Vec::new(),
            catalog: Arc::new(liberado_common::CapabilityCatalog::new()),
            sessions_root: root,
            main_agent_capabilities: liberado_common::CapabilitySet::empty(),
            dispatcher_capabilities: liberado_common::CapabilitySet::empty(),
            config: Arc::new(test_config_with_life_grants()),
            sessions: Arc::new(Default::default()),
            model_name: None,
            provider: None,
            hooks: std::collections::HashMap::new(),
            hook_tx,
            hook_idempotency: crate::hooks::IdempotencyCache::default(),
        });

        let app = Router::new()
            .route("/api/goals", axum::routing::post(goals_start))
            .route(
                "/api/goals/{id}/message",
                axum::routing::post(goals_message),
            )
            .with_state(state);
        (app, goals, chat, conv)
    }

    fn post_json(uri: &str, body: &str) -> Request<Body> {
        Request::builder()
            .method("POST")
            .uri(uri)
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap()
    }

    /// The grant an attended `/spawn` of the life pack resolves to: it may interrupt the human, and
    /// it may write the note it was asked for. Interactivity is a capability (S6) — a session
    /// started without `AskHuman` cannot receive input at all, so these tests must grant it
    /// explicitly rather than relying on an ambient "interactive" payload flag.
    fn attended_life_grant() -> liberado_session::SessionGrant {
        liberado_session::SessionGrant {
            capabilities: life_capabilities(),
            profile: None,
            overrides: serde_json::Value::Null,
        }
    }

    fn life_capabilities() -> liberado_common::CapabilitySet {
        use liberado_common::{Capability, CapabilitySet, Zone};
        let mut capabilities = CapabilitySet::empty();
        capabilities.grant(Capability::AskHuman);
        capabilities.grant(Capability::Write(Zone::vault("tasks")));
        capabilities
    }

    /// A config whose `"life"` component holds the grant an unprofiled life session resolves to —
    /// mirroring the shipped `policy.toml`. Without this the HTTP path would resolve *zero*
    /// authority and a `/spawn`ed session would (correctly) refuse to ask the human anything, which
    /// is precisely the behavior `spawned_session_without_ask_human_never_awaits` pins down.
    fn test_config_with_life_grants() -> liberado_bootstrap::Config {
        use liberado_config::Grant;
        let mut config = liberado_bootstrap::Config::default();
        config.policy.grants.push(Grant {
            component: "life".into(),
            capabilities: life_capabilities().capabilities,
        });
        config
    }

    async fn start_interactive(goals: &Arc<GoalSessionHub>) -> String {
        goals
            .start_with_grant(
                GoalSpec {
                    id: None,
                    description: "capture a note interactively".into(),
                    success_criteria: vec![],
                    domain: DomainHint::Life,
                    max_turns: 0,
                    max_idle_secs: None,
                    origin: None,
                    profile: None,
                    payload: serde_json::json!({ "interactive": true }),
                },
                attended_life_grant(),
            )
            .await
            .unwrap()
    }

    async fn wait_awaiting(goals: &Arc<GoalSessionHub>, id: &str) {
        for _ in 0..200 {
            tokio::time::sleep(Duration::from_millis(5)).await;
            if let Some(snap) = goals.snapshot(id).await
                && snap.session.awaiting_input
            {
                return;
            }
        }
        panic!("session {id} never reached awaiting_input");
    }

    async fn wait_terminal(goals: &Arc<GoalSessionHub>, id: &str) -> SessionSnapshot {
        for _ in 0..200 {
            tokio::time::sleep(Duration::from_millis(5)).await;
            let snap = goals.snapshot(id).await.unwrap();
            if snap.session.status.is_terminal() {
                return snap;
            }
        }
        panic!("session {id} did not finish");
    }

    #[tokio::test]
    async fn message_delivers_the_answer_echoes_it_and_returns_202() {
        let (app, goals) = goals_app();
        let id = start_interactive(&goals).await;
        wait_awaiting(&goals, &id).await;

        let response = app
            .oneshot(post_json(
                &format!("/api/goals/{id}/message"),
                r#"{"text": "Weekly Review"}"#,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::ACCEPTED);

        let snap = wait_terminal(&goals, &id).await;
        assert_eq!(
            snap.session.status,
            liberado_session::SessionStatus::Succeeded
        );
        // The endpoint's `send_input` echoed the message into the transcript as `human_input`.
        assert!(snap.events.iter().any(|e| matches!(
            &e.kind,
            liberado_session::SessionEventKind::HumanInput { text } if text == "Weekly Review"
        )));
        // And the answer drove the session outcome.
        assert!(
            snap.session
                .result
                .as_ref()
                .unwrap()
                .summary
                .contains("Weekly Review")
        );
    }

    #[tokio::test]
    async fn message_to_unknown_session_is_404() {
        let (app, _goals) = goals_app();
        let response = app
            .oneshot(post_json(
                "/api/goals/does-not-exist/message",
                r#"{"text": "hello"}"#,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[test]
    fn handoff_note_includes_status_summary_artifacts_and_rejoin_hint() {
        use liberado_session::{
            DomainHint, GoalResult, GoalSessionRecord, GoalSpec, SessionStatus, TerminalKind,
        };
        let mut record = GoalSessionRecord::new(GoalSpec {
            id: Some("g_01ABC".into()),
            description: "build a hello CLI".into(),
            success_criteria: vec![],
            domain: DomainHint::Coding,
            max_turns: 0,
            max_idle_secs: None,
            origin: None,
            profile: None,
            payload: serde_json::json!({}),
        });
        record.status = SessionStatus::Succeeded;
        record.result = Some(GoalResult {
            terminal: TerminalKind::Succeeded,
            summary: "wrote src/main.rs".into(),
            artifacts: vec!["src/main.rs".into()],
            diagnostics: serde_json::json!({}),
        });
        let note = format_handoff_note(&record, "g_01ABC");
        assert!(note.contains("[coding session succeeded]"), "note: {note}");
        assert!(note.contains("build a hello CLI"));
        assert!(note.contains("Outcome: wrote src/main.rs"));
        assert!(note.contains("Artifacts: src/main.rs"));
        assert!(note.contains("/join g_01ABC"));
    }

    #[tokio::test]
    async fn message_to_finished_session_is_409() {
        let (app, goals) = goals_app();
        // A session that *could* take input (it holds AskHuman), answered and now terminal. This is
        // the real 409: not "you may not", but "you're too late".
        let id = start_interactive(&goals).await;
        wait_awaiting(&goals, &id).await;
        goals.send_input(&id, "Weekly Review").await.unwrap();
        let _ = wait_terminal(&goals, &id).await;

        let response = app
            .oneshot(post_json(
                &format!("/api/goals/{id}/message"),
                r#"{"text": "too late"}"#,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn message_to_a_session_without_ask_human_is_403_not_409() {
        // The S6 distinction the status code has to carry: this session was *never allowed* human
        // input (its grant omits AskHuman), which is an authority answer — not the timing answer a
        // 409 gives. Started with the default zero-authority grant, exactly like an unattended cron.
        let (app, goals) = goals_app();
        let id = goals
            .start(GoalSpec {
                id: None,
                description: "unattended goal".into(),
                success_criteria: vec!["done".into()],
                domain: DomainHint::Life,
                max_turns: 0,
                max_idle_secs: None,
                origin: None,
                profile: None,
                payload: serde_json::json!({ "interactive": true }),
            })
            .await
            .unwrap();

        let response = app
            .oneshot(post_json(
                &format!("/api/goals/{id}/message"),
                r#"{"text": "let me help"}"#,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn a_session_without_ask_human_never_awaits_even_when_asked_to_be_interactive() {
        // Interactivity is a capability, not a payload flag the caller can assert. Despite
        // `interactive: true`, a zero-authority grant means the pack gets a closed input channel and
        // must finish on its own rather than block on a human who can never reply.
        let (_app, goals) = goals_app();
        let id = goals
            .start(GoalSpec {
                id: None,
                description: "unattended note".into(),
                success_criteria: vec!["done".into()],
                domain: DomainHint::Life,
                max_turns: 0,
                max_idle_secs: None,
                origin: None,
                profile: None,
                payload: serde_json::json!({ "interactive": true }),
            })
            .await
            .unwrap();

        let snap = wait_terminal(&goals, &id).await;
        assert!(
            !snap.session.awaiting_input,
            "a session without AskHuman must never await a human"
        );
        assert!(snap.session.status.is_terminal());
    }

    #[tokio::test]
    async fn origin_session_folds_its_summary_into_the_parent_conversation() {
        // The full S4 return handoff, end to end: POST /api/goals with an origin → interactive
        // session → answer it → on terminal, its summary is appended to the parent conversation.
        let (app, goals, chat, conv) = goals_app_with_chat().await;

        // Spawn an interactive life session linked to the conversation (exactly what `/spawn` posts).
        let body = format!(
            r#"{{"description":"capture a note","domain":"life","payload":{{"interactive":true}},"origin":{{"conversation_id":"{conv}"}}}}"#
        );
        let resp = app
            .clone()
            .oneshot(post_json("/api/goals", &body))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::ACCEPTED);
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let id = v["session_id"].as_str().unwrap().to_string();

        wait_awaiting(&goals, &id).await;
        let resp = app
            .clone()
            .oneshot(post_json(
                &format!("/api/goals/{id}/message"),
                r#"{"text": "Weekly Review"}"#,
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::ACCEPTED);
        wait_terminal(&goals, &id).await;

        // The handoff watcher appends the summary into the parent conversation (async — poll for it).
        let conv_ulid: Ulid = conv.parse().unwrap();
        let mut folded = false;
        for _ in 0..200 {
            tokio::time::sleep(Duration::from_millis(5)).await;
            let history = chat.history(conv_ulid).await.unwrap();
            if history.iter().any(|m| {
                m.content.contains("life session succeeded") && m.content.contains("Weekly Review")
            }) {
                folded = true;
                break;
            }
        }
        assert!(
            folded,
            "return handoff did not fold the session summary into the parent conversation"
        );
    }

    // ── Forking ──────────────────────────────────────────────────────────────────────────────

    /// A router with the fork route mounted over a real `SessionStore`, seeded with a chat of
    /// `turns` (user, assistant) exchanges. Returns the router, the store, and the conversation id.
    async fn fork_app(
        turns: &[(&str, &str)],
    ) -> (Router, Arc<liberado_session_store::SessionStore>, String) {
        use liberado_conversation_store::{Author, ConversationStore, NewNode};
        use liberado_provider::Message;

        let sessions = Arc::new(liberado_session_store::SessionStore::new());
        let conv = sessions
            .create_session(liberado_session_store::NewSession {
                title: Some("original".into()),
                ..Default::default()
            })
            .await
            .id;

        let mut parent = None;
        for (q, a) in turns {
            let u = sessions
                .append(
                    conv,
                    NewNode {
                        parent_id: parent,
                        author: Author::User,
                        message: Message::user(*q),
                    },
                )
                .await
                .unwrap();
            let a = sessions
                .append(
                    conv,
                    NewNode {
                        parent_id: Some(u.id),
                        author: Author::Assistant,
                        message: Message::assistant(*a),
                    },
                )
                .await
                .unwrap();
            parent = Some(a.id);
        }

        let (hook_tx, _hook_rx) = tokio::sync::mpsc::unbounded_channel();
        let state = Arc::new(AppState {
            start_time: Instant::now(),
            reactions: Arc::new(tokio::sync::Mutex::new(Vec::new())),
            dispatcher_attached: false,
            orchestrator_attached: false,
            vault_path: "/tmp/vault".to_string(),
            goals: Arc::new(GoalSessionHub::new(GoalSessionStore::new())),
            chat: None,
            chat_tools: 0,
            chat_tool_names: Vec::new(),
            catalog: Arc::new(liberado_common::CapabilityCatalog::new()),
            sessions_root: std::path::PathBuf::from("/tmp/liberado/sessions"),
            main_agent_capabilities: liberado_common::CapabilitySet::empty(),
            dispatcher_capabilities: liberado_common::CapabilitySet::empty(),
            config: Arc::new(test_config_with_life_grants()),
            sessions: sessions.clone(),
            model_name: None,
            provider: None,
            hooks: std::collections::HashMap::new(),
            hook_tx,
            hook_idempotency: crate::hooks::IdempotencyCache::default(),
        });

        let app = Router::new()
            .route("/api/sessions/{id}/fork", axum::routing::post(session_fork))
            .with_state(state);
        (app, sessions, conv.to_string())
    }

    async fn post_fork(app: &Router, conv: &str, body: serde_json::Value) -> (StatusCode, String) {
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/sessions/{conv}/fork"))
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = resp.status();
        let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024)
            .await
            .unwrap();
        (status, String::from_utf8(bytes.to_vec()).unwrap())
    }

    #[tokio::test]
    async fn forking_a_whole_conversation_snapshots_it_and_leaves_the_original_alone() {
        let (app, store, conv) = fork_app(&[("q1", "a1"), ("q2", "a2")]).await;

        let (status, body) = post_fork(&app, &conv, serde_json::json!({})).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let fork: chat_client_contract::ForkResponse = serde_json::from_str(&body).unwrap();
        assert_eq!(fork.kept_turns, 2);
        assert_eq!(fork.total_turns, 2);

        use liberado_conversation_store::ConversationStore;
        let fork_id: Ulid = fork.id.parse().unwrap();
        let copied = store.leaf_path(fork_id, None).await.unwrap();
        assert_eq!(
            copied
                .iter()
                .map(|n| n.message.content.clone())
                .collect::<Vec<_>>(),
            vec!["q1", "a1", "q2", "a2"],
        );
        // The original still exists, unchanged, alongside the fork — that is the whole request.
        let original: Ulid = conv.parse().unwrap();
        assert_eq!(store.leaf_path(original, None).await.unwrap().len(), 4);
        assert_eq!(store.list_sessions().await.len(), 2);
    }

    #[tokio::test]
    async fn forking_after_a_turn_resolves_that_turn_to_the_right_node() {
        // The server's whole job here: a human points at a *turn*; the store speaks *nodes*.
        // `after_turn: 1` must keep q1 and the answer it got, and drop everything from q2 onward.
        let (app, store, conv) = fork_app(&[("q1", "a1"), ("q2", "a2"), ("q3", "a3")]).await;

        let (status, body) = post_fork(&app, &conv, serde_json::json!({ "after_turn": 1 })).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let fork: chat_client_contract::ForkResponse = serde_json::from_str(&body).unwrap();
        assert_eq!(fork.kept_turns, 1);
        assert_eq!(fork.total_turns, 3);

        use liberado_conversation_store::ConversationStore;
        let copied = store
            .leaf_path(fork.id.parse().unwrap(), None)
            .await
            .unwrap();
        assert_eq!(
            copied
                .iter()
                .map(|n| n.message.content.clone())
                .collect::<Vec<_>>(),
            vec!["q1", "a1"],
            "the reply to turn 1 comes along; turn 2 onward does not"
        );
    }

    #[tokio::test]
    async fn forking_past_the_last_turn_is_the_whole_conversation_not_an_error() {
        // Asking to keep more turns than exist is not a mistake worth refusing — it is just "all of
        // it", which is what a bare /fork means anyway.
        let (app, _store, conv) = fork_app(&[("q1", "a1")]).await;
        let (status, body) = post_fork(&app, &conv, serde_json::json!({ "after_turn": 99 })).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let fork: chat_client_contract::ForkResponse = serde_json::from_str(&body).unwrap();
        assert_eq!(fork.kept_turns, 1);
        assert_eq!(fork.total_turns, 1);
    }

    #[tokio::test]
    async fn forking_turn_zero_is_refused_rather_than_silently_meaning_something_else() {
        let (app, _store, conv) = fork_app(&[("q1", "a1")]).await;
        let (status, body) = post_fork(&app, &conv, serde_json::json!({ "after_turn": 0 })).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(body.contains("1-based"), "{body}");
    }

    #[tokio::test]
    async fn forking_an_unknown_session_is_404() {
        let (app, _store, _conv) = fork_app(&[("q1", "a1")]).await;
        let (status, _) = post_fork(&app, &Ulid::new().to_string(), serde_json::json!({})).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }
}
