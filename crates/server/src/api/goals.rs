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
mod goal_message_tests {

    //! HTTP-level integration tests for `POST /api/goals/{id}/message`, against a real `axum::Router`
    //! wired to a `GoalSessionHub` with the life-ops demo pack (the same pattern as `hooks.rs`).
    /// The bound must announce itself. A silently truncated diff reads as a complete one, which is
    /// worse than a large response: the human concludes the change set is smaller than it is.
    ///
    /// Scope (R5): this exercises the bounding rule, not the handler — running the real endpoint
    /// would need a git workspace carrying a megabyte of uncommitted change.
    #[test]
    fn an_oversized_diff_is_truncated_and_says_so() {
        let small = "diff --git a/x b/x
+one line
"
        .to_string();
        assert_eq!(
            super::bound_diff(small.clone()),
            small,
            "a diff under the cap must be returned byte-for-byte"
        );

        let huge = "x".repeat(super::MAX_DIFF_BYTES + 5_000);
        let bounded = super::bound_diff(huge);
        assert!(
            bounded.len() < super::MAX_DIFF_BYTES + 500,
            "must actually shrink, got {} bytes",
            bounded.len()
        );
        assert!(
            bounded.contains("diff truncated"),
            "truncation must be visible in the body, not silent"
        );
    }

