//! Goal-session endpoints (start, stream, cancel, message, handoff).

use std::convert::Infallible;
use std::pin::Pin;
use std::sync::Arc;

use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
    response::{
        IntoResponse,
        sse::{Event, Sse},
    },
};
use futures::Stream;
use liberado_conversation_store::Ulid;
use serde::Deserialize;

use chat_client_contract::ApiError;

use crate::state::AppState;

/// The SSE item stream type shared with chat streaming.
type SseBody = Pin<Box<dyn Stream<Item = Result<Event, Infallible>> + Send>>;

/// Strip Windows extended-length prefixes (`\\?\C:\…`, `\\?\UNC\…`) so paths on the wire are
/// usable by git and readable in session records.
fn strip_windows_extended_path(path: &std::path::Path) -> String {
    let s = path.to_string_lossy();
    if let Some(rest) = s.strip_prefix(r"\\?\") {
        if let Some(unc) = rest.strip_prefix(r"UNC\") {
            return format!(r"\\{unc}");
        }
        return rest.to_string();
    }
    s.into_owned()
}

// â”€â”€ Goal sessions (scratchpad F) â€” surfaces are clients; packs own the loop â”€â”€

/// `GET /api/goals/domains` â€” which domain packs are registered (coding, life, â€¦).
pub async fn goals_domains(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    Json(serde_json::json!({
        "domains": state.goals.registered_domains(),
    }))
}

/// `GET /api/projects` — declared coding project roots for `/goal in` pickers (coding-tui S3 / G4).
///
/// Enabled entries only, in configured order. The human picking a name is the authorization
/// *moment*; the topology entry is the authorization *fact*.
pub async fn list_projects(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let projects: Vec<serde_json::Value> = state
        .config
        .enabled_projects()
        .into_iter()
        .map(|p| {
            serde_json::json!({
                "name": p.name,
                "root": p.root,
                "write_class": p.write_class,
            })
        })
        .collect();
    Json(serde_json::json!({ "projects": projects }))
}

/// `GET /api/goals` â€” list goal sessions, newest first.
pub async fn goals_list(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    Json(state.goals.list().await)
}

