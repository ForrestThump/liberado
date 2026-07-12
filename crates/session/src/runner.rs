//! Domain pack runner port.

use async_trait::async_trait;
use tokio::sync::mpsc::Sender;

use crate::event::SessionEvent;
use crate::goal::{GoalResult, GoalSpec};

#[derive(Debug, thiserror::Error)]
pub enum PackError {
    #[error("setup: {0}")]
    Setup(String),
    #[error("cancelled")]
    Cancelled,
    #[error("pack failed: {0}")]
    Failed(String),
}

/// One domain pack's goal-session implementation (coding, life, …).
#[async_trait]
pub trait DomainPackRunner: Send + Sync {
    fn domain_id(&self) -> &str;

    /// Run until terminal result. Emit events on `events`. Poll `cancel` and exit with
    /// [`PackError::Cancelled`] when true.
    async fn run(
        &self,
        session_id: &str,
        goal: &GoalSpec,
        events: Sender<SessionEvent>,
        cancel: tokio::sync::watch::Receiver<bool>,
    ) -> Result<GoalResult, PackError>;
}