    /// Cutting at a fixed byte offset can land mid-codepoint. `String` will not hold an invalid
    /// slice, so getting this wrong is a panic on any workspace with non-ASCII in its diff.
    #[test]
    fn truncation_lands_on_a_char_boundary() {
        // 3 bytes wide, and the cap is not a multiple of 3 — so a naive cut lands *inside* a
        // codepoint. A 2-byte char would align with the even cap and prove nothing.
        assert_ne!(
            super::MAX_DIFF_BYTES % 3,
            0,
            "fixture only bites on a misaligned cap"
        );
        let huge = "€".repeat(super::MAX_DIFF_BYTES);
        let bounded = super::bound_diff(huge);
        assert!(bounded.contains("diff truncated"));
    }

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
    /// a test can poll session state directly (start a session, wait for `awaiting_input`, â€¦).
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
            watcher_active: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            vault_path: "/tmp/vault".to_string(),
            goals: goals.clone(),
            chat: None,
            chat_tools: 0,
            chat_tool_names: Vec::new(),
            catalog: Arc::new(liberado_common::CapabilityCatalog::new()),
            data_dir: std::path::PathBuf::from("/tmp/liberado"),
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
            live_mcp: liberado_bootstrap::LiveMcpController::empty(),
            drain: crate::shutdown::DrainGate::default(),
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
    /// is never actually called for completions) and the `/api/goals` start route mounted â€” so the
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
            watcher_active: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            vault_path: "/tmp/vault".to_string(),
            goals: goals.clone(),
            chat: Some(chat.clone()),
            chat_tools: 0,
            chat_tool_names: Vec::new(),
            catalog: Arc::new(liberado_common::CapabilityCatalog::new()),
            data_dir: root.clone(),
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
            live_mcp: liberado_bootstrap::LiveMcpController::empty(),
            drain: crate::shutdown::DrainGate::default(),
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
            ..Default::default()
        }
    }

    fn grant_with_ask_human() -> liberado_session::SessionGrant {
        use liberado_common::{Capability, CapabilitySet, Zone};
        let mut capabilities = CapabilitySet::empty();
        capabilities.grant(Capability::AskHuman);
        capabilities.grant(Capability::Write(Zone::vault("tasks")));
        liberado_session::SessionGrant {
            capabilities,
            profile: None,
            overrides: serde_json::Value::Null,
            ..Default::default()
        }
    }

    fn goal_with_interactive(interactive: Option<bool>) -> liberado_session::GoalSpec {
        let payload = match interactive {
            Some(flag) => serde_json::json!({ "interactive": flag }),
            None => serde_json::json!({}),
        };
        GoalSpec {
            id: None,
            description: "shepherd kickback".into(),
            success_criteria: vec![],
            domain: DomainHint::Coding,
            max_turns: 0,
            max_idle_secs: None,
            origin: None,
            profile: None,
            payload,
        }
    }

    /// F11: shepherd sends `interactive: false`; the grant must drop AskHuman.
    #[test]
    fn interactive_false_strips_ask_human_from_the_grant() {
        use liberado_common::{Capability, Zone};
        let grant =
            apply_interactive_to_grant(&goal_with_interactive(Some(false)), grant_with_ask_human());
        assert!(
            !grant.capabilities.grants_ask_human(),
            "unattended goals must not receive AskHuman"
        );
        assert!(
            grant
                .capabilities
                .contains(&Capability::Write(Zone::vault("tasks"))),
            "non-AskHuman capabilities must survive"
        );
    }

    #[test]
    fn interactive_true_keeps_ask_human() {
        let grant =
            apply_interactive_to_grant(&goal_with_interactive(Some(true)), grant_with_ask_human());
        assert!(grant.capabilities.grants_ask_human());
    }

    #[test]
    fn interactive_absent_keeps_profile_grant() {
        let grant =
            apply_interactive_to_grant(&goal_with_interactive(None), grant_with_ask_human());
        assert!(
            grant.capabilities.grants_ask_human(),
            "absent flag must not silently strip AskHuman"
        );
    }

    /// Ignoring the flag reintroduces F11: this test fails if the call site is removed.
    ///
    /// The helper definition, its docs, or a call from another handler is not enough. The grant
    /// passed to `start_with_grant` must be the value narrowed inside `goals_start`.
    #[test]
    fn apply_interactive_is_invoked_from_goals_start() {
        let src = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/api/goals.rs"));
        let production = src.split("#[cfg(test)]").next().expect("production");
        let goals_start = production
            .split_once("pub async fn goals_start(")
            .and_then(|(_, tail)| tail.split_once("pub async fn goals_get("))
            .map(|(body, _)| body)
            .expect("production source must contain the goals_start body");
        assert!(
            goals_start.contains("let grant = apply_interactive_to_grant(&goal, grant);"),
            "goals_start must narrow and rebind the grant before start_with_grant"
        );
    }

    fn life_capabilities() -> liberado_common::CapabilitySet {
        use liberado_common::{Capability, CapabilitySet, Zone};
        let mut capabilities = CapabilitySet::empty();
        capabilities.grant(Capability::AskHuman);
        capabilities.grant(Capability::Write(Zone::vault("tasks")));
        capabilities
    }

    /// A config whose `"life"` component holds the grant an unprofiled life session resolves to â€”
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
        // input (its grant omits AskHuman), which is an authority answer â€” not the timing answer a
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
        // The full S4 return handoff, end to end: POST /api/goals with an origin â†’ interactive
        // session â†’ answer it â†’ on terminal, its summary is appended to the parent conversation.
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

        // The handoff watcher appends the summary into the parent conversation (async â€” poll for it).
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

    // â”€â”€ Forking â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

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
                        model: None,
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
                        model: None,
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
            watcher_active: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            vault_path: "/tmp/vault".to_string(),
            goals: Arc::new(GoalSessionHub::new(GoalSessionStore::new())),
            chat: None,
            chat_tools: 0,
            chat_tool_names: Vec::new(),
            catalog: Arc::new(liberado_common::CapabilityCatalog::new()),
            data_dir: std::path::PathBuf::from("/tmp/liberado"),
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
            live_mcp: liberado_bootstrap::LiveMcpController::empty(),
            drain: crate::shutdown::DrainGate::default(),
        });

        let app = Router::new()
            .route(
                "/api/sessions/{id}/fork",
                axum::routing::post(crate::api::session_fork),
            )
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
        // The original still exists, unchanged, alongside the fork â€” that is the whole request.
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
        // Asking to keep more turns than exist is not a mistake worth refusing â€” it is just "all of
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

#[cfg(test)]
mod project_auth_http_tests {
    //! S3/G4: coding goals refuse undeclared projects/paths at `POST /api/goals` (403).

    use std::sync::Arc;
    use std::time::Instant;