/// Apply `payload.interactive` to a session grant (F11).
///
/// - `interactive: false` → strip `Capability::AskHuman` so unattended/shepherd goals cannot park
///   on intake questions. The pack already skips intake when AskHuman is absent.
/// - `interactive: true` or absent → leave the profile grant unchanged (profiles may still omit
///   AskHuman; this function never *adds* it).
pub(crate) fn apply_interactive_to_grant(
    goal: &liberado_session::GoalSpec,
    mut grant: liberado_session::SessionGrant,
) -> liberado_session::SessionGrant {
    let interactive = goal.payload.get("interactive").and_then(|v| v.as_bool());
    if interactive == Some(false) {
        grant
            .capabilities
            .capabilities
            .retain(|c| *c != liberado_common::Capability::AskHuman);
    }
    grant
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
    // may interrupt a human is decided *here*, by the grant â€” not by the caller asserting it.
    // A named profile that resolves to nothing is refused, not silently downgraded to the domain
    // grant — that would run the session with authority the caller never asked for.
    let resolved = match state
        .config
        .resolve_session_profile(goal.profile.as_deref(), goal.domain.as_str())
    {
        Ok(resolved) => resolved,
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
    // A goal session needs a pack to run it, and a chat-only profile names none. Refused rather than
    // defaulted to the caller's domain: that would run the work under a pack the profile never
    // authorized, on nothing worse than picking the wrong name from a list.
    let Some(domain) = resolved.domain.as_deref() else {
        return (
            StatusCode::BAD_REQUEST,
            Json(ApiError {
                error: format!(
                    "session profile '{}' declares no domain — it is a chat profile and cannot                      run a goal session",
                    resolved.name.as_deref().unwrap_or("?")
                ),
            }),
        )
            .into_response();
    };
    // A session that may do nothing is safe, and never useful. Refuse it here rather than start it.
    //
    // With no profile named, the grant is keyed by the **domain name** (the pool rule). A domain with
    // no matching `[[grants]]` entry therefore resolves to zero authority — correct as a fail-safe,
    // but the session then burns a run failing every action with a capability gap, and the error the
    // operator sees names the *MCP* rather than the missing grant.
    //
    // `resolve_session_profile` keeps its documented empty-on-unknown-domain behaviour; the refusal
    // belongs here, where "can do nothing" is actionable and can name both remedies.
    //
    // Ordered after the chat-profile check on purpose: that one establishes `domain`, and a chat
    // profile is the more specific diagnosis of the two.
    if goal.profile.is_none() && resolved.capabilities.capabilities.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(ApiError {
                error: format!(
                    "domain '{domain}' has no capability grant, so this session could do nothing.                      Add a policy.toml [[grants]] entry with component = \"{domain}\", or name a                      session profile that carries one."
                ),
            }),
        )
            .into_response();
    }
    if domain != goal.domain.as_str() {
        goal.domain = liberado_session::DomainHint::from(domain);
    }

    // Coding project authorization (S3/G4): resolve `payload.project` / `payload.workspace_root`
    // against topology `[[projects]]` before the pack runs. Fail closed — unknown names and
    // undeclared paths never reach the coding tools.
    if domain == liberado_session::CODING_DOMAIN {
        let mut payload = match liberado_coder_agent::CodingGoalPayload::parse(&goal.payload) {
            Ok(payload) => payload,
            Err(error) => {
                return (StatusCode::BAD_REQUEST, Json(ApiError { error })).into_response();
            }
        };
        let project = payload.project().map(str::to_string);
        let workspace_root = payload.workspace_root().map(str::to_string);
        match state
            .config
            .authorize_coding_workspace(project.as_deref(), workspace_root.as_deref())
        {
            Ok(liberado_config::CodingWorkspaceAuth::Ephemeral) => {
                goal.payload = payload.into_value();
            }
            Ok(liberado_config::CodingWorkspaceAuth::Project { name, root }) => {
                // Strip Windows `\\?\` extended prefixes so session records and git tools see a
                // plain drive path (dogfood finding #1 residual).
                let root_s = strip_windows_extended_path(&root);
                payload.set_authorized_workspace(name.clone(), root_s);
                goal.payload = payload.into_value();
                // Inject ship preflight from topology when the client did not supply steps.
                // Pack still applies liberado built-in defaults when project is "liberado".
                let payload = goal
                    .payload
                    .as_object_mut()
                    .expect("CodingGoalPayload always serializes to an object");
                if payload
                    .get("preflight")
                    .and_then(|v| v.get("steps"))
                    .is_none()
                    && let Some(proj) = state.config.project_by_name(&name)
                    && let Some(preflight) = proj.ship_preflight_payload()
                {
                    payload.insert("preflight".into(), preflight);
                }
            }
            Err(e) => {
                return (
                    StatusCode::FORBIDDEN,
                    Json(ApiError {
                        error: e.to_string(),
                    }),
                )
                    .into_response();
            }
        }
    }

    // Per-goal idle wins; otherwise the profile default (E5 — hours for interactive coding).
    if goal.max_idle_secs.is_none() {
        goal.max_idle_secs = resolved.max_idle_secs;
    }
    let parts = resolved.grant_parts();
    let grant = liberado_session::SessionGrant {
        capabilities: parts.capabilities,
        profile: goal.profile.clone(),
        overrides: serde_json::to_value(&resolved.overrides).unwrap_or(serde_json::Value::Null),
        delegation: parts.delegation,
        model: parts.model.map(str::to_string),
        prompt_append: parts.prompt_append.map(str::to_string),
    };
    // F11: map `payload.interactive: false` onto the grant. The pack already skips intake when
    // AskHuman is absent; the shepherd already sends the flag. Without this strip the flag is an
    // eighth shadowed setting — sent, parsed into JSON, and never read.
    let grant = apply_interactive_to_grant(&goal, grant);

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
/// (session-focus S4 / D2). Spawned at start, it subscribes to the session's own event stream â€”
/// `subscribe()` returns catch-up history + a live receiver, so a finish between start and here is
/// not missed â€” waits for the terminal event, then appends a note via `ChatSessions::append_note`.
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
        // A park is not a finish. `SessionFinished { status: "parked" }` closes the *stream*, but
        // the session is coming back — folding a summary into the parent now would announce an
        // ending that has not happened, and the terminal-settle wait below would spin for a second
        // and then log a spurious warning.
        let is_park = |e: &liberado_session::SessionEvent| matches!(&e.kind, K::SessionFinished { status, .. } if status == "parked");
        if history.iter().any(is_park) {
            return;
        }
        let already_done = history
            .iter()
            .any(|e| matches!(e.kind, K::SessionFinished { .. }));
        if !already_done {
            loop {
                match rx.recv().await {
                    Ok(ev) if is_park(&ev) => return,
                    Ok(ev) if matches!(ev.kind, K::SessionFinished { .. }) => break,
                    Ok(_) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        }

        let Some(chat) = state.chat.as_ref() else {
            return; // no conversation store to fold into (chat disabled â€” there was no parent anyway)
        };
        // The `SessionFinished` *event* broadcasts just before `store.finish()` records the terminal
        // status/result, so read the snapshot only once the *record* has actually settled terminal â€”
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
        // No parent conversation â‡’ nothing to hand back to. That is the normal case for an
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
/// artifacts, and the `/join` hint so the human can reopen the full transcript. Compact by design â€”
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

/// `GET /api/goals/{id}` â€” session record + event history so far.
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

/// `POST /api/goals/{id}/cancel` â€” cooperative cancel of a running goal session.
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

/// `POST /api/goals/{id}/message` â€” deliver a human message into a running interactive goal
/// session (the reply to an `awaiting_input` prompt). A thin wrapper over
/// [`liberado_session::GoalSessionHub::send_input`], which echoes the message into the transcript
/// as a `human_input` event. `202 Accepted` on delivery; `404` when the session is unknown; `409`
/// when it has already finished (a terminal session accepts no input); `403` when the session's
/// grant omits `AskHuman` and so may *never* receive human input (S6) â€” an authority answer, not a
/// timing one.
/// `POST /api/goals/{id}/park` — ask a running session to wind down and land in `Parked`
/// (S2/G2). Cooperative, like cancel: it returns once the request is *accepted*, not once the pack
/// has stopped. Unlike cancel, the session keeps its awaiting-input state and can be resumed.
pub async fn goals_park(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    match state.goals.park(&id).await {
        Ok(()) => (
            StatusCode::ACCEPTED,
            Json(serde_json::json!({ "session_id": id, "status": "parking" })),
        )
            .into_response(),
        Err(e) => (StatusCode::NOT_FOUND, Json(ApiError { error: e })).into_response(),
    }
}

/// Body for [`goals_rewind`]: optional checkpoint id (default = latest in session events).
#[derive(Deserialize, Default)]
pub struct GoalRewindRequest {
    #[serde(default)]
    pub checkpoint_id: Option<String>,
}

/// Resolve the rewind workspace: a durable session worktree wins, the payload's
/// `workspace_root` is the fallback. Pure so the two failure messages are unit-tested.
fn rewind_workspace(
    durable: Option<std::path::PathBuf>,
    payload_root: Option<&str>,
) -> Result<std::path::PathBuf, String> {
    match durable {
        Some(path) if path.exists() => Ok(path),
        Some(_) => payload_root.map(std::path::PathBuf::from).ok_or_else(|| {
            "coding session has no workspace_root and no durable session worktree — cannot rewind"
                .to_string()
        }),
        None => payload_root
            .map(std::path::PathBuf::from)
            .ok_or_else(|| "coding session has no workspace_root in payload".to_string()),
    }
}

/// Latest-or-explicit checkpoint for a rewind: an explicit id that names a checkpoint event wins
/// (with the event's label + tree hash); otherwise the most recent checkpoint event is used; a
/// missing explicit id falls back to the id alone with an "explicit" label.
fn rewind_checkpoint(
    events: &[liberado_session::SessionEvent],
    want: Option<&str>,
) -> Result<(String, String, String), String> {
    use liberado_session::SessionEventKind;
    if let Some(id) = want {
        let from_ev = events.iter().rev().find_map(|e| match &e.kind {
            SessionEventKind::Checkpoint {
                id: cid,
                label,
                tree_hash,
            } if cid == id => Some((cid.clone(), label.clone(), tree_hash.clone())),
            _ => None,
        });
        return Ok(from_ev.unwrap_or_else(|| (id.to_string(), "explicit".into(), String::new())));
    }
    events
        .iter()
        .rev()
        .find_map(|e| match &e.kind {
            SessionEventKind::Checkpoint {
                id,
                label,
                tree_hash,
            } => Some((id.clone(), label.clone(), tree_hash.clone())),
            _ => None,
        })
        .ok_or_else(|| "no checkpoint events on this session — cannot rewind".to_string())
}

/// `POST /api/goals/{id}/rewind` — restore workspace files from a shadow-git checkpoint (S4).
/// Conversation/transcript is untouched. Coding sessions only (needs `workspace_root` +
/// checkpoint events). Returns the restored checkpoint id.
pub async fn goals_rewind(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    body: Option<Json<GoalRewindRequest>>,
) -> impl IntoResponse {
    let want = body.and_then(|Json(b)| b.checkpoint_id);
    let snap = match state.goals.snapshot(&id).await {
        Some(s) => s,
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(ApiError {
                    error: format!("no such goal session '{id}'"),
                }),
            )
                .into_response();
        }
    };
    if snap.session.goal.domain.as_str() != liberado_session::CODING_DOMAIN {
        return (
            StatusCode::BAD_REQUEST,
            Json(ApiError {
                error: "rewind is only supported for coding goal sessions".into(),
            }),
        )
            .into_response();
    }
    // Prefer durable session worktree (where attempt checkpoints are taken) when present.
    let workspace = match rewind_workspace(
        liberado_coder_agent::durable_session_workspace(&id),
        snap.session
            .goal
            .payload
            .get("workspace_root")
            .and_then(|v| v.as_str()),
    ) {
        Ok(w) => w,
        Err(error) => {
            return (StatusCode::BAD_REQUEST, Json(ApiError { error })).into_response();
        }
    };
    let (cp_id, label, tree_hash) = match rewind_checkpoint(&snap.events, want.as_deref()) {
        Ok(cp) => cp,
        Err(error) => {
            return (StatusCode::CONFLICT, Json(ApiError { error })).into_response();
        }
    };

    let sg = match liberado_coder_agent::ShadowGit::open_or_init(&workspace, &id) {
        Ok(s) => s,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiError {
                    error: format!("open shadow-git: {e}"),
                }),
            )
                .into_response();
        }
    };
    if let Err(e) = sg.restore(&cp_id).await {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiError {
                error: format!("restore checkpoint: {e}"),
            }),
        )
            .into_response();
    }
    tracing::info!(
        session = %id,
        checkpoint = %cp_id,
        label = %label,
        "goals_rewind: restored workspace files"
    );
    (
        StatusCode::OK,
        Json(serde_json::json!({
            "session_id": id,
            "checkpoint_id": cp_id,
            "label": label,
            "tree_hash": tree_hash,
            "restored": true,
        })),
    )
        .into_response()
}

