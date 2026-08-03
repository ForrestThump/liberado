//! Status, models, catalog, reactions, and vault endpoints.

use std::sync::Arc;

use axum::{
    Json,
    extract::{Query, State},
    response::IntoResponse,
};
use serde::Deserialize;

use chat_client_contract::{CatalogResponse, DaemonStatus, McpInfo, VaultInfo};

use crate::state::AppState;
#[derive(Deserialize)]
pub struct ReactionsQuery {
    limit: Option<usize>,
}

/// Active model id: prefer live provider state (hot-swappable) over boot-time snapshot.
fn active_model(state: &AppState) -> Option<String> {
    state
        .provider
        .as_ref()
        .map(|p| p.model())
        .or_else(|| state.model_name.clone())
}

pub async fn status(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let reactions_len = state.reactions.lock().await.len();

    Json(DaemonStatus {
        running: true,
        vault_path: state.vault_path.clone(),
        uptime_seconds: state.start_time.elapsed().as_secs(),
        watcher_active: true,
        dispatcher_attached: state.dispatcher_attached,
        orchestrator_attached: state.orchestrator_attached,
        reactions_seen: reactions_len as u64,
        model_name: active_model(&state),
        // Context occupancy of the newest chat turn, read from the latency journal the daemon
        // already writes. No second counter, and pricing stays read-time and out of this field.
        token_usage_total: liberado_cost::context_tokens_for_data_dir(&state.data_dir),
        context_window: None,
        chat_tools: state.chat_tools,
        chat_tool_names: state.chat_tool_names.clone(),
        // The config enum is the source; the wire carries the one bit it means. Asked of the type
        // that owns it rather than re-derived here, so `[webui] enter_key` cannot come to disagree
        // with what the browser does.
        enter_sends: state.config.topology.webui.enter_sends(),
    })
}

/// `GET /api/models` â€” live model catalog from the provider (`GET /models` upstream) plus the
/// currently configured model. Soft-fails: always 200 with `error` set when the provider list
/// cannot be fetched so the TUI can still show `current`.
pub async fn models(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    use chat_client_contract::ModelsResponse;

    let current = active_model(&state);
    let Some(provider) = state.provider.as_ref() else {
        return Json(ModelsResponse {
            models: Vec::new(),
            current,
            error: Some("no inference provider configured".into()),
        });
    };

    match provider.list_models().await {
        Ok(mut models) => {
            // Ensure the active model appears even if the catalog omitted it.
            if let Some(cur) = current.as_ref()
                && !models.iter().any(|m| m == cur)
            {
                models.insert(0, cur.clone());
            }
            models.sort();
            models.dedup();
            Json(ModelsResponse {
                models,
                current,
                error: None,
            })
        }
        Err(e) => {
            tracing::warn!(error = %e, "GET /api/models: provider list_models failed");
            let mut models = Vec::new();
            if let Some(cur) = current.as_ref() {
                models.push(cur.clone());
            }
            Json(ModelsResponse {
                models,
                current,
                error: Some(e.to_string()),
            })
        }
    }
}

/// `POST /api/models/select` — choose the model for subsequent completions, without restarting.
///
/// Body: `{"model":"…"}` swaps the **daemon-wide** default, which is what every surface has always
/// done. `{"model":"…","conversation":"<ulid>"}` instead binds the choice to that one chat: its next
/// turn runs on that model and stamps it onto the log, after which the conversation stays there on
/// its own. Nothing global changes, so other chats are untouched.
///
/// The per-conversation form is deliberately *not* durable at this point. There is nothing to record
/// until a turn happens, and once one does the log carries it — a second stored copy would be a
/// second thing to keep in sync with what actually ran.
pub async fn select_model(
    State(state): State<Arc<AppState>>,
    Json(body): Json<SelectModelRequest>,
) -> impl IntoResponse {
    use chat_client_contract::ModelsResponse;

    let model = body.model.trim().to_string();
    if model.is_empty() {
        return (
            axum::http::StatusCode::BAD_REQUEST,
            Json(ModelsResponse {
                models: Vec::new(),
                current: active_model(&state),
                error: Some("model must be a non-empty string".into()),
            }),
        );
    }

    // Scoped to one conversation: record the pick and return without touching the shared provider.
    if let Some(conversation) = body.conversation.as_deref() {
        let Ok(id) = conversation.parse::<liberado_conversation_store::Ulid>() else {
            return (
                axum::http::StatusCode::BAD_REQUEST,
                Json(ModelsResponse {
                    models: Vec::new(),
                    current: active_model(&state),
                    error: Some(format!("not a conversation id: {conversation}")),
                }),
            );
        };
        let Some(chat) = state.chat.as_ref() else {
            return (
                axum::http::StatusCode::SERVICE_UNAVAILABLE,
                Json(ModelsResponse {
                    models: Vec::new(),
                    current: None,
                    error: Some("chat is disabled".into()),
                }),
            );
        };
        chat.select_model(id, model.clone());
        tracing::info!(conversation = %id, model = %model, "model selected for one conversation");
        return (
            axum::http::StatusCode::OK,
            Json(ModelsResponse {
                models: Vec::new(),
                current: Some(model),
                error: None,
            }),
        );
    }

    let Some(provider) = state.provider.as_ref() else {
        return (
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            Json(ModelsResponse {
                models: Vec::new(),
                current: None,
                error: Some("no inference provider configured".into()),
            }),
        );
    };

    let previous = provider.model();
    provider.set_model(model.clone());
    crate::state::resync_compaction_trigger_for_face_model(&state, provider.model().as_str());
    tracing::info!(%previous, current = %model, "hot-swapped active model");

    (
        axum::http::StatusCode::OK,
        Json(ModelsResponse {
            models: Vec::new(),
            current: Some(provider.model()),
            error: None,
        }),
    )
}

