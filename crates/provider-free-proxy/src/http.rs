//! axum wiring: three routes, no more.
//!
//! - `GET  /healthz` — liveness for process supervision.
//! - `GET  /v1/models` — **only** free models, ranked best-coding-first, in OpenAI's `{"data":
//!   [{"id": …}]}` shape so every existing picker (`parse_models_response`, TUI model browser,
//!   ACP catalog) displays the ranking as its natural order.
//! - `POST /v1/chat/completions` — the proxy path: resolve candidates, rewrite `model`, forward,
//!   fail over down the ranking on candidate-shaped refusals, relay everything else verbatim.
//!
//! Responses are relayed as raw bytes (streaming included) rather than re-serialized: this proxy
//! must stay transparent to SSE chunk boundaries and unknown upstream fields alike.

use std::sync::Arc;

use axum::extract::State;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{Value, json};

use crate::service::{AttemptOutcome, ProxyService, RouteError};

pub fn router(state: Arc<ProxyService>) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/v1/models", get(list_models))
        .route("/v1/chat/completions", post(chat_completions))
        .with_state(state)
}

async fn healthz() -> &'static str {
    "ok"
}

/// The ranked free catalog. A resolver error means we have never seen a usable free set —
/// report 503 so pickers show an empty list and logs carry the reason.
async fn list_models(State(state): State<Arc<ProxyService>>) -> Response {
    match state.resolver.current().await {
        Ok(resolution) => {
            let data: Vec<Value> = resolution
                .ranked
                .iter()
                .map(|m| json!({ "id": m.id, "object": "model", "owned_by": "openrouter-free" }))
                .collect();
            (
                StatusCode::OK,
                Json(json!({ "object": "list", "data": data })),
            )
                .into_response()
        }
        Err(e) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({ "error": { "message": e.to_string() } })),
        )
            .into_response(),
    }
}

/// What to do after one upstream attempt.
enum Step {
    /// Success or a non-failover verdict: send this response and stop.
    Finish(Response),
    /// The candidate itself was refused; walk down the ranking.
    NextCandidate,
}

async fn chat_completions(
    State(state): State<Arc<ProxyService>>,
    Json(mut body): Json<Value>,
) -> Response {
    let candidates = match state.candidates_for(&body).await {
        Ok(c) => c,
        Err(e) => return route_error_response(e),
    };
    if candidates.is_empty() {
        return (
            StatusCode::BAD_GATEWAY,
            Json(json!({ "error": { "message": "no free models available to route to" } })),
        )
            .into_response();
    }

    tracing::info!(candidates = ?candidates, "routing chat completion");
    for slug in candidates {
        ProxyService::rewrite_model(&mut body, &slug);
        if let Step::Finish(response) = attempt_candidate(&state, &body, &slug).await {
            return response;
        }
    }

    exhausted_response(&state)
}

async fn attempt_candidate(state: &ProxyService, body: &Value, slug: &str) -> Step {
    let response = state
        .http
        .post(state.chat_endpoint())
        .bearer_auth(&state.config.upstream_api_key)
        .json(body)
        .send()
        .await;

    let response = match response {
        Ok(r) => r,
        Err(e) => {
            // Our network path failed, not one candidate's quota — retrying other models would
            // not change that, so report rather than churn through the ranking.
            tracing::warn!(candidate = %slug, error = %e, "upstream transport failure");
            return Step::Finish(
                (
                    StatusCode::BAD_GATEWAY,
                    Json(json!({ "error": { "message": format!("upstream unreachable: {e}") } })),
                )
                    .into_response(),
            );
        }
    };

    let status = response.status();
    if !status.is_success() {
        return match classify_failure(state, slug, response).await {
            FailureVerdict::FailOver => Step::NextCandidate,
            FailureVerdict::Relay(response) => Step::Finish(response),
        };
    }

    match state.classify_response(response) {
        AttemptOutcome::Ready(upstream) => Step::Finish(relay(upstream).await),
        AttemptOutcome::Failed(reason) => {
            tracing::warn!(candidate = %slug, %reason, "classified failed after success status");
            Step::NextCandidate
        }
    }
}

/// An unsuccessful upstream reply either names the *candidate* as the problem (fail over to the
/// next ranked model) or it does not (relay verbatim).
enum FailureVerdict {
    FailOver,
    Relay(Response),
}

async fn classify_failure(
    state: &ProxyService,
    slug: &str,
    response: reqwest::Response,
) -> FailureVerdict {
    let status = response.status();
    // Read the error body (bounded) to classify it; a payload problem relays straight back
    // instead of burning another free model's quota.
    let text = response.text().await.unwrap_or_default();
    let clipped: String = text.chars().take(2_000).collect();
    tracing::warn!(
        candidate = %slug,
        status = status.as_u16(),
        body = %clipped,
        "upstream refusal"
    );
    if state.should_fail_over(status.as_u16(), &text) {
        return FailureVerdict::FailOver;
    }
    FailureVerdict::Relay(error_relay(status, &clipped))
}

fn exhausted_response(state: &ProxyService) -> Response {
    (
        StatusCode::BAD_GATEWAY,
        Json(json!({
            "error": { "message": format!(
                "all {} ranked free candidates refused the request",
                state.config.max_attempts) }
        })),
    )
        .into_response()
}

fn route_error_response(err: RouteError) -> Response {
    let status = match &err {
        RouteError::NotFree { .. } => StatusCode::BAD_REQUEST,
        RouteError::Resolve(_) => StatusCode::SERVICE_UNAVAILABLE,
    };
    (
        status,
        Json(json!({ "error": { "message": err.to_string() } })),
    )
        .into_response()
}

/// Relay an upstream success response byte-for-byte, streaming included.
async fn relay(upstream: reqwest::Response) -> Response {
    let content_type = upstream
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("application/json")
        .to_string();

    let status =
        StatusCode::from_u16(upstream.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, content_type)
        .body(axum::body::Body::from_stream(upstream.bytes_stream()))
        .expect("statically valid response builder")
}

fn error_relay(status: reqwest::StatusCode, body_text: &str) -> Response {
    // Upstream client errors are our callers' errors; server errors become 502 with the detail
    // attached, so callers never mistake a provider outage for their own malformed request.
    let status = if status.is_client_error() {
        status
    } else {
        reqwest::StatusCode::BAD_GATEWAY
    };
    let code = StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
    (code, body_text.to_string()).into_response()
}
