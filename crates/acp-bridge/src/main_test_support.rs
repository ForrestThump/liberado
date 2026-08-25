//! Shared fixtures for the split main.rs test modules.

#![allow(unused_imports)]
#![allow(dead_code)]

use super::*;
use crate::provider::catalog_model_ids;
use liberado_provider::MockProvider;
use tempfile::TempDir;

/// A Bridge with a scripted provider — enough to drive `handle_request` in tests.
pub(crate) fn test_bridge() -> Arc<Bridge> {
    use liberado_provider::MockProvider;
    test_bridge_with(Arc::new(MockProvider::with_script("mock", [])))
}

pub(crate) fn test_bridge_with(provider: Arc<dyn Provider>) -> Arc<Bridge> {
    Arc::new(Bridge {
        provider,
        backend: "mock".into(),
        catalog: Mutex::new(Vec::new()),
        current_model: Mutex::new("mock-model".into()),
        default_mode: AgentMode::Coding,
        max_turns: 8,
        coder_tuning: liberado_coder_core::CoderTuning::default(),
        config_dir: None,
        local_grant: liberado_common::CapabilitySet::empty(),
        system_prompt: None,
        acp_sessions: Mutex::new(HashMap::new()),
        permissions: Arc::new(permission::PermissionBroker::new()),
    })
}

/// Captures ACP notifications and JSON-RPC responses instead of writing stdout
/// (for MockProvider turns and dispatch-loop tests).
pub(crate) struct CaptureSink {
    pub(crate) lines: std::sync::Mutex<Vec<(String, Value)>>,
}

impl WireSink for CaptureSink {
    fn emit(&self, method: &str, params: Value) -> Result<(), String> {
        self.lines
            .lock()
            .map_err(|e| e.to_string())?
            .push((method.to_string(), params));
        Ok(())
    }

    fn write_rpc_response(
        &self,
        id: Value,
        outcome: Result<Value, JsonRpcErrorBody>,
    ) -> Result<(), String> {
        let body = match outcome {
            Ok(result) => json!({ "id": id, "result": result }),
            Err(error) => {
                json!({ "id": id, "error": { "code": error.code, "message": error.message } })
            }
        };
        self.lines
            .lock()
            .map_err(|e| e.to_string())?
            .push(("response".into(), body));
        Ok(())
    }
}

pub(crate) struct EchoTool;

#[async_trait::async_trait]
impl ToolRuntime for EchoTool {
    fn catalog(&self) -> Vec<liberado_provider::ToolDef> {
        vec![liberado_provider::ToolDef::new(
            "echo",
            "Echo a message",
            json!({
                "type": "object",
                "properties": { "msg": { "type": "string" } },
                "required": ["msg"]
            }),
        )]
    }
    async fn invoke(&self, call: &liberado_provider::ToolInvocation) -> Result<String, String> {
        let msg = call
            .arguments
            .get("msg")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        Ok(format!("echo:{msg}"))
    }
}

// ── session/load tests ───────────────────────────────────────────────
/// Serializes tests in this module that redirect `sessions_dir()` so they do not race with
/// `session_store` tests or each other.
pub(crate) static SESSION_LOAD_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

pub(crate) fn lock_sessions_dir(dir: &TempDir) -> LockedSessionsDir {
    let load_lock = SESSION_LOAD_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    // Field order is the drop order: the directory override resets first (under the dir lock it
    // still holds), then the load lock releases.
    LockedSessionsDir {
        _dir_override: session_store::set_sessions_dir(dir),
        _load_lock: load_lock,
    }
}

/// Both serialization locks for a sessions-dir test, torn down in the safe order on drop.
pub(crate) struct LockedSessionsDir {
    _dir_override: session_store::SessionsDirOverride,
    _load_lock: std::sync::MutexGuard<'static, ()>,
}

// ── Dispatch-loop and CLI survivors (mutation campaign) ────────────────────
/// Serializes tests that touch process-global env vars.
pub(crate) static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// A sink + bridge harness for the spawn path; no stdout is touched.
pub(crate) struct SpawnHarness {
    pub(crate) bridge: Arc<Bridge>,
    pub(crate) sink: Arc<CaptureSink>,
}

impl SpawnHarness {
    pub(crate) fn new() -> Self {
        Self {
            bridge: test_bridge(),
            sink: Arc::new(CaptureSink {
                lines: std::sync::Mutex::new(Vec::new()),
            }),
        }
    }
}

impl CaptureSink {
    pub(crate) fn new_test() -> Self {
        Self {
            lines: std::sync::Mutex::new(Vec::new()),
        }
    }
}

pub(crate) async fn catalog_model_ids_owned(bridge: &Bridge) -> Vec<String> {
    bridge
        .catalog
        .lock()
        .await
        .iter()
        .map(|m| m.model_id.clone())
        .collect()
}

/// A live session with a pending prompt task whose abort we can observe: when the task
/// dies, its receiver drops and the kept sender reports closed.
pub(crate) async fn session_with_pending_prompt(
    bridge: &Bridge,
    sid: &str,
) -> (InFlightPrompt, tokio::sync::oneshot::Sender<()>) {
    let (cancel_tx, _cancel_rx) = watch::channel(false);
    bridge.acp_sessions.lock().await.insert(
        sid.to_string(),
        AcpSession {
            mode: AgentMode::Coding,
            cwd: std::env::current_dir().unwrap_or_else(|_| ".".into()),
            coding: coding_run::CodingSessionState {
                cwd: ".".into(),
                coding_session_id: sid.into(),
                prior_feedback: Vec::new(),
                last_summary: None,
                rounds: 0,
            },
            converse: None,
            face_daemon_session: None,
            cancel_tx,
            cancel_rx: watch::channel(false).1,
        },
    );
    let (liveness_tx, liveness_rx) = tokio::sync::oneshot::channel::<()>();
    let handle = tokio::spawn(async move {
        let _still_alive = liveness_rx.await;
        std::future::pending::<Result<Value, String>>().await
    });
    (
        InFlightPrompt {
            session_id: sid.to_string(),
            request_id: json!(1),
            handle,
        },
        liveness_tx,
    )
}
