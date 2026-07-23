//! Unified session list and fork endpoints.

use std::sync::Arc;

use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
};
use liberado_conversation_store::Ulid;

use chat_client_contract::ApiError;

use crate::state::AppState;
/// `GET /api/sessions` â€” **every** session, newest first: chats and goal sessions in one list (S5â€²).
///
/// This is the endpoint the unified switcher always wanted. Before convergence a surface had to poll
/// `/api/conversations` *and* `/api/goals`, invent a row type for each, and stitch them together â€”
/// which meant the client re-derived a distinction the model says does not exist. Here the
/// distinction is one field: `goal` is absent on a chat and present on a session that runs to a
/// terminal status.
///
/// The older two endpoints remain: they are the *lenses* (`/api/conversations` for the chat view,
/// `/api/goals` for the kernel view), and things like the chat sidebar legitimately want just one.
pub async fn sessions_list(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    Json(state.sessions.list_sessions().await)
}

/// `POST /api/sessions/{id}/fork` â€” branch a conversation, keeping the original.
///
/// Two things a human wants and could not do: *fork this and keep the original*, and *go back to
/// turn N and take a different path*. Both are the same operation over the message DAG â€” copy the
/// prefix up to a node â€” which is why forking was additive rather than a migration: the store has
/// carried `parent_id` and `leaf_path(conv, Some(node))` from day one, and nothing ever asked it to
/// reconstruct a prefix.
///
/// The fork is a **copy**, so it is a snapshot: continue the original afterwards and the fork does
/// not move (see `SessionStore::fork_session` for why copy and not reference).
///
/// The client names the branch point by **turn**, because that is the thing it can show a human;
/// resolving turn â†’ node is this function's whole job.
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

    // Your turns are the anchors â€” the assistant's replies and the tool traffic between them hang
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
            // Asking to keep every turn there is *is* forking the whole thing â€” not an error.
            (None, total_turns)
        }
        Some(n) => {
            // Keep turn `n` and everything that answered it: branch at the node immediately before
            // turn `n+1` began. That is exactly the context you had when you typed turn n+1 â€”
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