    use axum::{
        Router,
        body::Body,
        http::{Request, StatusCode},
    };
    use liberado_common::{Capability, CapabilitySet, WriteClass, Zone};
    use liberado_config::{Grant, ProjectConfig};
    use liberado_session::{GoalSessionHub, GoalSessionStore, LifeOpsDemoRunner};
    use tower::ServiceExt;

    use crate::api::goals::{goals_start, list_projects};
    use crate::state::AppState;

    fn coding_capabilities() -> CapabilitySet {
        let mut caps = CapabilitySet::empty();
        caps.grant(Capability::AskHuman);
        caps.grant(Capability::Write(Zone::vault("tasks")));
        caps
    }

    fn config_with_project(project: ProjectConfig) -> liberado_bootstrap::Config {
        let mut config = liberado_bootstrap::Config::default();
        config.policy.grants.push(Grant {
            component: "coding".into(),
            capabilities: coding_capabilities().capabilities,
        });
        config.topology.projects.push(project);
        config
    }

    /// A stand-in for the coding pack that only records the goal it was handed.
    ///
    /// Registering the real one would pull the whole `coder-agent` dependency tree into a server
    /// test. What has to be observed here is narrow: the payload the daemon starts the session
    /// with, after authorization has rewritten it.
    struct RecordingCodingPack {
        seen: Arc<std::sync::Mutex<Vec<serde_json::Value>>>,
    }

    #[async_trait::async_trait]
    impl liberado_session::DomainPackRunner for RecordingCodingPack {
        fn domain_id(&self) -> &str {
            liberado_session::CODING_DOMAIN
        }

        async fn run(
            &self,
            _session_id: &str,
            goal: &liberado_session::GoalSpec,
            _ctx: &liberado_session::PackContext<'_>,
            _events: tokio::sync::mpsc::Sender<liberado_session::SessionEvent>,
            _inputs: liberado_session::InputChannel,
            _cancel: tokio::sync::watch::Receiver<bool>,
        ) -> Result<liberado_session::GoalResult, liberado_session::PackError> {
            self.seen.lock().unwrap().push(goal.payload.clone());
            Ok(liberado_session::GoalResult {
                terminal: liberado_session::TerminalKind::Succeeded,
                summary: "recorded".into(),
                artifacts: Vec::new(),
                diagnostics: serde_json::Value::Null,
            })
        }
    }

    /// `coding_goals_app`, plus a coding pack that records what it was started with.
    fn coding_goals_app_recording(
        config: liberado_bootstrap::Config,
    ) -> (Router, Arc<std::sync::Mutex<Vec<serde_json::Value>>>) {
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let app = coding_goals_app_with(
            config,
            Some(Arc::new(RecordingCodingPack { seen: seen.clone() })),
        );
        (app, seen)
    }

    fn coding_goals_app(config: liberado_bootstrap::Config) -> Router {
        coding_goals_app_with(config, None)
    }

    /// The 403 paths never reach a pack, so the life demo alone is enough for them. Anything
    /// asserting on what a *started* coding session received needs a pack answering to "coding".
    fn coding_goals_app_with(
        config: liberado_bootstrap::Config,
        coding_pack: Option<Arc<RecordingCodingPack>>,
    ) -> Router {
        let mut hub = GoalSessionHub::new(GoalSessionStore::new());
        hub.register_pack(Arc::new(LifeOpsDemoRunner));
        if let Some(pack) = coding_pack {
            hub.register_pack(pack);
        }
        let goals = Arc::new(hub);
        let (hook_tx, _hook_rx) = tokio::sync::mpsc::unbounded_channel();
        let state = Arc::new(AppState {
            start_time: Instant::now(),
            reactions: Arc::new(tokio::sync::Mutex::new(Vec::new())),
            dispatcher_attached: false,
            orchestrator_attached: false,
            watcher_active: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            vault_path: "/tmp/vault".to_string(),
            goals,
            chat: None,
            chat_tools: 0,
            chat_tool_names: Vec::new(),
            catalog: Arc::new(liberado_common::CapabilityCatalog::new()),
            data_dir: std::path::PathBuf::from("/tmp/liberado"),
            sessions_root: std::path::PathBuf::from("/tmp/liberado/sessions"),
            main_agent_capabilities: CapabilitySet::empty(),
            dispatcher_capabilities: CapabilitySet::empty(),
            config: Arc::new(config),
            sessions: Arc::new(Default::default()),
            model_name: None,
            provider: None,
            hooks: std::collections::HashMap::new(),
            hook_tx,
            hook_idempotency: crate::hooks::IdempotencyCache::default(),
            live_mcp: liberado_bootstrap::LiveMcpController::empty(),
            drain: crate::shutdown::DrainGate::default(),
        });
        Router::new()
            .route("/api/goals", axum::routing::post(goals_start))
            .route("/api/projects", axum::routing::get(list_projects))
            .with_state(state)
    }

