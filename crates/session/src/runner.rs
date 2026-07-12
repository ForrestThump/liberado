//! Domain pack runner port + the inbound human-input channel for interactive sessions.

use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::mpsc::{Receiver, Sender};

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

/// One inbound message from a human into a running interactive session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HumanInput {
    pub text: String,
}

impl HumanInput {
    pub fn new(text: impl Into<String>) -> Self {
        Self { text: text.into() }
    }
}

/// What awaiting the next human input yielded.
#[derive(Debug)]
pub enum InputOutcome {
    /// The human sent input.
    Received(HumanInput),
    /// The idle budget elapsed with no input — the pack should terminate `BudgetExhausted`.
    IdleExpired(Duration),
    /// The input channel closed (the session is tearing down / being cancelled).
    Closed,
}

/// The inbound-human-input side of an interactive goal session, handed to the pack's [`run`].
///
/// Bundles the receiver with the kernel-owned **idle budget** ([`GoalSpec::max_idle_secs`]) so
/// packs await input through one helper instead of each reinventing the timeout. A non-interactive
/// pack simply never calls [`recv`](Self::recv) — dropping the channel is fine.
///
/// [`run`]: DomainPackRunner::run
/// [`GoalSpec::max_idle_secs`]: crate::GoalSpec::max_idle_secs
pub struct InputChannel {
    rx: Receiver<HumanInput>,
    idle_budget: Option<Duration>,
}

impl InputChannel {
    pub fn new(rx: Receiver<HumanInput>, idle_budget: Option<Duration>) -> Self {
        Self { rx, idle_budget }
    }

    /// Await the next human input, the idle budget expiring, or the channel closing.
    ///
    /// Packs typically `select!` this against their cancel receiver. Input arriving while the pack
    /// is busy (not awaiting) is buffered by the channel and delivered at the next call — the
    /// one-writer rule: input never interleaves into an in-flight provider turn.
    pub async fn recv(&mut self) -> InputOutcome {
        match self.idle_budget {
            Some(budget) => match tokio::time::timeout(budget, self.rx.recv()).await {
                Ok(Some(input)) => InputOutcome::Received(input),
                Ok(None) => InputOutcome::Closed,
                Err(_elapsed) => InputOutcome::IdleExpired(budget),
            },
            None => match self.rx.recv().await {
                Some(input) => InputOutcome::Received(input),
                None => InputOutcome::Closed,
            },
        }
    }
}

/// One domain pack's goal-session implementation (coding, life, …).
#[async_trait]
pub trait DomainPackRunner: Send + Sync {
    fn domain_id(&self) -> &str;

    /// Run until terminal result. Emit events on `events`. Interactive packs await human input via
    /// `inputs` (non-interactive packs ignore it). Poll `cancel` and exit with
    /// [`PackError::Cancelled`] when true.
    async fn run(
        &self,
        session_id: &str,
        goal: &GoalSpec,
        events: Sender<SessionEvent>,
        inputs: InputChannel,
        cancel: tokio::sync::watch::Receiver<bool>,
    ) -> Result<GoalResult, PackError>;
}