pub async fn goals_message(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(req): Json<GoalMessageRequest>,
) -> impl IntoResponse {
    use liberado_session::SendInputError;
    // A parked session has no live pack to deliver into, so `send_input` cannot take the answer â€”
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
        // `Parked` is a 409 like the others â€” the answer cannot be delivered â€” but its *message*
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

/// `GET /api/goals/{id}/stream` â€” SSE: catch-up history then live events.
/// Events use `event:` names matching [`liberado_session::SessionEventKind`] type tags
/// (`session_started`, `tool_started`, `session_finished`, â€¦); `data` is full JSON.
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

    // Same heartbeat as the chat stream, for the same reason: a session watching a long tool call
    // emits no events for as long as that tool takes, and an idle connection is one an intermediary
    // may close. See `super::chat::keep_alive`.
    Sse::new(Box::pin(stream) as SseBody)
        .keep_alive(super::chat::keep_alive())
        .into_response()
}

/// Encode a kernel [`liberado_session::SessionEvent`] as SSE. Event names are the kind's serde
/// tags â€” the same converged vocabulary `to_sse` maps chat's `AgentEvent` onto, decoded
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
        K::CriticVerdict { .. } => "critic_verdict",
        K::FileChanged { .. } => "file_changed",
        K::Checkpoint { .. } => "checkpoint",
        K::LoopGuard { .. } => "loop_guard",
        K::SessionFinished { .. } => "session_finished",
        K::Failed { .. } => "failed",
    };
    let data = serde_json::to_string(ev).unwrap_or_else(|_| "{}".into());
    Event::default().event(name).data(data)
}