    async fn post_goal(app: &Router, body: serde_json::Value) -> (StatusCode, String) {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/goals")
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        (status, String::from_utf8_lossy(&bytes).into_owned())
    }

    #[tokio::test]
    async fn unknown_project_name_is_403() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let app = coding_goals_app(config_with_project(ProjectConfig {
            name: "liberado".into(),
            root,
            write_class: WriteClass::AgentWritable,
            enabled: true,
            preflight: Default::default(),
        }));
        let (status, body) = post_goal(
            &app,
            serde_json::json!({
                "description": "do a thing",
                "domain": "coding",
                "payload": { "project": "not-a-real-project", "interactive": true }
            }),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
        assert!(
            body.contains("unknown coding project") || body.contains("not-a-real-project"),
            "{body}"
        );
    }

    #[tokio::test]
    async fn undeclared_workspace_path_is_403() {
        let declared = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(declared.path()).unwrap();
        let outside = std::fs::canonicalize(outside.path()).unwrap();
        let app = coding_goals_app(config_with_project(ProjectConfig {
            name: "liberado".into(),
            root,
            write_class: WriteClass::AgentWritable,
            enabled: true,
            preflight: Default::default(),
        }));
        let (status, body) = post_goal(
            &app,
            serde_json::json!({
                "description": "do a thing",
                "domain": "coding",
                "payload": {
                    "workspace_root": outside.to_string_lossy(),
                    "interactive": true
                }
            }),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
        assert!(
            body.contains("not under any declared") || body.contains("fail-closed"),
            "{body}"
        );
    }

    #[tokio::test]
    async fn malformed_coding_payload_is_rejected_before_workspace_authorization() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let app = coding_goals_app(config_with_project(ProjectConfig {
            name: "liberado".into(),
            root,
            write_class: WriteClass::AgentWritable,
            enabled: true,
            preflight: Default::default(),
        }));
        let (status, body) = post_goal(
            &app,
            serde_json::json!({
                "description": "do a thing",
                "domain": "coding",
                "payload": { "workspace_root": 42 }
            }),
        )
        .await;

        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert!(body.contains("invalid coding goal payload"), "{body}");
    }

