//! `POST /api/hooks/{name}` — the external webhook half of "cron and hooks" (Decision 6/18/19).
//! Arbitrary software that can `curl` an HTTP endpoint (systemd `ExecStart`, CI webhook steps,
//! monitoring alerts, home-automation HTTP actions) triggers a reaction the same way a vault
//! change or a cron firing does — the seam is `Daemon::event_sender()`
//! (`crates/daemon/src/lib.rs`), which hands out a clone of the same channel every other
//! `EventSource` pushes onto, so this handler needs no `EventSource` loop of its own; it's a
//! *push*-style producer where cron and vault-watch are *pull*-style.
//!
//! Auth is a per-hook shared secret (`X-Liberado-Hook-Secret` header) — the user's explicit choice
//! for this to be trivially `curl`-able from anything, not HMAC request signing. Each hook has its
//! own secret (`HookConfig::secret_ref`, resolved from the environment at boot — Decision 10), so a
//! leaked secret only compromises that one hook.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::{
    Json,
    body::Bytes,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
};
use liberado_common::{Event, EventPayload, event_source};
use liberado_config::{HookConfig, Topology};
use serde::Deserialize;
use tokio::sync::Mutex;

use chat_client_contract::ApiError;

use crate::state::AppState;

/// How long a caller-supplied idempotency key is remembered before it's pruned — long enough to
/// absorb a sender's own retry/backoff window (most webhook senders retry within seconds to a few
/// minutes), short enough not to accumulate unboundedly.
const IDEMPOTENCY_TTL: Duration = Duration::from_secs(600);

/// A configured hook, resolved at boot: the actual secret value (not the env var name) and the
/// goal text to dispatch. Only enabled hooks whose `secret_ref` env var is actually set are
/// included — [`liberado_config::Config::validate`] should already guarantee this in production
/// (fail-fast), but a direct/test-constructed `Topology` might skip validation, so a hook with no
/// resolvable secret is silently omitted (logged) rather than panicking at boot.
pub struct ResolvedHook {
    pub secret: String,
    pub goal: String,
    /// Which named dispatcher/executor pool (Decision 18 checkpoint #3) handles this hook's
    /// trigger — `None` routes to the daemon's always-present `"default"` pool.
    pub pool: Option<String>,
    /// Optional session profile name (E7) — carried into the event the same way cron does, so the
    /// reactor can resolve the grant from `[[session_profiles]]` instead of the pool default.
    pub profile: Option<String>,
}

/// Resolve every enabled hook's secret from the environment. Called once at boot.
pub fn resolve_hooks(topology: &Topology) -> HashMap<String, ResolvedHook> {
    topology
        .hooks
        .iter()
        .filter(|h: &&HookConfig| h.enabled)
        .filter_map(|h| match std::env::var(&h.secret_ref) {
            Ok(secret) => Some((
                h.name.clone(),
                ResolvedHook {
                    secret,
                    goal: h.goal.clone(),
                    pool: h.pool.clone(),
                    profile: h.profile.clone(),
                },
            )),
            Err(_) => {
                tracing::warn!(
                    hook = %h.name,
                    secret_ref = %h.secret_ref,
                    "hook secret_ref has no environment variable set — this hook will 404"
                );
                None
            }
        })
        .collect()
}

/// An in-memory, best-effort idempotency cache — a hook redelivering the same
/// `X-Liberado-Idempotency-Key` within [`IDEMPOTENCY_TTL`] is accepted but not re-enqueued. No
/// caller-supplied key means no dedup, the same "no heavy persistence" posture cron's own
/// no-catch-up-on-restart behavior already accepted; this is deliberately not the durable
/// `.liberado/reactions/<id>.json` journal marker the original Decision 6 text sketched — that was
/// never actually built anywhere in this codebase, so an in-memory cache is the honest, consistent
/// alternative, not a regression from a real mechanism.
#[derive(Default)]
pub struct IdempotencyCache(Mutex<HashMap<String, Instant>>);

impl IdempotencyCache {
    /// `true` if `key` was already seen within the TTL (a duplicate — don't re-enqueue); records
    /// `key` as seen either way. Also prunes anything older than the TTL while it holds the lock.
    async fn seen_recently(&self, key: &str) -> bool {
        let now = Instant::now();
        let mut cache = self.0.lock().await;
        cache.retain(|_, seen_at| now.duration_since(*seen_at) < IDEMPOTENCY_TTL);
        let duplicate = cache.contains_key(key);
        cache.insert(key.to_string(), now);
        duplicate
    }
}