#[derive(Deserialize)]
pub struct SelectModelRequest {
    model: String,
    /// Scope the choice to one conversation. Absent = the daemon-wide default, which is the
    /// historical behaviour and stays the behaviour for callers that do not ask for otherwise.
    #[serde(default)]
    conversation: Option<String>,
}

/// `POST /api/mcp/reload` — re-read topology from the config dir and apply the MCP peer set
/// without restarting the process. Hand-edited `topology.toml` is the operator surface; this is
/// the architectural hot-reload seam (not agent self-registration).
pub async fn reload_mcp_peers(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    match state.live_mcp.reload_from_config_dir() {
        Ok(report) => {
            tracing::info!(
                enabled = ?report.enabled,
                added = ?report.added,
                removed = ?report.removed,
                "POST /api/mcp/reload: MCP peer set applied"
            );
            (
                axum::http::StatusCode::OK,
                Json(serde_json::json!({
                    "ok": true,
                    "enabled": report.enabled,
                    "added": report.added,
                    "removed": report.removed,
                })),
            )
        }
        Err(e) => {
            tracing::warn!(error = %e, "POST /api/mcp/reload rejected — prior peer set kept");
            (
                axum::http::StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "ok": false,
                    "error": e.message,
                })),
            )
        }
    }
}

pub async fn catalog(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let descriptors = state.catalog.descriptors();

    // `chat_tool_names` is the connected runtime's real, flat `<mcp>:<tool>`-prefixed catalog
    // (built once at boot in `build_chat`) â€” group it by server name so each row below gets its
    // actual tool breakdown instead of the tool_count:0/tool_names:[] stub this used to return.
    let mut tools_by_mcp: std::collections::HashMap<&str, Vec<String>> =
        std::collections::HashMap::new();
    for tool_name in &state.chat_tool_names {
        let mcp = liberado_common::mcp_of(tool_name);
        let bare = tool_name
            .strip_prefix(&format!("{mcp}:"))
            .unwrap_or(tool_name);
        tools_by_mcp.entry(mcp).or_default().push(bare.to_string());
    }

    let mcps = descriptors
        .iter()
        .map(|d| {
            // Convert the Consequence enum to its snake_case string representation.
            // We avoid depending on liberado-common in the contract crate, so we serialize
            // through serde_json here on the server side.
            let consequence = serde_json::to_value(d.consequence)
                .ok()
                .and_then(|v| v.as_str().map(String::from))
                .unwrap_or_default();
            let tool_names = tools_by_mcp
                .get(d.name.as_str())
                .cloned()
                .unwrap_or_default();

            McpInfo {
                name: d.name.clone(),
                description: d.description.clone(),
                consequence,
                tool_count: tool_names.len(),
                tool_names,
                provenance: d.provenance.clone(),
                visible_to_main_agent: state.main_agent_capabilities.grants_mcp(&d.name),
                visible_to_dispatcher: state.dispatcher_capabilities.grants_mcp(&d.name),
            }
        })
        .collect();

    Json(CatalogResponse { mcps })
}

pub async fn reactions(
    State(state): State<Arc<AppState>>,
    Query(query): Query<ReactionsQuery>,
) -> impl IntoResponse {
    let limit = query.limit.unwrap_or(20);
    let guard = state.reactions.lock().await;
    let events: Vec<_> = guard.iter().rev().take(limit).cloned().collect();
    Json(events)
}