    #[tokio::test]
    async fn list_projects_returns_enabled_entries() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let app = coding_goals_app(config_with_project(ProjectConfig {
            name: "liberado".into(),
            root: root.clone(),
            write_class: WriteClass::AgentWritable,
            enabled: true,
            preflight: Default::default(),
        }));
        let response = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/api/projects")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let projects = v["projects"].as_array().expect("projects array");
        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0]["name"], "liberado");
        assert_eq!(
            projects[0]["root"].as_str().unwrap(),
            root.to_string_lossy().as_ref()
        );
    }

    #[tokio::test]
    async fn an_authorized_project_name_reaches_the_pack_as_a_resolved_absolute_root() {
        // Naming a project is the entire point: `/goal in liberado` has to arrive at the pack as
        // that repo's path. Assert what the *pack* was started with, not the HTTP status — the
        // status is the same whether the root was injected or dropped, and dropping it does not
        // fail, it silently builds in a temp directory the human never asked for.
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let (app, seen) = coding_goals_app_recording(config_with_project(ProjectConfig {
            name: "liberado".into(),
            root: root.clone(),
            write_class: WriteClass::AgentWritable,
            enabled: true,
            preflight: Default::default(),
        }));

        let (status, body) = post_goal(
            &app,
            serde_json::json!({
                "description": "do a thing",
                "domain": "coding",
                "payload": { "project": "liberado" }
            }),
        )
        .await;
        assert_eq!(status, StatusCode::ACCEPTED, "{body}");

        // The pack runs on the hub's task; wait for it rather than racing it.
        let payload = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                if let Some(p) = seen.lock().unwrap().first().cloned() {
                    return p;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("the coding pack should have been started");

        assert_eq!(
            payload["project"], "liberado",
            "the resolved project name must reach the pack: {payload}"
        );
        let injected = payload["workspace_root"]
            .as_str()
            .unwrap_or_else(|| panic!("no workspace_root reached the pack: {payload}"));
        // Server strips Windows `\\?\` so git/tools see a plain drive path.
        let expected = super::strip_windows_extended_path(&root);
        assert_eq!(
            std::path::Path::new(injected),
            std::path::Path::new(&expected),
            "the pack must receive the project's resolved absolute root"
        );
    }

    #[tokio::test]
    async fn a_client_supplied_workspace_root_is_replaced_by_the_resolved_one() {
        // The payload field is caller-controlled. Authorization has to *overwrite* it, not merely
        // approve it — otherwise a non-canonical spelling of an allowed path is what the pack acts
        // on, and the string that was checked is not the string that is used.
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let sub = root.join("crates");
        std::fs::create_dir_all(&sub).unwrap();
        let (app, seen) = coding_goals_app_recording(config_with_project(ProjectConfig {
            name: "liberado".into(),
            root: root.clone(),
            write_class: WriteClass::AgentWritable,
            enabled: true,
            preflight: Default::default(),
        }));

        // Built as a *string*, not by `PathBuf::join`: pushing `..` onto a verbatim `\?\` path
        // collapses it at construction on Windows, so a joined path would arrive already canonical
        // and the test could not tell the two apart.
        let sep = std::path::MAIN_SEPARATOR;
        let scenic = format!("{}{sep}crates{sep}..{sep}crates", root.display());
        let (status, body) = post_goal(
            &app,
            serde_json::json!({
                "description": "do a thing",
                "domain": "coding",
                "payload": { "workspace_root": scenic }
            }),
        )
        .await;
        assert_eq!(status, StatusCode::ACCEPTED, "{body}");

        let payload = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                if let Some(p) = seen.lock().unwrap().first().cloned() {
                    return p;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("the coding pack should have been started");

        let expected = super::strip_windows_extended_path(&sub);
        assert_eq!(
            std::path::Path::new(payload["workspace_root"].as_str().unwrap()),
            std::path::Path::new(&expected),
            "the pack must get the canonical path, not the caller's spelling: {payload}"
        );
        assert_eq!(payload["project"], "liberado", "{payload}");
    }

    #[tokio::test]
    async fn life_domain_ignores_project_payload() {
        let mut config = liberado_bootstrap::Config::default();
        config.policy.grants.push(Grant {
            component: "life".into(),
            capabilities: coding_capabilities().capabilities,
        });
        let app = coding_goals_app(config);
        let (status, body) = post_goal(
            &app,
            serde_json::json!({
                "description": "capture a note",
                "domain": "life",
                "payload": { "project": "does-not-exist", "interactive": true }
            }),
        )
        .await;
        assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    }
}

#[cfg(test)]
mod rewind_tests {
    use super::*;
    use liberado_session::{SessionEvent, SessionEventKind};

    fn checkpoint(id: &str, label: &str, hash: &str) -> SessionEvent {
        SessionEvent {
            session_id: "s1".into(),
            at: chrono::Utc::now(),
            kind: SessionEventKind::Checkpoint {
                id: id.into(),
                label: label.into(),
                tree_hash: hash.into(),
            },
        }
    }

    fn token_event() -> SessionEvent {
        SessionEvent {
            session_id: "s1".into(),
            at: chrono::Utc::now(),
            kind: SessionEventKind::Token { text: "hi".into() },
        }
    }

    #[test]
    fn rewind_workspace_prefers_an_existing_durable_dir() {
        let durable = tempfile::tempdir().unwrap();
        let got = rewind_workspace(Some(durable.path().to_path_buf()), Some("/payload")).unwrap();
        assert_eq!(got, durable.path());
    }

    #[test]
    fn rewind_workspace_falls_back_to_payload_when_durable_is_missing() {
        let gone = std::path::PathBuf::from("C:\\definitely-not-here-rewind-test");
        let got = rewind_workspace(Some(gone), Some("/payload")).unwrap();
        assert_eq!(got, std::path::PathBuf::from("/payload"));
    }

    #[test]
    fn rewind_workspace_uses_payload_when_no_durable_dir_exists() {
        let got = rewind_workspace(None, Some("/payload")).unwrap();
        assert_eq!(got, std::path::PathBuf::from("/payload"));
    }

    #[test]
    fn rewind_workspace_reports_both_missing_sources() {
        let err = rewind_workspace(None, None).unwrap_err();
        assert!(err.contains("no workspace_root in payload"), "{err}");
        let gone = std::path::PathBuf::from("C:\\definitely-not-here-rewind-test");
        let err = rewind_workspace(Some(gone), None).unwrap_err();
        assert!(err.contains("no durable session worktree"), "{err}");
    }

    #[test]
    fn rewind_checkpoint_explicit_id_wins_with_event_label() {
        let events = vec![
            checkpoint("c1", "first", "h1"),
            token_event(),
            checkpoint("c2", "second", "h2"),
        ];
        let got = rewind_checkpoint(&events, Some("c1")).unwrap();
        assert_eq!(got, ("c1".into(), "first".into(), "h1".into()));
    }

    #[test]
    fn rewind_checkpoint_unknown_explicit_id_falls_back_to_explicit_label() {
        let events = vec![checkpoint("c1", "first", "h1")];
        let got = rewind_checkpoint(&events, Some("nope")).unwrap();
        assert_eq!(got, ("nope".into(), "explicit".into(), String::new()));
    }

    #[test]
    fn rewind_checkpoint_no_id_uses_the_most_recent_checkpoint() {
        let events = vec![
            checkpoint("c1", "first", "h1"),
            token_event(),
            checkpoint("c2", "second", "h2"),
        ];
        let got = rewind_checkpoint(&events, None).unwrap();
        assert_eq!(got, ("c2".into(), "second".into(), "h2".into()));
    }

    #[test]
    fn rewind_checkpoint_no_checkpoints_errors() {
        let err = rewind_checkpoint(&[token_event()], None).unwrap_err();
        assert!(err.contains("no checkpoint events"), "{err}");
    }
}

#[cfg(test)]
mod sse_tests {
    use super::*;
    use liberado_session::{SessionEvent, SessionEventKind};

    fn event(kind: SessionEventKind) -> axum::response::sse::Event {
        session_event_to_sse(&SessionEvent {
            session_id: "s1".into(),
            at: chrono::Utc::now(),
            kind,
        })
    }

    #[test]
    fn session_event_to_sse_maps_every_kind_to_its_name() {
        let cases: Vec<(SessionEventKind, &str)> = vec![
            (
                SessionEventKind::SessionStarted {
                    domain: "coding".into(),
                    description: "d".into(),
                },
                "session_started",
            ),
            (
                SessionEventKind::RoleStarted {
                    role: "r".into(),
                    model: "m".into(),
                },
                "role_started",
            ),
            (
                SessionEventKind::RoleFinished { role: "r".into() },
                "role_finished",
            ),
            (
                SessionEventKind::ToolStarted {
                    name: "t".into(),
                    args_preview: "a".into(),
                },
                "tool_started",
            ),
            (
                SessionEventKind::ToolFinished {
                    name: "t".into(),
                    ok: true,
                    result_preview: "r".into(),
                },
                "tool_finished",
            ),
            (
                SessionEventKind::Progress {
                    message: "p".into(),
                },
                "progress",
            ),
            (
                SessionEventKind::AwaitingInput {
                    prompt: "p".into(),
                    options: Vec::new(),
                },
                "awaiting_input",
            ),
            (
                SessionEventKind::HumanInput { text: "h".into() },
                "human_input",
            ),
            (
                SessionEventKind::ValidationFinished {
                    ok: true,
                    summary: "s".into(),
                },
                "validation_finished",
            ),
            (
                SessionEventKind::CriticVerdict {
                    reviewer: "rv".into(),
                    kind: "k".into(),
                    approved: true,
                    issues: Vec::new(),
                    coerced: false,
                },
                "critic_verdict",
            ),
            (
                SessionEventKind::FileChanged {
                    path: "f".into(),
                    change: "c".into(),
                },
                "file_changed",
            ),
            (
                SessionEventKind::Checkpoint {
                    id: "c1".into(),
                    label: "l".into(),
                    tree_hash: "h".into(),
                },
                "checkpoint",
            ),
            (
                SessionEventKind::LoopGuard {
                    guard: "g".into(),
                    action: "a".into(),
                },
                "loop_guard",
            ),
            (
                SessionEventKind::SessionFinished {
                    status: "ok".into(),
                    summary: "s".into(),
                },
                "session_finished",
            ),
            (
                SessionEventKind::Failed {
                    message: "m".into(),
                },
                "failed",
            ),
        ];
        for (kind, expected) in cases {
            let ev = event(kind);
            let dbg = format!("{ev:?}");
            assert!(dbg.contains(&format!("event: {expected}\\n")), "{dbg}");
        }
    }

    #[test]
    fn session_event_to_sse_token_is_a_bare_text_frame() {
        let ev = event(SessionEventKind::Token {
            text: "hello".into(),
        });
        let dbg = format!("{ev:?}");
        assert!(dbg.contains("event: token\\n"), "{dbg}");
        assert!(dbg.contains("data: hello"), "{dbg}");
    }

    #[test]
    fn session_event_to_sse_carries_the_full_event_json() {
        let ev = event(SessionEventKind::RoleStarted {
            role: "orchestrator".into(),
            model: "m".into(),
        });
        let dbg = format!("{ev:?}");
        assert!(dbg.contains("orchestrator"), "{dbg}");
        assert!(dbg.contains(r#"\"type\":\"role_started\""#), "{dbg}");
    }
}

#[cfg(test)]
mod pure_helper_tests {
    use super::*;

    #[test]
    fn windows_extended_prefixes_are_stripped_for_the_wire() {
        use std::path::Path;
        assert_eq!(
            strip_windows_extended_path(Path::new(r"\\?\C:\repos\liberado")),
            r"C:\repos\liberado"
        );
        assert_eq!(
            strip_windows_extended_path(Path::new(r"\\?\UNC\server\share\repo")),
            r"\\server\share\repo"
        );
        assert_eq!(
            strip_windows_extended_path(Path::new("/home/user/repo")),
            "/home/user/repo",
            "plain paths pass through untouched"
        );
    }

    /// A diff between the old 2 KiB floor and the real cap must round-trip: a cap that
    /// silently shrank would truncate readable diffs while every huge-fixture test still
    /// passed.
    #[test]
    fn a_mid_sized_diff_is_not_truncated() {
        // An independent size, not derived from the cap: a cap that silently shrank to a few
        // kilobytes must show up here, not hide behind the shared constant.
        let mid = "x".repeat(3_000);
        assert_eq!(bound_diff(mid.clone()), mid);
    }

    /// The cut lands on a char boundary and keeps an exact, non-empty prefix of the input.
    #[test]
    fn truncation_lands_on_a_char_boundary_keeping_exact_content() {
        // Byte MAX_DIFF_BYTES falls inside the first three-byte character.
        let mut diff = "a".repeat(MAX_DIFF_BYTES - 1);
        diff.push_str("漢漢漢");
        diff.push_str(&"b".repeat(1_000));

        let bounded = bound_diff(diff.clone());
        assert!(bounded.contains("[diff truncated"), "{bounded}");
        let head = bounded.split("\n\n[").next().unwrap();
        assert!(!head.is_empty(), "some content is kept");
        assert!(
            head.len() <= MAX_DIFF_BYTES,
            "the kept prefix stays within the cap: {}",
            head.len()
        );
        assert!(
            diff.starts_with(head),
            "kept bytes are an exact prefix of the input"
        );
    }
}
