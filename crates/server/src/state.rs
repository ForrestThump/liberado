use std::sync::Arc;
use std::time::Instant;

use liberado_daemon::Reaction;
use liberado_executor::ToolRuntime;
use liberado_main_agent::ChatSessions;
use liberado_provider::{ToolDef, ToolInvocation};
use tokio::sync::{Mutex, mpsc::UnboundedSender};

use liberado_common::CapabilityCatalog;

use chat_client_contract::{ReactionEvent, ReactionOutcome};

pub struct AppState {
    pub start_time: Instant,
    pub reactions: Arc<Mutex<Vec<ReactionEvent>>>,
    pub dispatcher_attached: bool,
    pub orchestrator_attached: bool,
    pub vault_path: String,
    /// Present when `DEEPSEEK_API_KEY` is set — the durable, session-keyed chat agent. All
    /// persistence orchestration lives inside [`ChatSessions`]; the HTTP handlers are thin adapters.
    pub chat: Option<Arc<ChatSessions>>,
    /// Number of connected chat tools (0 when MCP is unconfigured or failed to connect).
    pub chat_tools: usize,
    /// Names of the tools available to chat, for diagnostic visibility.
    pub chat_tool_names: Vec<String>,
    /// Live capability catalog describing every registered MCP server's name, description,
    /// and consequence. Populated at boot from `config.topology.mcps` and updated at runtime
    /// as MCPs come and go. Exposed via `GET /api/catalog`.
    pub catalog: Arc<CapabilityCatalog>,
    /// The active model id, from `Provider::model()` (`None` when no provider is configured).
    /// Display-only for now — there is no runtime model switch; the model is fixed at daemon
    /// startup by config/env (`DEEPSEEK_MODEL`).
    pub model_name: Option<String>,
}

/// A tool runtime with no tools — chat still works (just conversation) when no MCP is configured.
pub struct NoTools;

#[async_trait::async_trait]
impl ToolRuntime for NoTools {
    fn catalog(&self) -> Vec<ToolDef> {
        Vec::new()
    }
    async fn invoke(&self, _call: &ToolInvocation) -> Result<String, String> {
        Err("no tools are configured".into())
    }
}

impl AppState {
    pub fn reaction_tx(&self) -> UnboundedSender<Reaction> {
        let reactions = self.reactions.clone();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Reaction>();

        tokio::spawn(async move {
            while let Some(reaction) = rx.recv().await {
                // Mirror the old `liberado <vault>` stderr line so `liberado serve` still surfaces
                // reactions to operators, not just to the `/api/reactions` buffer below.
                tracing::info!(
                    event_type = %reaction.event.event_type,
                    path = reaction.event.payload.path.as_deref().unwrap_or_default(),
                    correlation_id = %reaction.event.correlation_id,
                    outcome = reaction.outcome.label(),
                    "REACTION"
                );

                // Map the daemon's ReactionOutcome enum variants directly to the wire enum.
                // The granular label() strings ("(observed)", "acted:reported", etc.) are kept
                // in the tracing line above; only the wire outcome is the 3-variant enum.
                let wire_outcome = match &reaction.outcome {
                    liberado_daemon::ReactionOutcome::Observed => ReactionOutcome::Observed,
                    liberado_daemon::ReactionOutcome::Decided(_) => ReactionOutcome::Decided,
                    liberado_daemon::ReactionOutcome::Acted(_) => ReactionOutcome::Acted,
                };

                let event = ReactionEvent {
                    event_type: reaction.event.event_type.clone(),
                    timestamp: reaction.event.timestamp.to_rfc3339(),
                    source: reaction.event.source.clone(),
                    correlation_id: reaction.event.correlation_id.clone(),
                    path: reaction.event.payload.path.clone(),
                    outcome: wire_outcome,
                };
                let mut guard = reactions.lock().await;
                guard.push(event);
                if guard.len() > 500 {
                    let excess = guard.len() - 500;
                    guard.drain(..excess);
                }
            }
        });

        tx
    }
}
