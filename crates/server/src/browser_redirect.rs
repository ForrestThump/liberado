//! Redirect browser HTML requests to the configured WebAuthn origin when needed.

use std::sync::Arc;

use axum::{
    extract::{Request, State},
    http::{Method, StatusCode, Uri, header},
    middleware::Next,
    response::{IntoResponse, Redirect, Response},
};

use crate::auth::guard::ApiGuard;
use crate::auth::webauthn_config;

pub async fn redirect_browser_origin(
    State(guard): State<Arc<ApiGuard>>,
    request: Request,
    next: Next,
) -> Response {
    let Some(webauthn) = guard.webauthn.as_ref() else {
        return next.run(request).await;
    };
    if *request.method() != Method::GET {
        return next.run(request).await;
    }
    let path = request.uri().path();
    if path.starts_with("/api/") {
        return next.run(request).await;
    }
    let Some(host) = request
        .headers()
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
    else {
        return next.run(request).await;
    };
    if webauthn_config::is_loopback_host(host) {
        return next.run(request).await;
    }
    let allowed = webauthn
        .origins
        .iter()
        .any(|origin| webauthn_config::authority_from_origin(origin) == host);
    if allowed {
        return next.run(request).await;
    }
    let Some(target_base) = webauthn.origins.first() else {
        return next.run(request).await;
    };
    let mut dest = target_base.clone();
    if !dest.ends_with('/') && path.starts_with('/') {
        dest.push_str(path);
    } else {
        dest.push_str(path.trim_start_matches('/'));
    }
    if let Some(query) = request.uri().query() {
        dest.push('?');
        dest.push_str(query);
    }
    let Ok(uri) = dest.parse::<Uri>() else {
        return next.run(request).await;
    };
    Redirect::permanent(&uri.to_string()).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode, header};
    use axum::middleware::from_fn_with_state;
    use axum::routing::get;
    use axum::Router;
    use liberado_config::{ApiConfig, WebAuthnConfig};
    use tower::ServiceExt;

    use crate::auth::{ApiAuth, ApiGuard};

    fn guard_with_webauthn() -> Arc<ApiGuard> {
        ApiGuard::with_api(
            ApiAuth::from_values(Some("human"), &[]),
            ApiConfig {
                webauthn: Some(WebAuthnConfig {
                    rp_id: "homelab-node-ai.mermaid-halfbeak.ts.net".into(),
                    origins: vec![
                        "https://homelab-node-ai.mermaid-halfbeak.ts.net:8444".into(),
                    ],
                    rp_name: "Liberado".into(),
                }),
                ..ApiConfig::default()
            },
        )
    }

    fn redirect_app(guard: Arc<ApiGuard>) -> Router {
        Router::new()
            .route("/api/status", get(|| async { "ok" }))
            .fallback(get(|| async { "html" }))
            .layer(from_fn_with_state(guard, redirect_browser_origin))
    }

    #[tokio::test]
    async fn html_get_on_wrong_host_redirects() {
        let app = redirect_app(guard_with_webauthn());
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/chat")
                    .header(header::HOST, "wrong.example:8444")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::PERMANENT_REDIRECT);
    }

    #[tokio::test]
    async fn api_get_on_wrong_host_is_not_redirected() {
        let app = redirect_app(guard_with_webauthn());
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/status")
                    .header(header::HOST, "wrong.example:8444")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }
}
