//! The control plane: axum routes speaking `liberado_delegate_contract`, one bearer
//! token on every route (plan §5 — LAN-only, no discovery, token auth).
//!
//! Handlers stay thin over [`TaskStore`] and [`runner::execute`]; submit spawns the run
//! only when persistence says it was *not* a duplicate, which is what makes redelivery
//! safe.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::{Request, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use liberado_delegate_contract::{RejectReason, TaskSpec, WorkerEvent, WorkerHealth};

use crate::queue::TaskStore;
use crate::runner::{self, RunContext};

pub struct AppState {
    pub settings: Arc<crate::config::WorkerSettings>,
    pub store: Arc<TaskStore>,
    /// Concurrency ceiling across delegated runs; acquired by the runner.
    pub slots: Arc<tokio::sync::Semaphore>,
    pub run: RunContext,
}

pub fn router(state: Arc<AppState>) -> Router {
    let protected = Router::new()
        .route(liberado_delegate_contract::routes::HEALTH, get(health))
        .route(liberado_delegate_contract::routes::TASKS, post(submit))
        .route("/v1/delegate/tasks/{task_id}", get(get_task))
        .route("/v1/delegate/tasks/{task_id}/events", get(task_events))
        .route("/v1/delegate/tasks/{task_id}/cancel", post(cancel))
        .route_layer(middleware::from_fn_with_state(state.clone(), bearer_auth));
    protected.with_state(state)
}

/// Constant-time byte comparison — avoids leaking a partial secret match via timing
/// (same shape as the daemon's webhook check).
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter()
        .zip(b.iter())
        .fold(0u8, |diff, (x, y)| diff | (x ^ y))
        == 0
}

async fn bearer_auth(
    State(state): State<Arc<AppState>>,
    request: Request<axum::body::Body>,
    next: Next,
) -> Response {
    let presented = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "));
    let authorized = presented
        .map(|token| constant_time_eq(token.as_bytes(), state.settings.token.as_bytes()))
        .unwrap_or(false);
    if !authorized {
        return (StatusCode::UNAUTHORIZED, "unauthorized\n").into_response();
    }
    next.run(request).await
}

async fn health(State(_state): State<Arc<AppState>>) -> Json<WorkerHealth> {
    Json(runner::health_payload())
}

async fn submit(State(state): State<Arc<AppState>>, Json(spec): Json<TaskSpec>) -> Response {
    if let Some(reason) = validate(&spec) {
        return (StatusCode::BAD_REQUEST, Json(RejectReason::new(reason))).into_response();
    }
    match state.store.submit(&spec) {
        Ok(outcome) => {
            if !outcome.duplicate {
                let ctx = state.run.clone();
                tokio::spawn(runner::execute(ctx, spec));
            }
            let code = if outcome.duplicate {
                StatusCode::OK
            } else {
                StatusCode::CREATED
            };
            (code, Json(outcome)).into_response()
        }
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(RejectReason::new(error.to_string())),
        )
            .into_response(),
    }
}

/// The four identity fields must be present; everything else has defaults. Cheap and
/// honest: a malformed task is rejected before it ever reaches the queue.
fn validate(spec: &TaskSpec) -> Option<String> {
    for (field, empty) in [
        ("project", spec.project.trim().is_empty()),
        ("repository", spec.repository.trim().is_empty()),
        ("base_branch", spec.base_branch.trim().is_empty()),
        ("goal", spec.goal.trim().is_empty()),
    ] {
        if empty {
            return Some(format!("'{field}' must not be empty"));
        }
    }
    None
}

async fn get_task(State(state): State<Arc<AppState>>, Path(task_id): Path<String>) -> Response {
    match state.store.get(&task_id) {
        Ok(Some(record)) => (StatusCode::OK, Json(record)).into_response(),
        Ok(None) => not_found(&task_id),
        Err(error) => internal_error(error.to_string()),
    }
}

/// The task's event stream: journal replay first, then live until a terminal event.
///
/// Subscribe *before* reading the journal, then deduplicate by correlation id while
/// splicing — otherwise an event landing in the gap is either lost (subscribe after
/// read) or delivered twice (read after subscribe without the check). At-least-once
/// with stable ids turns both hazards into a client-side skip.
async fn task_events(State(state): State<Arc<AppState>>, Path(task_id): Path<String>) -> Response {
    if state.store.get(&task_id).ok().flatten().is_none() {
        return not_found(&task_id);
    }
    let mut live = state.store.subscribe();
    let replay = state.store.replay(&task_id).unwrap_or_default();
    let seen: std::collections::HashSet<String> = replay
        .iter()
        .map(|event| event.correlation_id.clone())
        .collect();

    let stream = async_stream::stream! {
        for event in &replay {
            yield Ok::<SseEvent, std::convert::Infallible>(worker_event_to_sse(event));
        }
        // A task that already finished has its terminal event in the journal; waiting
        // on the broadcast after it would park the connection forever — the channel
        // carries no history and no close signal.
        if replay.last().is_some_and(WorkerEvent::is_terminal) {
            return;
        }
        loop {
            match live.recv().await {
                Ok(event) => {
                    if seen.contains(&event.correlation_id) {
                        continue;
                    }
                    let terminal = event.is_terminal();
                    yield Ok(worker_event_to_sse(&event));
                    if terminal {
                        break;
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    };

    // Same heartbeat rationale as the daemon's chat stream: a long tool call emits
    // nothing for minutes, and an idle connection is one an intermediary may close.
    Sse::new(Box::pin(stream))
        .keep_alive(KeepAlive::new().interval(std::time::Duration::from_secs(15)))
        .into_response()
}

/// Encode one [`WorkerEvent`] as SSE. The frame's event name is the kind's serde tag;
/// the data is the full event JSON, correlation id included for client-side dedupe.
fn worker_event_to_sse(event: &WorkerEvent) -> SseEvent {
    let name = match event.kind {
        liberado_delegate_contract::EventKind::Question => "question",
        liberado_delegate_contract::EventKind::StatusChanged => "status_changed",
        liberado_delegate_contract::EventKind::PrReady => "pr_ready",
        liberado_delegate_contract::EventKind::Blocked => "blocked",
    };
    let data = serde_json::to_string(event).unwrap_or_else(|_| "{}".into());
    SseEvent::default().event(name).data(data)
}

async fn cancel(State(state): State<Arc<AppState>>, Path(task_id): Path<String>) -> Response {
    match state.store.cancel(&task_id) {
        Ok(record) => (StatusCode::OK, Json(record)).into_response(),
        Err(crate::queue::CancelError::Running(_)) => (
            StatusCode::CONFLICT,
            Json(RejectReason::new(
                "task is running; cooperative cancel lands in D2",
            )),
        )
            .into_response(),
        Err(crate::queue::CancelError::NotFound(_)) => not_found(&task_id),
        Err(error) => internal_error(error.to_string()),
    }
}

fn not_found(task_id: &str) -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(RejectReason::new(format!("no such task: {task_id}"))),
    )
        .into_response()
}

fn internal_error(message: String) -> Response {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(RejectReason::new(message)),
    )
        .into_response()
}

#[cfg(test)]
mod tests;
