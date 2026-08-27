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
    // Model rewriting indexes the body as an object; anything else (array, string, number)
    // must be refused at the boundary — an `IndexMut` panic here would kill the connection
    // with no HTTP response, which is remote-triggerable.
    if !body.is_object() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": { "message": "request body must be a JSON object" } })),
        )
            .into_response();
    }

    let candidates = match state.candidates_for(&body).await {
        // Invariant: `Ok` never carries an empty list — an empty free set surfaces as
        // `RouteError::Resolve` (`NoFreeModels`) instead, so there is no empty-arm here.
        Ok(c) => c,
        Err(e) => return route_error_response(e),
    };

    tracing::info!(candidates = ?candidates, "routing chat completion");
    let mut attempted = 0usize;
    for slug in candidates {
        attempted += 1;
        ProxyService::rewrite_model(&mut body, &slug);
        if let Step::Finish(response) = attempt_candidate(&state, &body, &slug).await {
            return response;
        }
    }

    exhausted_response(attempted)
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

/// Read an upstream error body bounded — a hostile or misbehaving peer must not choose our
/// memory footprint. Unreadable bodies become empty strings: classification falls back to the
/// status code alone.
async fn failure_body_text(response: reqwest::Response) -> String {
    match crate::bounded::read_capped(response, "upstream error", 64 * 1024).await {
        Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        Err(e) => {
            tracing::warn!(error = %e, "unreadable upstream error body");
            String::new()
        }
    }
}

async fn classify_failure(
    state: &ProxyService,
    slug: &str,
    response: reqwest::Response,
) -> FailureVerdict {
    let status = response.status();
    // Bounded read, then clip again for the log line and relay. A payload problem relays
    // straight back instead of burning another free model's quota.
    let text = failure_body_text(response).await;
    let clipped: String = text.chars().take(2_000).collect();
    let status_code = status.as_u16();
    tracing::warn!(
        candidate = %slug,
        status = status_code,
        body = %clipped,
        "upstream refusal"
    );
    if state.should_fail_over(status.as_u16(), &text) {
        return FailureVerdict::FailOver;
    }
    FailureVerdict::Relay(error_relay(status, &clipped))
}

fn exhausted_response(attempted: usize) -> Response {
    (
        StatusCode::BAD_GATEWAY,
        Json(json!({
            "error": { "message": format!(
                "all {attempted} attempted free candidates refused the request") }
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
