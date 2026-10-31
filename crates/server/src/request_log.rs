//! One log line per `/api/*` request — method, path, status, latency (no query/body/headers).

use std::time::Instant;

use axum::http::Uri;
use axum::{extract::Request, middleware::Next, response::Response};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ApiRequestLogLevel {
    Info,
    Debug,
}

pub(crate) fn api_request_log_path(uri: &Uri) -> &str {
    uri.path()
}

pub(crate) fn api_request_log_fields(
    method: &str,
    path: &str,
    status: u16,
    latency_ms: u128,
) -> String {
    format!("method={method} path={path} status={status} latency_ms={latency_ms}")
}

pub(crate) fn api_request_log_line_for_uri(
    method: &str,
    uri: &Uri,
    status: u16,
    latency_ms: u128,
) -> Option<String> {
    let path = api_request_log_path(uri);
    if !path.starts_with("/api/") {
        return None;
    }
    Some(api_request_log_fields(method, path, status, latency_ms))
}

pub(crate) async fn run_api_request_log(
    request: Request,
    next: Next,
) -> (Response, Option<String>) {
    let method = request.method().to_string();
    let uri = request.uri().clone();
    if api_request_log_loggable(&uri) {
        let started = Instant::now();
        let response = next.run(request).await;
        let line = api_request_log_line_for_uri(
            &method,
            &uri,
            response.status().as_u16(),
            started.elapsed().as_millis(),
        );
        return (response, line);
    }
    (next.run(request).await, None)
}

fn api_request_log_loggable(uri: &Uri) -> bool {
    api_request_log_path(uri).starts_with("/api/")
}

pub(crate) async fn log_api_request(request: Request, next: Next) -> Response {
    let uri = request.uri().clone();
    let (response, line) = run_api_request_log(request, next).await;
    if let Some(fields) = line {
        emit_api_request_log_fields_by_path(api_request_log_path(&uri), &fields);
    }
    response
}

fn emit_api_request_log_fields_by_path(path: &str, fields: &str) {
    match api_request_log_level(path) {
        ApiRequestLogLevel::Debug => {
            tracing::debug!(target: "liberado_server::api", "{fields}");
        }
        ApiRequestLogLevel::Info => {
            tracing::info!(target: "liberado_server::api", "{fields}");
        }
    }
}

pub(crate) fn api_request_log_level(path: &str) -> ApiRequestLogLevel {
    if matches!(
        path,
        "/api/health" | "/api/auth/session" | "/api/status" | "/api/chat/stream"
    ) {
        return ApiRequestLogLevel::Debug;
    }
    if path.starts_with("/api/goals/") && path.ends_with("/stream") {
        return ApiRequestLogLevel::Debug;
    }
    if path.starts_with("/api/conversations/") && path.ends_with("/attach") {
        return ApiRequestLogLevel::Debug;
    }
    if path.starts_with("/api/conversations/") && path.ends_with("/approvals") {
        return ApiRequestLogLevel::Debug;
    }
    ApiRequestLogLevel::Info
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use axum::{
        Router,
        body::Body,
        extract::Request,
        http::{Method, StatusCode, Uri},
        routing::post,
    };
    use tower::ServiceExt;

    use super::*;

    #[test]
    fn log_level_health_is_debug_push_status_is_info() {
        assert_eq!(
            api_request_log_level("/api/health"),
            ApiRequestLogLevel::Debug
        );
        assert_eq!(
            api_request_log_level("/api/push/status"),
            ApiRequestLogLevel::Info
        );
    }

    #[test]
    fn polled_and_stream_routes_log_at_debug() {
        assert_eq!(
            api_request_log_level("/api/status"),
            ApiRequestLogLevel::Debug
        );
        assert_eq!(
            api_request_log_level("/api/chat/stream"),
            ApiRequestLogLevel::Debug
        );
        assert_eq!(
            api_request_log_level("/api/conversations/abc/attach"),
            ApiRequestLogLevel::Debug
        );
        assert_eq!(
            api_request_log_level("/api/conversations/abc/approvals"),
            ApiRequestLogLevel::Debug
        );
        assert_eq!(
            api_request_log_level("/api/goals/g1/stream"),
            ApiRequestLogLevel::Debug
        );
    }

    #[test]
    fn log_line_for_uri_omits_query_string() {
        let uri: Uri = "http://127.0.0.1/api/x?token=SECRET".parse().unwrap();
        let line = api_request_log_line_for_uri("POST", &uri, 200, 3).expect("line");
        assert!(line.contains("method=POST"));
        assert!(line.contains("path=/api/x"));
        assert!(!line.contains('?'));
        assert!(!line.contains("SECRET"));
    }

    #[tokio::test]
    async fn middleware_end_to_end_omits_query_and_body() {
        let recorded = Arc::new(Mutex::new(String::new()));
        let captured = recorded.clone();
        let app = Router::new()
            .route("/api/x", post(|| async { "ok" }))
            .layer(axum::middleware::from_fn(
                move |request: Request, next: Next| {
                    let captured = captured.clone();
                    async move {
                        let (response, line) = run_api_request_log(request, next).await;
                        if let Some(line) = line
                            && let Ok(mut slot) = captured.lock()
                        {
                            *slot = line;
                        }
                        response
                    }
                },
            ));
        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/x?token=SECRET")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"secret":true}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let line = recorded.lock().expect("lock").clone();
        assert!(line.contains("path=/api/x"));
        assert!(!line.contains('?'));
        assert!(!line.contains("SECRET"));
    }
}
