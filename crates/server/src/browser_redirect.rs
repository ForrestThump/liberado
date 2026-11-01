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
