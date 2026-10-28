//! Map the chat supervisor's peer reports onto `GET /api/status`.

use std::time::Duration;

use chat_client_contract::McpPeerStatus;
use liberado_mcp::{McpLinkState, McpPeerReport};

use crate::state::AppState;

pub(crate) fn from_state(state: &AppState) -> Vec<McpPeerStatus> {
    let mut rows = Vec::new();
    for report in state.live_mcp.registry().peer_reports() {
        rows.push(to_status(report));
    }
    rows
}

fn to_status(report: McpPeerReport) -> McpPeerStatus {
    McpPeerStatus {
        name: report.name,
        state: state_name(report.state).to_string(),
        last_error: report.last_error,
        attempts: report.attempts,
        next_retry_ms: retry_ms(report.next_retry),
        tools: tool_count(report.tools),
    }
}

fn state_name(state: McpLinkState) -> &'static str {
    match state {
        McpLinkState::Connected => "connected",
        McpLinkState::Degraded => "degraded",
    }
}

fn retry_ms(wait: Option<Duration>) -> Option<u64> {
    wait.map(|delay| u64::try_from(delay.as_millis()).unwrap_or(u64::MAX))
}

fn tool_count(tools: usize) -> u32 {
    u32::try_from(tools).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    use async_trait::async_trait;
    use axum::body::Body;
    use axum::http::Request;
    use liberado_common::WriteProvenance;
    use liberado_executor::{RuntimeSetupError, ToolRuntime};
    use liberado_mcp::{
        LiveRegistryRuntime, McpConnector, McpRegistry, RebindableRuntime, ReconnectPolicy,
    };
    use liberado_provider::{ToolDef, ToolInvocation};
    use serde_json::json;
    use tower::ServiceExt;

    use super::*;
    use crate::api::status;

    struct DownFlag(Arc<AtomicBool>);

    struct FlaggedRuntime {
        down: Arc<AtomicBool>,
    }

    #[async_trait]
    impl McpConnector for DownFlag {
        async fn connect(
            &self,
            _provenance: WriteProvenance,
        ) -> Result<Box<dyn RebindableRuntime>, RuntimeSetupError> {
            Ok(Box::new(FlaggedRuntime {
                down: Arc::clone(&self.0),
            }))
        }
    }

    #[async_trait]
    impl ToolRuntime for FlaggedRuntime {
        fn catalog(&self) -> Vec<ToolDef> {
            vec![ToolDef::new("add", "add", json!({}))]
        }

        async fn invoke(&self, _call: &ToolInvocation) -> Result<String, String> {
            if self.down.load(Ordering::SeqCst) {
                Err("peer down".into())
            } else {
                Ok("added".into())
            }
        }
    }

    #[async_trait]
    impl RebindableRuntime for FlaggedRuntime {
        fn rebind_provenance(&mut self, _provenance: WriteProvenance) {}

        fn connection_is_dead(&self) -> bool {
            self.down.load(Ordering::SeqCst)
        }

        async fn probe(&self) -> Result<(), String> {
            if self.down.load(Ordering::SeqCst) {
                Err("peer down".into())
            } else {
                Ok(())
            }
        }
    }

    async fn spin_until(mut ready: impl FnMut() -> bool) {
        for _ in 0..400 {
            if ready() {
                return;
            }
            tokio::task::yield_now().await;
        }
        panic!("timed out waiting for MCP status");
    }

    async fn body_of(state: Arc<AppState>) -> chat_client_contract::DaemonStatus {
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
        serde_json::from_slice(&bytes).unwrap()
    }

    #[tokio::test(start_paused = true)]
    async fn status_reports_degraded_then_connected() {
        let down = Arc::new(AtomicBool::new(false));
        let registry = McpRegistry::new().register("turbovault", DownFlag(Arc::clone(&down)));
        let live = LiveRegistryRuntime::with_policy(
            registry.clone(),
            WriteProvenance::agent("test", "c"),
            ReconnectPolicy::exact(
                Duration::from_secs(1),
                Duration::from_secs(60),
                Duration::from_secs(3600),
            ),
        );
        spin_until(|| live.catalog().len() == 1).await;

        let root = tempfile::tempdir().unwrap();
        let sessions = Arc::new(liberado_session_store::SessionStore::open(root.path()).await);
        let mut app = crate::state::AppState::for_test(sessions, None, root.path().to_path_buf());
        app.live_mcp = liberado_bootstrap::LiveMcpController::new(
            Arc::new(liberado_common::CapabilityCatalog::new()),
            registry.clone(),
        );
        let state = Arc::new(app);

        let connected = body_of(Arc::clone(&state)).await;
        assert_eq!(connected.mcp.len(), 1);
        assert_eq!(connected.mcp[0].name, "turbovault");
        assert_eq!(connected.mcp[0].state, "connected");
        assert!(connected.mcp[0].last_error.is_none());
        assert_eq!(connected.mcp[0].attempts, 0);
        assert!(connected.mcp[0].next_retry_ms.is_none());
        assert_eq!(connected.mcp[0].tools, 1);

        down.store(true, Ordering::SeqCst);
        let err = live
            .invoke(&ToolInvocation::new("c", "turbovault:add", json!({})))
            .await
            .unwrap_err();
        assert!(err.contains("unavailable"), "{err}");
        spin_until(|| live.catalog().is_empty()).await;

        let degraded = body_of(Arc::clone(&state)).await;
        assert_eq!(degraded.mcp[0].state, "degraded");
        assert!(degraded.mcp[0].last_error.is_some());
        assert_eq!(degraded.mcp[0].attempts, 1);
        assert_eq!(degraded.mcp[0].next_retry_ms, Some(1_000));
        assert_eq!(degraded.mcp[0].tools, 0);

        down.store(false, Ordering::SeqCst);
        tokio::time::advance(Duration::from_secs(1)).await;
        spin_until(|| live.catalog().len() == 1).await;
        let back = body_of(state).await;
        assert_eq!(back.mcp[0].state, "connected");
        assert_eq!(back.mcp[0].attempts, 0);
        assert!(back.mcp[0].next_retry_ms.is_none());
        assert_eq!(back.mcp[0].tools, 1);
    }
}
