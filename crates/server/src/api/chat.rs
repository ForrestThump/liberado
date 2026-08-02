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
use tokio::sync::broadcast;
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
    /// Open this chat as [`Visibility::Background`](liberado_session::Visibility::Background):
    /// durable, but filtered out of the sidebar. Same rule as `incognito` / `profile` — only
    /// consulted when `session` is absent (it describes how to *create* one).
    ///
    /// Live conformance (and any other machinery that must not pollute the human's chat list) sets
    /// this rather than inventing a parallel session path.
    #[serde(default)]
    pub background: bool,
    /// Session profile for a chat being **created** by this request.
    ///
    /// Same rule as `incognito`: consulted only when `session` is absent, because it describes how to
    /// open a conversation. Switching an existing one is
    /// `POST /api/conversations/{id}/profile` — a deliberate, recorded act, not a field on a message.
    ///
    /// Without this the first turn of every chat ran on the default grant, since a profile could not
    /// be chosen before the session it applies to existed. For a "basic chat" profile that is exactly
    /// the turn you wanted scoped.
    #[serde(default)]
    pub profile: Option<String>,
    /// Run **this turn** on this model.
    ///
    /// Unlike `incognito` and `profile`, this is honoured whether or not `session` is set: a model is
    /// a property of a turn, not of how a conversation was opened. The turn stamps it onto the log,
    /// after which the conversation stays there on its own and this field is only needed to *change*
    /// the answer.
    ///
    /// It exists because a chat has no id until its first message creates one, so a client cannot
    /// scope a model to a conversation that does not exist yet — the same gap `profile` above was
    /// added to close, and for the same reason. Without it the only thing a client could do with a
    /// pick made before the first message was swap the daemon-wide default, which silently retuned
    /// every *other* conversation. Carrying the choice on the message that opens the chat means the
    /// common path — new chat, pick a model, type — never touches global state at all.
    #[serde(default)]
    pub model: Option<String>,
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
) -> axum::response::Response {
    chat_stream_core(
        state,
        req.message,
        req.session,
        req.incognito,
        req.background,
        req.profile,
        req.model,
    )
    .await
}

/// `GET /api/chat/stream?message=â€¦` â€” the `EventSource`-friendly variant (browsers can't `POST` an
/// `EventSource`). Same SSE contract as the POST form, including `?session=â€¦` to continue a chat.
pub async fn chat_stream_get(
    State(state): State<Arc<AppState>>,
    Query(req): Query<ChatRequest>,
) -> axum::response::Response {
    chat_stream_core(
        state,
        req.message,
        req.session,
        req.incognito,
        req.background,
        req.profile,
        req.model,
    )
    .await
}

/// The SSE item stream `chat_stream_core` returns. Boxed because the function has several early
/// returns (chat disabled, create failed, the live turn) whose `impl Stream` types would otherwise
/// differ â€” one named type lets them share a return.
type SseBody = Pin<Box<dyn Stream<Item = Result<Event, Infallible>> + Send>>;

/// The heartbeat every SSE response here carries.
///
/// A turn that delegates sends **nothing** between the `delegate` tool starting and the subagent
/// finishing — observed at 3m28s on 2026-08-01, and there is no upper bound on it. Without a
/// heartbeat that is an idle connection, and every idle timeout between the browser and the daemon
/// (proxy, mobile radio, load balancer) is free to close it. When one does, the turn is cancelled
/// and its answer discarded, so the cost of a missing keep-alive is the whole reply.
///
/// The payload is an SSE comment: `EventSource` ignores it per spec, and the native parser skips
/// comment-only blocks (`chat_client_contract::native` — `comments_only_block_returns_nothing`), so
/// no client sees a frame for it.
pub(super) fn keep_alive() -> axum::response::sse::KeepAlive {
    axum::response::sse::KeepAlive::new().interval(KEEP_ALIVE_INTERVAL)
}