pub async fn vault(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    Json(VaultInfo {
        root: state.vault_path.clone(),
        note_count: 0,
        watcher_active: true,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    /// `token_usage_total` is **context occupancy of the newest chat turn**, not lifetime spend.
    ///
    /// R2: asserts the parsed `DaemonStatus` field, never a substring of the body.
    /// R3: drives the real axum handler, so the state→cost-crate wiring is covered, not just the
    /// helper. The data dir comes off `AppState`, so nothing here mutates process environment —
    /// `set_var` races every other test in the binary and is `unsafe` for exactly that reason.
    ///
    /// The fixture is chosen so the two candidate answers differ: lifetime sum is 280, context
    /// occupancy is 50. A fixture with one event would pass either implementation.
    #[tokio::test]
    async fn status_token_usage_total_is_context_occupancy_not_lifetime_sum() {
        let dir = tempfile::tempdir().unwrap();
        let latency = dir.path().join("latency");
        std::fs::create_dir_all(&latency).unwrap();
        // Two face turns, then a later subagent call: the subagent is newer but is not the chat's
        // context, so it must not be the number reported.
        std::fs::write(
            latency.join("events.jsonl"),
            r#"{"ts_ms":1,"correlation":"c","role":"face","model":"m","kind":"llm_call","wall_ms":1,"prompt_tokens":200,"completion_tokens":30,"total_tokens":230,"finish":"stop","tool_calls":0,"streamed":false}
{"ts_ms":2,"correlation":"c","role":"face","model":"m","kind":"llm_call","wall_ms":1,"prompt_tokens":40,"completion_tokens":10,"total_tokens":50,"finish":"stop","tool_calls":0,"streamed":false}
{"ts_ms":3,"correlation":"kid","role":"orchestrator","model":"m","kind":"llm_call","wall_ms":1,"prompt_tokens":90000,"completion_tokens":5,"finish":"stop","tool_calls":0,"streamed":false}
"#,
        )
        .unwrap();

        // The two readings of this journal are different numbers; this pins which one ships.
        assert_eq!(
            liberado_cost::token_usage_total_for_data_dir(dir.path()),
            Some(90_285),
            "precondition: the cumulative reading is a very different number"
        );

        let root = dir.path().join("sessions-root");
        std::fs::create_dir_all(&root).unwrap();
        let sessions = Arc::new(liberado_session_store::SessionStore::open(&root).await);
        let mut st = crate::state::AppState::for_test(sessions, None, root.clone());
        st.data_dir = dir.path().to_path_buf();
        let state = Arc::new(st);

        let app = axum::Router::new()
            .route("/api/status", axum::routing::get(status))
            .with_state(state);

        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/status")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert!(resp.status().is_success());
        let bytes = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let body: DaemonStatus = serde_json::from_slice(&bytes).expect("DaemonStatus");
        assert_eq!(
            body.token_usage_total,
            Some(50),
            "must be the newest face turn's prompt+completion (40+10), not the 90285 total \
             and not the subagent's 90005"
        );
    }

    /// A journal with no chat turn in it leaves the field absent rather than reporting 0 — the
    /// status bar renders this as a percentage of the context window, and a fabricated 0 reads as
    /// "empty context" rather than "unknown".
    #[tokio::test]
    async fn status_token_usage_total_absent_when_no_face_turn() {
        let dir = tempfile::tempdir().unwrap();
        let latency = dir.path().join("latency");
        std::fs::create_dir_all(&latency).unwrap();
        std::fs::write(
            latency.join("events.jsonl"),
            r#"{"ts_ms":1,"correlation":"kid","role":"orchestrator","model":"m","kind":"llm_call","wall_ms":1,"prompt_tokens":700,"completion_tokens":5,"finish":"stop","tool_calls":0,"streamed":false}
"#,
        )
        .unwrap();

        let root = dir.path().join("sessions-root");
        std::fs::create_dir_all(&root).unwrap();
        let sessions = Arc::new(liberado_session_store::SessionStore::open(&root).await);
        let mut st = crate::state::AppState::for_test(sessions, None, root.clone());
        st.data_dir = dir.path().to_path_buf();
        let state = Arc::new(st);

        let app = axum::Router::new()
            .route("/api/status", axum::routing::get(status))
            .with_state(state);

        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/status")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let bytes = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let body: DaemonStatus = serde_json::from_slice(&bytes).expect("DaemonStatus");
        assert_eq!(body.token_usage_total, None);
    }
}