/// Cap on the diff body returned by [`goals_diff`].
///
/// `git diff HEAD` over a large working tree is unbounded, and this endpoint is polled by a UI —
/// the whole thing is read into memory and sent. A megabyte is far more than a human reads and
/// still small enough that a runaway workspace cannot cost the daemon its heap.
const MAX_DIFF_BYTES: usize = 1024 * 1024;

/// Truncate `diff` to [`MAX_DIFF_BYTES`] on a char boundary, announcing it when it happens.
///
/// Separate from the handler so the bounding rule is testable without a git repo holding a
/// megabyte of uncommitted change: the handler's job is running the command, this is the rule.
fn bound_diff(diff: String) -> String {
    if diff.len() <= MAX_DIFF_BYTES {
        return diff;
    }
    let mut cut = MAX_DIFF_BYTES;
    while cut > 0 && !diff.is_char_boundary(cut) {
        cut -= 1;
    }
    format!(
        "{}

[diff truncated at {MAX_DIFF_BYTES} bytes — run `git diff HEAD` in the workspace          for the rest]",
        &diff[..cut]
    )
}

/// `GET /api/goals/{id}/diff` — workspace `git diff HEAD` for a coding goal session.
pub async fn goals_diff(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let snap = match state.goals.snapshot(&id).await {
        Some(s) => s,
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(ApiError {
                    error: format!("goal session '{id}' not found"),
                }),
            )
                .into_response();
        }
    };
    let Some(ws) = snap
        .session
        .goal
        .payload
        .get("workspace_root")
        .and_then(|v| v.as_str())
    else {
        return (
            StatusCode::NOT_FOUND,
            Json(ApiError {
                error: "this goal session has no workspace (not a coding session)".into(),
            }),
        )
            .into_response();
    };
    if ws.is_empty() || !std::path::Path::new(ws).is_dir() {
        return (
            StatusCode::NOT_FOUND,
            Json(ApiError {
                error: "workspace not available".into(),
            }),
        )
            .into_response();
    }
    match tokio::time::timeout(
        std::time::Duration::from_secs(10),
        liberado_common::process::command("git")
            .args(["diff", "HEAD", "--", "."])
            .current_dir(ws)
            .output(),
    )
    .await
    {
        Err(_elapsed) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiError {
                error: "git diff timed out".into(),
            }),
        )
            .into_response(),
        Ok(Err(_e)) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiError {
                error: "could not run git diff".into(),
            }),
        )
            .into_response(),
        Ok(Ok(out)) if out.status.success() => {
            let d = String::from_utf8_lossy(&out.stdout).into_owned();
            let body = if d.is_empty() {
                "(no changes)".into()
            } else {
                bound_diff(d)
            };
            (
                StatusCode::OK,
                [("content-type", "text/plain; charset=utf-8")],
                body,
            )
                .into_response()
        }
        Ok(Ok(_out)) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiError {
                error: "git diff failed".into(),
            }),
        )
            .into_response(),
    }
}

#[cfg(test)]
#[path = "goals_goal_message_tests.rs"]
mod goal_message_tests;

#[cfg(test)]
#[path = "goals_project_auth_http_tests.rs"]
mod project_auth_http_tests;

#[cfg(test)]
#[path = "goals_rewind_tests.rs"]
mod rewind_tests;

#[cfg(test)]
#[path = "goals_sse_tests.rs"]
mod sse_tests;

#[cfg(test)]
#[path = "goals_pure_helper_tests.rs"]
mod pure_helper_tests;