/// How often an otherwise-silent stream emits a heartbeat.
///
/// Named so it is a decision rather than a literal: it has to stay comfortably under the shortest
/// idle timeout anywhere between a browser and the daemon — proxies commonly use 60s — with room to
/// miss a tick. Raising it past that silently reintroduces the disconnect it exists to prevent, and
/// nothing else in the system would notice.
pub(super) const KEEP_ALIVE_INTERVAL: std::time::Duration = std::time::Duration::from_secs(15);

async fn chat_stream_core(
    state: Arc<AppState>,
    message: String,
    session: Option<Ulid>,
    incognito: bool,
    background: bool,
    profile: Option<String>,
    model: Option<String>,
) -> axum::response::Response {
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
        return Sse::new(stream_with_session(None, rx))
            .keep_alive(keep_alive())
            .into_response();
    };

    // Resolve the session up front (creating one on the first message), so we can announce it to the
    // client *before* the agent events. A creation failure becomes a single `failed` event.
    // Resolve the requested profile before creating anything: an unknown name must fail the request
    // rather than quietly open a chat on the default grant, which is wider than whatever was asked
    // for. Same fail-closed rule the switch endpoint follows.
    let grant = match profile.as_deref() {
        None => None,
        Some(name) => match state.config.resolve_session_profile(Some(name), "") {
            Ok(resolved) => {
                let parts = resolved.grant_parts();
                Some(liberado_session::SessionGrant {
                    capabilities: parts.capabilities,
                    profile: parts.profile,
                    overrides: serde_json::to_value(&resolved.overrides)
                        .unwrap_or(serde_json::Value::Null),
                    delegation: parts.delegation,
                    model: parts.model.map(str::to_string),
                    prompt_append: parts.prompt_append.map(str::to_string),
                })
            }
            Err(e) => {
                let msg = e.to_string();
                tokio::spawn(async move {
                    let _ = tx.send(AgentEvent::Error(msg)).await;
                });
                return Sse::new(stream_with_session(None, rx))
                    .keep_alive(keep_alive())
                    .into_response();
            }
        },
    };

    let session = match session {
        Some(id) => id,
        None => {
            // Incognito wins over background when both are set: RAM-only already never lists.
            // Background + grant is the conformance path (durable, out of sidebar, scoped).
            let created = if incognito {
                sessions.create_incognito(None).await
            } else if background {
                sessions
                    .create_background(None, grant.unwrap_or_default())
                    .await
            } else if let Some(grant) = grant {
                sessions.create_with_grant(None, grant).await
            } else {
                sessions.create(None).await
            };
            match created {
                Ok(id) => id,
                Err(e) => {
                    tracing::warn!(error = %e, "chat stream could not create a conversation");
                    tokio::spawn(async move {
                        let _ = tx.send(AgentEvent::Error(e.to_string())).await;
                    });
                    return Sse::new(stream_with_session(None, rx))
                        .keep_alive(keep_alive())
                        .into_response();
                }
            }
        }
    };

    // Now that the conversation has an id, a model asked for on the request becomes a pick scoped to
    // it. Recorded through the same seam `POST /api/models/select` uses, rather than a second way of
    // saying the same thing: the turn below consumes it and stamps it onto the log.
    //
    // This is deliberately after creation, which is the whole point — before it there is no id to
    // scope to, and that is what made a client reach for the daemon-wide default instead.
    if let Some(model) = model {
        sessions.select_model(session, model);
    }

    // Start the turn, or join the one already running. The response is now a *subscriber*: dropping
    // it ends the viewing, not the work. That is the whole change - a refresh, a suspended tab, or a
    // client that simply times out used to cost the answer, because the turn was owned by the
    // connection watching it.
    //
    // `tx` is no longer the turn's channel; it only carried pre-turn failure events, so it goes.
    drop(tx);
    let (replay, live) = sessions.start_or_attach(session, &message);

    Sse::new(attached_stream(Some(session), replay, live))
        .keep_alive(keep_alive())
        .into_response()
}

