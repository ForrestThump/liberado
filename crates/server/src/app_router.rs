//! HTTP router bands: public sign-in, authenticated API, owner-only authority routes.

use std::sync::Arc;

use axum::Router;
use tower_http::compression::CompressionLayer;
use tower_http::cors::CorsLayer;
use tower_http::services::ServeDir;

use crate::api;
use crate::auth;
use crate::dist_dir;
use crate::hooks;
use crate::shutdown;
use crate::state::AppState;

pub(crate) fn owner_only_routes(state: &Arc<AppState>) -> Router {
    Router::new()
        // Human-only. POST so a web-fetching MCP (GET only) cannot decide; owner-only so the
        // shell a coding run opens cannot decide either, which POST never stopped.
        .route(
            "/api/approvals/{id}/resolve",
            axum::routing::post(api::resolve_approval),
        )
        // Surface-only authority change. Deliberately POST, and deliberately absent from every
        // tool catalog — see the handler's docs for why both halves matter.
        .route(
            "/api/conversations/{id}/profile",
            axum::routing::post(api::set_conversation_profile),
        )
        .route(
            "/api/profiles/{name}",
            axum::routing::patch(api::rename_profile).delete(api::delete_profile),
        )
        .route(
            "/api/plugins/{id}",
            axum::routing::put(api::set_plugin_enabled),
        )
        .route("/api/models/select", axum::routing::post(api::select_model))
        .route(
            "/api/mcp/reload",
            axum::routing::post(api::reload_mcp_peers),
        )
        .route_layer(axum::middleware::from_fn(auth::require_owner))
        .with_state(state.clone())
}

/// Routes that need no credential, and why each one is safe without.
///
/// `GET /api/health` says only that the process is up. The three `auth` routes are how a
/// credential is obtained in the first place. `POST /api/hooks/{name}` carries its own
/// per-hook shared secret (`X-Liberado-Hook-Secret`), which is the whole point of a webhook:
/// it is called by software that will never hold an API token.
pub(crate) fn unauthenticated_routes(state: &Arc<AppState>, guard: &Arc<auth::ApiGuard>) -> Router {
    let hooks = Router::new()
        .route(
            "/api/hooks/{name}",
            axum::routing::post(hooks::trigger_hook),
        )
        .with_state(state.clone());
    let sign_in = Router::new()
        .route("/api/health", axum::routing::get(api::health))
        .route("/api/auth/login", axum::routing::post(api::auth_login))
        .route("/api/auth/logout", axum::routing::post(api::auth_logout))
        .route("/api/auth/session", axum::routing::get(api::auth_session))
        .with_state(guard.clone());
    hooks.merge(sign_in)
}