/// Constant-time byte comparison — avoids leaking a partial secret match via response timing.
/// Length itself isn't treated as sensitive (only which bytes matched), matching the usual scope
/// of a constant-time compare for this kind of shared-secret check.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

#[derive(Deserialize, Default)]
struct HookRequest {
    /// Optional free-text runtime context appended to the hook's configured goal — e.g. a
    /// monitoring alert's message. The body is entirely optional; a bare `curl -X POST` with no
    /// body at all just fires the hook's goal as configured.
    #[serde(default)]
    context: Option<String>,
}

fn error(status: StatusCode, message: impl Into<String>) -> axum::response::Response {
    (
        status,
        Json(ApiError {
            error: message.into(),
        }),
    )
        .into_response()
}

/// `POST /api/hooks/{name}` — see the module doc comment for the full contract. Always responds
/// immediately; dispatch happens asynchronously off the pushed [`Event`], so a slow dispatch→
/// orchestrate cycle never makes the caller's own request time out.
pub async fn trigger_hook(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    if !state.drain.is_accepting() {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({ "error": crate::shutdown::SHUTTING_DOWN_ERROR })),
        )
            .into_response();
    }

    let Some(hook) = state.hooks.get(&name) else {
        return error(StatusCode::NOT_FOUND, "unknown or disabled hook");
    };

    let provided_secret = headers
        .get("X-Liberado-Hook-Secret")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if !constant_time_eq(provided_secret.as_bytes(), hook.secret.as_bytes()) {
        return error(StatusCode::UNAUTHORIZED, "invalid or missing hook secret");
    }

    let context = if body.is_empty() {
        None
    } else {
        serde_json::from_slice::<HookRequest>(&body)
            .ok()
            .and_then(|r| r.context)
            .filter(|c| !c.is_empty())
    };

    let idempotency_key = headers
        .get("X-Liberado-Idempotency-Key")
        .and_then(|v| v.to_str().ok());
    let correlation_id = match idempotency_key {
        Some(key) => format!("webhook:{name}:{key}"),
        None => format!("webhook:{name}:{}", chrono::Utc::now().to_rfc3339()),
    };

    if idempotency_key.is_some() && state.hook_idempotency.seen_recently(&correlation_id).await {
        // A genuine redelivery of a request we've already accepted — truthfully still "accepted"
        // from the caller's point of view, just not pushed a second time.
        return (
            StatusCode::ACCEPTED,
            Json(serde_json::json!({
                "accepted": true,
                "correlation_id": correlation_id,
                "duplicate": true,
            })),
        )
            .into_response();
    }

    let goal = match context {
        Some(ctx) => format!(
            "{}\n\nAdditional context from the trigger: {ctx}",
            hook.goal
        ),
        None => hook.goal.clone(),
    };

    // Profile rides on `payload.data` exactly as cron does (`liberado-cron::build_event`), so
    // `reaction_goal` can pick it up without a second channel.
    let data = match &hook.profile {
        Some(p) => {
            let mut map = serde_json::Map::new();
            map.insert("profile".into(), serde_json::Value::String(p.clone()));
            serde_json::Value::Object(map)
        }
        None => serde_json::Value::Null,
    };
    let event = Event::trigger(
        "WebhookFired",
        format!("{}:{name}", event_source::WEBHOOK),
        correlation_id.clone(),
        EventPayload {
            summary: Some(goal),
            pool: hook.pool.clone(),
            data,
            ..Default::default()
        },
    );

    if state.hook_tx.send(event).is_err() {
        tracing::error!(hook = %name, "hook event channel closed — is the daemon running?");
        return error(
            StatusCode::SERVICE_UNAVAILABLE,
            "event pipeline unavailable",
        );
    }

    (
        StatusCode::ACCEPTED,
        Json(serde_json::json!({ "accepted": true, "correlation_id": correlation_id })),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constant_time_eq_matches_equal_strings() {
        assert!(constant_time_eq(b"same-secret", b"same-secret"));
    }

    #[test]
    fn constant_time_eq_rejects_different_strings() {
        assert!(!constant_time_eq(b"secret-a", b"secret-b"));
    }

    #[test]
    fn constant_time_eq_rejects_different_lengths() {
        assert!(!constant_time_eq(b"short", b"much-longer-secret"));
    }

    #[test]
    fn resolve_hooks_only_includes_enabled_hooks_with_a_set_env_var() {
        // SAFETY(test): setting a process-wide env var in a single-threaded test context.
        unsafe {
            std::env::set_var("HOOKS_RS_TEST_SECRET", "shh");
        }

        let topology = Topology {
            hooks: vec![
                HookConfig {
                    name: "enabled-with-secret".into(),
                    enabled: true,
                    secret_ref: "HOOKS_RS_TEST_SECRET".into(),
                    goal: "goal a".into(),
                    pool: None,
                    profile: None,
                },
                HookConfig {
                    name: "disabled".into(),
                    enabled: false,
                    secret_ref: "HOOKS_RS_TEST_SECRET".into(),
                    goal: "goal b".into(),
                    pool: None,
                    profile: None,
                },
                HookConfig {
                    name: "enabled-missing-secret".into(),
                    enabled: true,
                    secret_ref: "HOOKS_RS_TEST_DEFINITELY_UNSET".into(),
                    goal: "goal c".into(),
                    pool: None,
                    profile: None,
                },
            ],
            ..Topology::default()
        };

        let resolved = resolve_hooks(&topology);
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved["enabled-with-secret"].secret, "shh");
        assert_eq!(resolved["enabled-with-secret"].goal, "goal a");

        unsafe {
            std::env::remove_var("HOOKS_RS_TEST_SECRET");
        }
    }

    #[tokio::test]
    async fn idempotency_cache_flags_a_repeated_key_but_not_a_fresh_one() {
        let cache = IdempotencyCache::default();
        assert!(
            !cache.seen_recently("key-1").await,
            "first sighting is not a duplicate"
        );
        assert!(
            cache.seen_recently("key-1").await,
            "second sighting is a duplicate"
        );
        assert!(
            !cache.seen_recently("key-2").await,
            "a different key is not a duplicate"
        );
    }

    // ── HTTP-level integration tests ──────────────────────────────────────────

    use axum::Router;
    use axum::body::Body;
    use axum::http::Request;
    use liberado_common::CapabilityCatalog;
    use tokio::sync::mpsc::unbounded_channel;
    use tower::ServiceExt;

    fn test_app(hook_secret: &str) -> (Router, tokio::sync::mpsc::UnboundedReceiver<Event>) {
        let (hook_tx, hook_rx) = unbounded_channel::<Event>();
        let mut hooks = HashMap::new();
        hooks.insert(
            "nightly-backup".to_string(),
            ResolvedHook {
                secret: hook_secret.to_string(),
                goal: "back up the vault".to_string(),
                pool: None,
                profile: None,
            },
        );

        let state = Arc::new(AppState {
            start_time: Instant::now(),
            reactions: Arc::new(Mutex::new(Vec::new())),
            dispatcher_attached: false,
            orchestrator_attached: false,
            vault_path: "/tmp/vault".to_string(),
            goals: Arc::new(liberado_session::GoalSessionHub::new(
                liberado_session::GoalSessionStore::new(),
            )),
            chat: None,
            chat_tools: 0,
            chat_tool_names: Vec::new(),
            catalog: Arc::new(CapabilityCatalog::new()),
            data_dir: std::path::PathBuf::from("/tmp/liberado"),
            sessions_root: std::path::PathBuf::from("/tmp/liberado/sessions"),
            main_agent_capabilities: liberado_common::CapabilitySet::empty(),
            dispatcher_capabilities: liberado_common::CapabilitySet::empty(),
            config: Arc::new(Default::default()),
            sessions: Arc::new(Default::default()),
            model_name: None,
            provider: None,
            hooks,
            hook_tx,
            hook_idempotency: IdempotencyCache::default(),
            live_mcp: liberado_bootstrap::LiveMcpController::empty(),
            drain: crate::shutdown::DrainGate::default(),
        });

        let app = Router::new()
            .route("/api/hooks/{name}", axum::routing::post(trigger_hook))
            .with_state(state);
        (app, hook_rx)
    }

    fn post(uri: &str, secret: Option<&str>, body: &str) -> Request<Body> {
        let mut builder = Request::builder().method("POST").uri(uri);
        if let Some(secret) = secret {
            builder = builder.header("X-Liberado-Hook-Secret", secret);
        }
        builder.body(Body::from(body.to_string())).unwrap()
    }

    #[tokio::test]
    async fn correct_secret_pushes_an_event_and_returns_202() {
        let (app, mut hook_rx) = test_app("shh");
        let response = app
            .oneshot(post("/api/hooks/nightly-backup", Some("shh"), ""))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::ACCEPTED);

        let event = hook_rx
            .try_recv()
            .expect("hook should have pushed an event");
        assert_eq!(event.source, "webhook:nightly-backup");
        assert_eq!(event.payload.summary.as_deref(), Some("back up the vault"));
        assert!(event.provenance.is_none());
    }

    #[tokio::test]
    async fn a_hooks_configured_pool_is_carried_onto_its_event() {
        let (hook_tx, mut hook_rx) = unbounded_channel::<Event>();
        let mut hooks = HashMap::new();
        hooks.insert(
            "nightly-backup".to_string(),
            ResolvedHook {
                secret: "shh".to_string(),
                goal: "back up the vault".to_string(),
                pool: Some("restricted".to_string()),
                profile: None,
            },
        );
        let state = Arc::new(AppState {
            start_time: Instant::now(),
            reactions: Arc::new(Mutex::new(Vec::new())),
            dispatcher_attached: false,
            orchestrator_attached: false,
            vault_path: "/tmp/vault".to_string(),
            goals: Arc::new(liberado_session::GoalSessionHub::new(
                liberado_session::GoalSessionStore::new(),
            )),
            chat: None,
            chat_tools: 0,
            chat_tool_names: Vec::new(),
            catalog: Arc::new(CapabilityCatalog::new()),
            data_dir: std::path::PathBuf::from("/tmp/liberado"),
            sessions_root: std::path::PathBuf::from("/tmp/liberado/sessions"),
            main_agent_capabilities: liberado_common::CapabilitySet::empty(),
            dispatcher_capabilities: liberado_common::CapabilitySet::empty(),
            config: Arc::new(Default::default()),
            sessions: Arc::new(Default::default()),
            model_name: None,
            provider: None,
            hooks,
            hook_tx,
            hook_idempotency: IdempotencyCache::default(),
            live_mcp: liberado_bootstrap::LiveMcpController::empty(),
            drain: crate::shutdown::DrainGate::default(),
        });
        let app = Router::new()
            .route("/api/hooks/{name}", axum::routing::post(trigger_hook))
            .with_state(state);

        let response = app
            .oneshot(post("/api/hooks/nightly-backup", Some("shh"), ""))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::ACCEPTED);

        let event = hook_rx
            .try_recv()
            .expect("hook should have pushed an event");
        assert_eq!(event.payload.pool.as_deref(), Some("restricted"));
    }

    #[tokio::test]
    async fn a_hooks_configured_profile_is_carried_onto_its_event() {
        let (hook_tx, mut hook_rx) = unbounded_channel::<Event>();
        let mut hooks = HashMap::new();
        hooks.insert(
            "conformance".to_string(),
            ResolvedHook {
                secret: "shh".to_string(),
                goal: "write the probe note".to_string(),
                pool: None,
                profile: Some("conformance".to_string()),
            },
        );
        let state = Arc::new(AppState {
            start_time: Instant::now(),
            reactions: Arc::new(Mutex::new(Vec::new())),
            dispatcher_attached: false,
            orchestrator_attached: false,
            vault_path: "/tmp/vault".to_string(),
            goals: Arc::new(liberado_session::GoalSessionHub::new(
                liberado_session::GoalSessionStore::new(),
            )),
            chat: None,
            chat_tools: 0,
            chat_tool_names: Vec::new(),
            catalog: Arc::new(CapabilityCatalog::new()),
            data_dir: std::path::PathBuf::from("/tmp/liberado"),
            sessions_root: std::path::PathBuf::from("/tmp/liberado/sessions"),
            main_agent_capabilities: liberado_common::CapabilitySet::empty(),
            dispatcher_capabilities: liberado_common::CapabilitySet::empty(),
            config: Arc::new(Default::default()),
            sessions: Arc::new(Default::default()),
            model_name: None,
            provider: None,
            hooks,
            hook_tx,
            hook_idempotency: IdempotencyCache::default(),
            live_mcp: liberado_bootstrap::LiveMcpController::empty(),
            drain: crate::shutdown::DrainGate::default(),
        });
        let app = Router::new()
            .route("/api/hooks/{name}", axum::routing::post(trigger_hook))
            .with_state(state);

        let response = app
            .oneshot(post("/api/hooks/conformance", Some("shh"), ""))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::ACCEPTED);

        let event = hook_rx
            .try_recv()
            .expect("hook should have pushed an event");
        assert_eq!(
            event.payload.data.get("profile").and_then(|v| v.as_str()),
            Some("conformance"),
            "profile must ride on payload.data like cron events do"
        );
    }

    #[tokio::test]
    async fn wrong_secret_is_rejected_and_pushes_nothing() {
        let (app, mut hook_rx) = test_app("shh");
        let response = app
            .oneshot(post("/api/hooks/nightly-backup", Some("wrong"), ""))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert!(
            hook_rx.try_recv().is_err(),
            "no event should have been pushed"
        );
    }

    #[tokio::test]
    async fn missing_secret_header_is_rejected() {
        let (app, _hook_rx) = test_app("shh");
        let response = app
            .oneshot(post("/api/hooks/nightly-backup", None, ""))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn unknown_hook_name_is_404() {
        let (app, _hook_rx) = test_app("shh");
        let response = app
            .oneshot(post("/api/hooks/does-not-exist", Some("shh"), ""))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn body_context_is_appended_to_the_configured_goal() {
        let (app, mut hook_rx) = test_app("shh");
        let response = app
            .oneshot(post(
                "/api/hooks/nightly-backup",
                Some("shh"),
                r#"{"context": "disk usage at 95%"}"#,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::ACCEPTED);

        let event = hook_rx.try_recv().unwrap();
        let summary = event.payload.summary.unwrap();
        assert!(summary.contains("back up the vault"));
        assert!(summary.contains("disk usage at 95%"));
    }

    #[tokio::test]
    async fn a_repeated_idempotency_key_is_accepted_but_not_re_pushed() {
        let (app, mut hook_rx) = test_app("shh");

        let req1 = Request::builder()
            .method("POST")
            .uri("/api/hooks/nightly-backup")
            .header("X-Liberado-Hook-Secret", "shh")
            .header("X-Liberado-Idempotency-Key", "retry-1")
            .body(Body::empty())
            .unwrap();
        let response1 = app.clone().oneshot(req1).await.unwrap();
        assert_eq!(response1.status(), StatusCode::ACCEPTED);
        assert!(
            hook_rx.try_recv().is_ok(),
            "first delivery should push an event"
        );

        let req2 = Request::builder()
            .method("POST")
            .uri("/api/hooks/nightly-backup")
            .header("X-Liberado-Hook-Secret", "shh")
            .header("X-Liberado-Idempotency-Key", "retry-1")
            .body(Body::empty())
            .unwrap();
        let response2 = app.oneshot(req2).await.unwrap();
        assert_eq!(
            response2.status(),
            StatusCode::ACCEPTED,
            "a redelivery is still truthfully 'accepted'"
        );
        assert!(
            hook_rx.try_recv().is_err(),
            "a repeated idempotency key must not push a second event"
        );
    }

    #[tokio::test]
    async fn hook_is_refused_with_shutting_down_during_drain() {
        let (hook_tx, _) = unbounded_channel::<Event>();
        let mut hooks = HashMap::new();
        hooks.insert(
            "nightly-backup".to_string(),
            ResolvedHook {
                secret: "shh".to_string(),
                goal: "back up the vault".to_string(),
                pool: None,
                profile: None,
            },
        );
        let state = Arc::new(AppState {
            start_time: Instant::now(),
            reactions: Arc::new(Mutex::new(Vec::new())),
            dispatcher_attached: false,
            orchestrator_attached: false,
            vault_path: "/tmp/vault".to_string(),
            goals: Arc::new(liberado_session::GoalSessionHub::new(
                liberado_session::GoalSessionStore::new(),
            )),
            chat: None,
            chat_tools: 0,
            chat_tool_names: Vec::new(),
            catalog: Arc::new(CapabilityCatalog::new()),
            data_dir: std::path::PathBuf::from("/tmp/liberado"),
            sessions_root: std::path::PathBuf::from("/tmp/liberado/sessions"),
            main_agent_capabilities: liberado_common::CapabilitySet::empty(),
            dispatcher_capabilities: liberado_common::CapabilitySet::empty(),
            config: Arc::new(Default::default()),
            sessions: Arc::new(Default::default()),
            model_name: None,
            provider: None,
            hooks,
            hook_tx,
            hook_idempotency: IdempotencyCache::default(),
            live_mcp: liberado_bootstrap::LiveMcpController::empty(),
            drain: crate::shutdown::DrainGate::default(),
        });
        state.drain.begin_drain();
        assert!(!state.drain.is_accepting());
        let app = Router::new()
            .route("/api/hooks/{name}", axum::routing::post(trigger_hook))
            .with_state(state);

        let response = app
            .oneshot(post("/api/hooks/nightly-backup", Some("shh"), ""))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    }
}