/// Prepend a `session` SSE event (the conversation id) ahead of the agent event stream, so the
/// client records the id and sends it back as `?session=â€¦` on the next turn. `None` means no session
/// was resolved (chat disabled or creation failed) â€” only the body's `failed` event is emitted.
/// `GET /api/conversations/{id}/attach` - SSE: join a turn already in flight, without starting one.
///
/// What a surface calls after a reload. It has a conversation open and needs to know whether
/// anything is still happening in it; posting the message again would start a second turn, and
/// polling the transcript would show nothing until the turn ended.
///
/// `409` when nothing is running - not `404`, which would say the conversation does not exist.
pub async fn attach_conversation(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> axum::response::Response {
    let Some(sessions) = state.chat.as_ref() else {
        return chat_disabled();
    };
    let Ok(session) = id.parse::<Ulid>() else {
        return (
            StatusCode::BAD_REQUEST,
            Json(ApiError {
                error: format!("not a conversation id: {id}"),
            }),
        )
            .into_response();
    };
    match sessions.attach(session) {
        Some((replay, live)) => Sse::new(attached_stream(Some(session), replay, live))
            .keep_alive(keep_alive())
            .into_response(),
        None => (
            StatusCode::CONFLICT,
            Json(ApiError {
                error: "no turn is running for this conversation".into(),
            }),
        )
            .into_response(),
    }
}

/// `POST /api/conversations/{id}/cancel` - stop the turn running for this conversation.
///
/// The explicit stop. It has to exist because closing the stream no longer cancels anything: that
/// used to be the only way to halt a turn, and detaching the turn from the connection would
/// otherwise have removed the stop button entirely rather than fixing it.
///
/// Cancelling still persists nothing - the same rollback a disconnect used to give. What changed is
/// only the trigger: from "nobody is watching" to "someone asked".
pub async fn cancel_conversation_turn(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> axum::response::Response {
    let Some(sessions) = state.chat.as_ref() else {
        return chat_disabled();
    };
    let Ok(session) = id.parse::<Ulid>() else {
        return (
            StatusCode::BAD_REQUEST,
            Json(ApiError {
                error: format!("not a conversation id: {id}"),
            }),
        )
            .into_response();
    };
    if sessions.cancel_turn(session) {
        tracing::info!(session = %session, "chat turn cancelled by request; persisted nothing");
        StatusCode::ACCEPTED.into_response()
    } else {
        (
            StatusCode::CONFLICT,
            Json(ApiError {
                error: "no turn is running for this conversation".into(),
            }),
        )
            .into_response()
    }
}

/// The shared "chat is off" response for the endpoints above.
fn chat_disabled() -> axum::response::Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(ApiError {
            error: "chat is disabled".into(),
        }),
    )
        .into_response()
}

