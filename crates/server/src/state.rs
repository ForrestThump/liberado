use std::sync::Arc;
use std::time::Instant;

use liberado_daemon::Reaction;
use liberado_executor::ToolRuntime;
use liberado_main_agent::ChatSessions;
use liberado_provider::{ToolDef, ToolInvocation};
use tokio::sync::{Mutex, mpsc::UnboundedSender};

pub struct AppState {
    pub start_time: Instant,
    pub reactions: Arc<Mutex<Vec<ReactionEvent>>>,
    pub dispatcher_attached: bool,
    pub orchestrator_attached: bool,
    pub vault_path: String,
    /// Present when `DEEPSEEK_API_KEY` is set — the durable, session-keyed chat agent. All
    /// persistence orchestration lives inside [`ChatSessions`]; the HTTP handlers are thin adapters.
    pub chat: Option<Arc<ChatSessions>>,
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

#[derive(Debug, Clone, serde::Serialize)]
pub struct ReactionEvent {
    pub event_type: String,
    pub timestamp: String,
    pub source: String,
    pub correlation_id: String,
    pub path: Option<String>,
    pub outcome: &'static str,
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
                let event = ReactionEvent {
                    event_type: reaction.event.event_type.clone(),
                    timestamp: reaction.event.timestamp.to_rfc3339(),
                    source: reaction.event.source.clone(),
                    correlation_id: reaction.event.correlation_id.clone(),
                    path: reaction.event.payload.path.clone(),
                    outcome: reaction.outcome.label(),
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