/// The full HTTP/SSE API router plus the static frontend fallback. Work-starting routes are
/// grouped behind the drain middleware (it refuses with `shutting_down` once drain begins);
/// attach/cancel/park/list stay on the main router so clients can rejoin or stop work already in
/// flight. `POST /api/goals` is gated for the same reason as chat — the gate is the capability,
/// not the surface that happened to be wired first.
///
/// Three bands, outermost first: everything under `/api` except [`unauthenticated_routes`]
/// needs a credential; [`owner_only_routes`] additionally needs the owner's. The credential
/// layer is a `route_layer`, so it covers matched API routes and not the static fallback —
/// the sign-in page has to load before anyone can sign in.
pub(crate) fn build_app_router(state: &Arc<AppState>, guard: &Arc<auth::ApiGuard>) -> Router {
    let work_start_routes = Router::new()
        .route("/api/chat", axum::routing::post(api::chat))
        .route(
            "/api/chat/stream",
            axum::routing::get(api::chat_stream_get).post(api::chat_stream_post),
        )
        .route("/api/goals", axum::routing::post(api::goals_start))
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            shutdown::refuse_new_turns_if_draining,
        ))
        .with_state(state.clone());

    let authenticated = Router::new()
        .merge(work_start_routes)
        .route("/api/status", axum::routing::get(api::status))
        .route("/api/models", axum::routing::get(api::models))
        .route("/api/catalog", axum::routing::get(api::catalog))
        .route("/api/reactions", axum::routing::get(api::reactions))
        .route("/api/vault", axum::routing::get(api::vault))
        .route("/api/library", axum::routing::get(api::list_library))
        .route(
            "/api/conversations",
            axum::routing::get(api::list_conversations).post(api::create_conversation),
        )
        .route(
            "/api/conversations/search",
            axum::routing::get(api::search_conversations),
        )
        .route(
            "/api/conversations/{id}",
            axum::routing::get(api::get_conversation)
                .patch(api::patch_conversation_title)
                .delete(api::delete_conversation),
        )
        // Read-only. The operator dogfood MCP lists these rows. It does not decide them.
        .route(
            "/api/approvals/pending",
            axum::routing::get(api::list_pending_approvals),
        )
        .route(
            "/api/conversations/{id}/approvals",
            axum::routing::get(api::list_approvals),
        )
        // Read-only. The grant is the current session header, not a fresh policy resolve.
        .route(
            "/api/conversations/{id}/grant",
            axum::routing::get(api::get_header_grant),
        )
        // Read-only. The next turn's tool, MCP, and zone grants. No revoke on this route.
        .route(
            "/api/conversations/{id}/grants",
            axum::routing::get(api::conversation_grants),
        )
        // Rejoin a turn after a reload, and stop one on purpose. Both exist because a turn no
        // longer belongs to the connection that started it. Not gated by drain.
        .route(
            "/api/conversations/{id}/attach",
            axum::routing::get(api::attach_conversation),
        )
        .route(
            "/api/conversations/{id}/cancel",
            axum::routing::post(api::cancel_conversation_turn),
        )
        .route("/api/profiles", axum::routing::get(api::list_profiles))
        .route("/api/hooks", axum::routing::get(api::list_hooks))
        .route("/api/schedules", axum::routing::get(api::list_schedules))
        // The one unified list (S5′): chats and goal sessions, same rows.
        .route("/api/sessions", axum::routing::get(api::sessions_list))
        // Branch a conversation, keeping the original (copy semantics — a snapshot, not a pointer).
        .route(
            "/api/sessions/{id}/fork",
            axum::routing::post(api::session_fork),
        )
        .route("/api/goals/domains", axum::routing::get(api::goals_domains))
        // Coding project picker (S3/G4) — read-only; start still drain-gated.
        .route("/api/projects", axum::routing::get(api::list_projects))
        // List only — start is on `work_start_routes` (drain-gated).
        .route("/api/goals", axum::routing::get(api::goals_list))
        .route("/api/goals/{id}", axum::routing::get(api::goals_get))
        .route("/api/goals/{id}/diff", axum::routing::get(api::goals_diff))
        .route(
            "/api/goals/{id}/stream",
            axum::routing::get(api::goals_stream),
        )
        .route(
            "/api/goals/{id}/cancel",
            axum::routing::post(api::goals_cancel),
        )
        .route(
            "/api/goals/{id}/message",
            axum::routing::post(api::goals_message),
        )
        .route("/api/goals/{id}/park", axum::routing::post(api::goals_park))
        .route(
            "/api/goals/{id}/rewind",
            axum::routing::post(api::goals_rewind),
        )
        .with_state(state.clone());

    let api_surface = authenticated
        .merge(owner_only_routes(state))
        .route_layer(axum::middleware::from_fn_with_state(
            guard.clone(),
            auth::require_credential,
        ))
        .merge(unauthenticated_routes(state, guard));

    let app = match cors_layer(guard) {
        Some(cors) => api_surface.layer(cors),
        // No CORS response headers at all, which is what makes a cross-origin `fetch` fail.
        // `CorsLayer::permissive()` stood here and answered every origin with
        // `Access-Control-Allow-Origin: *`, so any page the browser loaded could read the
        // control plane.
        None => api_surface,
    };

    app
        // Compression is scoped to the static fallback, deliberately not applied to the router as
        // a whole. The payload that needs it is the release .wasm (multi-MB, ~4x compressible, and
        // the whole page blocks on it over the tailnet); the payload that must never be buffered
        // is `/api/chat/stream`, where holding bytes back turns a live turn into a frozen UI.
        // Scoping it here makes that impossible by construction rather than by trusting a
        // predicate.
        .fallback_service(
            tower::ServiceBuilder::new()
                .layer(CompressionLayer::new())
                .service(ServeDir::new(dist_dir())),
        )
}

/// A CORS layer for the exact origins the operator listed, or `None` when the list is empty.
///
/// Empty is the default and the normal deployment: the WebUI is served by this same daemon on
/// this same origin, so it needs no CORS at all. The list exists for the `dx serve` dev loop,
/// where the UI is on `:8080` and the API on `:4201`. Every entry is matched exactly — there is
/// no wildcard, and `ApiConfig::problems` refuses `*` before the daemon starts.
pub(crate) fn cors_layer(guard: &Arc<auth::ApiGuard>) -> Option<CorsLayer> {
    let origins: Vec<axum::http::HeaderValue> = guard
        .allowed_origins
        .iter()
        .filter_map(|o| o.parse().ok())
        .collect();
    if origins.is_empty() {
        return None;
    }
    Some(
        CorsLayer::new()
            .allow_origin(origins)
            .allow_credentials(true)
            .allow_headers([
                axum::http::header::AUTHORIZATION,
                axum::http::header::CONTENT_TYPE,
                axum::http::HeaderName::from_static(auth::sessions::CSRF_HEADER),
            ])
            .allow_methods([
                axum::http::Method::GET,
                axum::http::Method::POST,
                axum::http::Method::PUT,
                axum::http::Method::PATCH,
                axum::http::Method::DELETE,
            ]),
    )
}