/// The SSE body for a client attached to a turn: the session id, everything that has already
/// happened, then the live feed.
///
/// Replay-then-live is what makes a reconnect useful rather than merely non-destructive. A client
/// that reattaches three seconds into an answer gets those three seconds, not a blank pane that
/// fills in from wherever the turn happens to be.
///
/// A `Lagged` receiver is skipped rather than closed: falling behind costs that client some tokens,
/// and ending its stream instead would turn a slow reader into a lost answer, which is the failure
/// this whole change exists to remove. The terminal event still arrives.
fn attached_stream(
    session: Option<Ulid>,
    replay: Vec<AgentEvent>,
    live: broadcast::Receiver<AgentEvent>,
) -> SseBody {
    let head = futures::stream::once(async move {
        match session {
            Some(id) => Ok(Event::default().event("session").data(id.to_string())),
            None => Ok(Event::default().comment("no session")),
        }
    });
    let caught_up = futures::stream::iter(replay.into_iter().map(|e| Ok(to_sse(e))));
    let body = async_stream::stream! {
        let mut live = live;
        loop {
            match live.recv().await {
                Ok(event) => {
                    let terminal = matches!(event, AgentEvent::Done | AgentEvent::Error(_));
                    yield Ok(to_sse(event));
                    if terminal {
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    };
    Box::pin(head.chain(caught_up).chain(body))
}

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

    // Same rule as the streaming path: the model rides the message, scoped to the conversation the
    // message belongs to. Both handlers honour it or neither should — a field that works on one of
    // two endpoints is a trap for whoever uses the other.
    if let Some(model) = req.model {
        sessions.select_model(session, model);
    }

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

/// `GET /api/profiles` — the session profiles a human may choose from.
///
/// Enabled entries only, in configured order, so the operator controls the list a picker shows.
pub async fn list_profiles(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let profiles: Vec<serde_json::Value> = state
        .config
        .enabled_session_profiles()
        .into_iter()
        .map(|p| {
            serde_json::json!({
                "name": p.name,
                "description": p.description,
                // Present for a pack profile (`/spawn`), absent for a chat profile (`/profile`).
                // The client uses it to say which list an entry belongs in.
                "domain": p.domain,
                "delegation": p.delegation,
                "model": p.model,
            })
        })
        .collect();
    Json(serde_json::json!({ "profiles": profiles }))
}

#[derive(Deserialize)]
pub struct ProfileRequest {
    /// The profile to switch to. `None` clears back to the daemon's default grant.
    #[serde(default)]
    pub name: Option<String>,
}

/// `POST /api/conversations/{id}/profile` — switch which session profile a chat runs under.
///
/// # This is the human-only authority path
///
/// Switching a profile can *widen* what a conversation may do. That is correct — narrow-only
/// (Decision 4) governs delegation, and a human re-authorising their own chat from operator-authored
/// `policy.toml` is a different act — but it is only correct **because a human does it**.
///
/// So: reachable from surfaces, and **never registered as a tool in any runtime catalog**. The face
/// agent's `delegate` cannot reach it and no MCP exposes it, which is what makes "the agent cannot
/// re-authorise itself" a structural property rather than a convention.
///
/// `POST` rather than `GET` is load-bearing for the same reason. A granted web-fetching MCP can
/// reach the daemon's own API over loopback (see `docs/reference/api.md`), and a fetcher that only
/// issues `GET`s cannot reach this. That is an incidental defence, not the guarantee — the guarantee
/// is the tool catalog — but it is the reason not to make this a convenience `GET`.
pub async fn set_conversation_profile(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Ulid>,
    Json(req): Json<ProfileRequest>,
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

    let grant = match req.name.as_deref() {
        // Clearing is a real choice, not an error: it returns the chat to the daemon's default
        // grant, which is what every conversation ran under before profiles existed.
        None => liberado_session::SessionGrant::default(),
        Some(name) => {
            // `resolve_session_profile` fails closed on an unknown or disabled name — a typo must
            // never resolve to "no profile", which would silently mean the *default* grant.
            //
            // The domain fallback is unused here: it only applies when no profile is named, and a
            // name is named. A chat profile has no domain and that is fine — unlike `/spawn`, a
            // conversation needs no pack.
            match state.config.resolve_session_profile(Some(name), "") {
                Ok(resolved) => {
                    let parts = resolved.grant_parts();
                    liberado_session::SessionGrant {
                        capabilities: parts.capabilities,
                        profile: parts.profile,
                        overrides: serde_json::to_value(&resolved.overrides)
                            .unwrap_or(serde_json::Value::Null),
                        delegation: parts.delegation,
                        model: parts.model.map(str::to_string),
                        prompt_append: parts.prompt_append.map(str::to_string),
                    }
                }
                Err(e) => {
                    return (
                        StatusCode::BAD_REQUEST,
                        Json(ApiError {
                            error: e.to_string(),
                        }),
                    )
                        .into_response();
                }
            }
        }
    };

    let label = grant.profile.clone();
    match sessions.set_profile(id, grant).await {
        Ok(()) => {
            tracing::info!(
                conversation = %id,
                profile = label.as_deref().unwrap_or("(default)"),
                "session profile switched by a human"
            );
            Json(serde_json::json!({
                "conversation": id.to_string(),
                "profile": label,
                // Said plainly so a surface can tell the human why nothing changed yet.
                "applies": "next turn",
            }))
            .into_response()
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
    match sessions.history_nodes(id).await {
        Ok(nodes) => {
            // Nodes carry `model` (which Message alone does not). Dropping the stamp here is what
            // made Tier 3 P2's model cross-check vacuous — every history reply had no model field.
            let messages: Vec<ChatMessage> =
                nodes.into_iter().map(chat_message_from_node).collect();
            // Read from the session's own header rather than tracked client-side: a conversation
            // opened in a second tab, or after a restart, must show the authority it actually runs
            // under.
            let profile = state
                .sessions
                .session(id)
                .await
                .and_then(|h| h.grant.profile);
            Json(ConversationHistoryResponse {
                messages,
                profile,
                // Asked at read time, not remembered: a client opening this conversation needs to
                // know whether the reply it cannot see is still coming.
                turn_running: sessions.turn_running(id),
                // Asked after `turn_running` and never alongside it: a live turn also ends on the
                // human's message, and calling that dead would mark every in-flight turn as failed.
                turn_unanswered: sessions.last_turn_unanswered(id).await,
            })
            .into_response()
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

/// Converts one stored transcript node into the wire `ChatMessage` — the single conversion point
/// that keeps `GET /api/conversations/{id}` honoring `chat-client-contract` instead of leaking an
/// internal type through a hand-rolled `serde_json::json!` literal.
///
/// Carries [`MessageNode::model`] so clients (and the Tier 3 suite) can cross-check which model
/// actually ran a turn without a second API.
fn chat_message_from_node(n: liberado_conversation_store::MessageNode) -> ChatMessage {
    let m = n.message;
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
        model: n.model,
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
    use liberado_conversation_store::ConversationStore;
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
        sessions: Arc<SessionStore>,
        state: Arc<crate::state::AppState>,
        _dir: tempfile::TempDir,
    }

    async fn harness() -> Harness {
        harness_scripted(vec![]).await
    }

    /// A harness whose provider will actually answer, for the tests that need a turn to complete.
    async fn harness_scripted(script: Vec<liberado_provider::CompletionResponse>) -> Harness {
        let dir = tempfile::tempdir().unwrap();
        let sessions = Arc::new(SessionStore::open(dir.path()).await);
        let executor = Executor::new(
            Arc::new(MockProvider::with_script("mock", script)),
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
            .route("/api/chat", axum::routing::post(super::chat))
            .with_state(state.clone());
        Harness {
            app,
            chat,
            sessions,
            state,
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

    /// The heartbeat, on the real response the handler returns.
    ///
    /// Asserted because its absence is invisible: a stream with no keep-alive looks identical to one
    /// with a keep-alive that has simply not ticked yet, and the symptom shows up minutes later as a
    /// dropped connection on someone else's machine. On 2026-08-01 a delegated turn sent nothing for
    /// 3m28s and the connection died before the answer arrived.
    ///
    /// `start_paused` runs tokio's clock on demand: the stream below never yields, so time advances
    /// straight to the heartbeat's timer and the test finishes instantly rather than waiting the real
    /// 15 seconds.
    #[tokio::test(start_paused = true)]
    async fn an_idle_stream_still_sends_a_heartbeat() {
        use http_body_util::BodyExt;

        let idle = Box::pin(futures::stream::pending::<Result<Event, Infallible>>()) as SseBody;
        let response = Sse::new(idle)
            .keep_alive(super::keep_alive())
            .into_response();

        // Bounded, so losing the keep-alive fails this test instead of hanging it. Without a
        // heartbeat the body never yields, and an unbounded await would block CI rather than report.
        // The bound is virtual time too, and later than the heartbeat, so the heartbeat still wins.
        let frame =
            tokio::time::timeout(super::KEEP_ALIVE_INTERVAL * 4, response.into_body().frame())
                .await
                .expect("no heartbeat arrived: an idle stream produced nothing before the deadline")
                .expect("an idle stream must still produce a frame")
                .expect("the heartbeat frame must not be an error");
        let bytes = frame.into_data().expect("a data frame");

        // An SSE comment: the wire form every client already ignores.
        assert_eq!(
            &bytes[..],
            b":

",
            "expected a keep-alive comment, got {:?}",
            String::from_utf8_lossy(&bytes)
        );
    }

    /// The interval is a decision, not a literal. Too long and it stops clearing the idle timeouts it
    /// exists for — proxies commonly close at 60s — and nothing else in the system would notice.
    #[test]
    fn the_heartbeat_stays_inside_common_idle_timeouts() {
        assert!(
            super::KEEP_ALIVE_INTERVAL >= std::time::Duration::from_secs(1)
                && super::KEEP_ALIVE_INTERVAL <= std::time::Duration::from_secs(30),
            "keep-alive interval {:?} is outside the range that clears a 60s proxy timeout",
            super::KEEP_ALIVE_INTERVAL
        );
    }

    // ── `model` on the request ───────────────────────────────────────────────────────────────

    /// A model asked for on the message must reach the turn — asserted on the **stamp the turn
    /// left**, not on the request parsing, because parsing correctly and then being ignored is
    /// exactly how this failed live: the field existed nowhere, the picker fell back to the
    /// daemon-wide swap, and every other conversation moved with it.
    #[tokio::test]
    async fn a_model_on_the_request_is_the_model_the_turn_runs_on() {
        let h = harness_scripted(vec![liberado_provider::CompletionResponse::text("ok")]).await;
        let id = h.chat.create(None).await.unwrap();

        let resp = h
            .app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/chat")
                    .header("content-type", "application/json")
                    .body(Body::from(format!(
                        r#"{{"message":"hi","session":"{id}","model":"picked/one"}}"#
                    )))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let nodes = h.sessions.leaf_path(id, None).await.unwrap();
        let user = nodes
            .iter()
            .find(|n| matches!(n.author, liberado_conversation_store::Author::User))
            .expect("the user message is persisted before inference");
        assert_eq!(
            user.model.as_deref(),
            Some("picked/one"),
            "the turn ran on the daemon default instead of the model the request asked for"
        );
    }

    /// The positive control for the test above. Without it, a handler that stamped every turn with
    /// some fixed string would pass — and so would one that ignored `model` while the default
    /// happened to match it, which is precisely the confound that made the first live test of this
    /// feature unreadable.
    #[tokio::test]
    async fn no_model_on_the_request_leaves_the_default_alone() {
        let h = harness_scripted(vec![liberado_provider::CompletionResponse::text("ok")]).await;
        let id = h.chat.create(None).await.unwrap();

        let resp = h
            .app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/chat")
                    .header("content-type", "application/json")
                    .body(Body::from(format!(
                        r#"{{"message":"hi","session":"{id}"}}"#
                    )))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let nodes = h.sessions.leaf_path(id, None).await.unwrap();
        let user = nodes
            .iter()
            .find(|n| matches!(n.author, liberado_conversation_store::Author::User))
            .unwrap();
        assert_eq!(
            user.model.as_deref(),
            Some("mock"),
            "with nothing asked for, the turn takes the provider's own model"
        );
    }

    #[test]
    fn chat_message_from_node_carries_the_store_model_stamp() {
        use liberado_conversation_store::{Author, MessageNode, Ulid};
        use liberado_provider::{Message, Role};

        let node = MessageNode {
            id: Ulid::new(),
            parent_id: None,
            conversation_id: Ulid::new(),
            author: Author::Assistant,
            created_at: chrono::Utc::now(),
            message: Message {
                role: Role::Assistant,
                content: "hi".into(),
                tool_calls: Vec::new(),
                tool_call_id: None,
            },
            model: Some("vendor/slug".into()),
        };
        let wire = chat_message_from_node(node);
        assert_eq!(wire.role, "assistant");
        assert_eq!(wire.model.as_deref(), Some("vendor/slug"));
    }

    /// Both transports, because `EventSource` can only `GET` and so the WebUI's picks arrive as a
    /// query parameter while every other client sends JSON. A field that works on one of the two is
    /// worse than one that works on neither, because it looks fine wherever you happen to test it.
    #[test]
    fn model_parses_from_both_the_query_string_and_json() {
        let q: ChatRequest = serde_urlencoded::from_str("message=hi&model=vendor%2Fslug").unwrap();
        assert_eq!(q.model.as_deref(), Some("vendor/slug"));

        let j: ChatRequest =
            serde_json::from_str(r#"{"message":"hi","model":"vendor/slug"}"#).unwrap();
        assert_eq!(j.model.as_deref(), Some("vendor/slug"));

        let absent: ChatRequest = serde_urlencoded::from_str("message=hi").unwrap();
        assert!(absent.model.is_none());
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

    // ── Profile switching ────────────────────────────────────────────────────────────────────

    async fn post_profile(app: &Router, id: &str, body: &str) -> (StatusCode, String) {
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/conversations/{id}/profile"))
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = resp.status();
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        (status, String::from_utf8_lossy(&bytes).into_owned())
    }

    fn profile_router(state: Arc<crate::state::AppState>) -> Router {
        Router::new()
            .route(
                "/api/conversations/{id}/profile",
                axum::routing::post(super::set_conversation_profile),
            )
            .with_state(state)
    }

    /// A typo must not resolve to "no profile", which would silently mean the *default* grant — a
    /// wider one than the profile being asked for. `resolve_session_profile` fails closed and this
    /// is the endpoint honouring that rather than swallowing it.
    #[tokio::test]
    async fn an_unknown_profile_is_refused_and_the_grant_is_untouched() {
        let h = harness().await;
        let id = h.chat.create(None).await.unwrap();
        let app = profile_router(h.state.clone());

        let (status, body) = post_profile(&app, &id.to_string(), r#"{"name":"nonesuch"}"#).await;

        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(body.contains("nonesuch"), "the error must name it: {body}");
        assert!(
            h.sessions
                .session(id)
                .await
                .expect("still there")
                .grant
                .profile
                .is_none(),
            "a refused switch must leave the conversation on the grant it had"
        );
    }

    #[tokio::test]
    async fn switching_to_an_unknown_conversation_is_404() {
        let h = harness().await;
        let app = profile_router(h.state.clone());
        let (status, _) = post_profile(&app, &Ulid::new().to_string(), r#"{"name":null}"#).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    /// Clearing is a real choice, not an error: it returns a chat to the daemon's default grant,
    /// which is what every conversation ran under before profiles existed.
    #[tokio::test]
    async fn clearing_the_profile_is_allowed_and_records_a_note() {
        let h = harness().await;
        let id = h.chat.create(None).await.unwrap();
        let app = profile_router(h.state.clone());

        let (status, body) = post_profile(&app, &id.to_string(), r#"{"name":null}"#).await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("next turn"), "say when it applies: {body}");

        // The switch is recorded in the transcript, not only in the header — a change of authority
        // the human cannot see in the thread is not meaningfully recorded.
        let nodes = liberado_conversation_store::ConversationStore::leaf_path(
            h.sessions.as_ref(),
            id,
            None,
        )
        .await
        .unwrap();
        assert!(
            nodes.iter().any(|n| matches!(
                &n.author,
                liberado_conversation_store::Author::Named(name)
                    if name == liberado_main_agent::PROFILE_AUTHOR
            )),
            "a profile-authored note must be on the transcript"
        );
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
