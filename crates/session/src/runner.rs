//! Domain pack runner port + the inbound human-input channel for interactive sessions.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use liberado_common::Capability;
use tokio::sync::mpsc::{Receiver, Sender};

use crate::event::SessionEvent;
use crate::goal::{GoalResult, GoalSpec, SessionGrant};
use crate::record_store::{SessionRecordStore, TurnAuthor};

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

/// Everything a pack is given about *how* to run this session, beyond the goal itself (S6).
///
/// Two halves, both resolved by the server from the session's profile before the run starts:
///
/// * `grant` — the session's **authority ceiling**. A pack must check it before doing anything
///   consequential (`ctx.can(&Capability::Write(zone))`). It can never be widened mid-run — see the
///   non-widening invariant in the hub's tests.
///
///   This check is on its **honour**, and that is not a figure of speech. This comment used to say
///   a pack should check `Write(zone)` "exactly as the MCP boundary does" — **the MCP boundary does
///   not do that.** `RiskGatedToolRuntime` checks: is the MCP granted (`ExecuteMcp`), is its
///   consequence too high, is the target zone write-class-restricted, does the call look sweepingly
///   destructive. It never consults `Capability::Write`. So for MCP-mediated work, `ExecuteMcp` is
///   all-or-nothing: a grant of `Read` + `ExecuteMcp("turbovault")` can write the whole vault.
///   Proved live (control F, 2026-07-14): a `Read`-only dispatch profile wrote a note.
///
///   The zone-write-class guard would be the other half of that defence, but it is **inert in
///   practice**: no MCP in `topology.toml` declares zones, so `resolve_zone` returns `None` for
///   every tool and the guard never fires. See `docs/roadmap/one-execution-engine-live-test.md`
///   § "Control F".
/// * `grant.overrides` — the pack's own **opaque** config (role, model, prompt path). The kernel
///   never interprets it; only this pack does.
pub struct PackContext<'a> {
    pub grant: &'a SessionGrant,
    /// Where [`record_turn`](Self::record_turn) writes. The hub supplies this; a pack never sees the
    /// store itself, only the one thing it is allowed to do with it.
    store: Arc<dyn SessionRecordStore>,
    session_id: String,
}

impl<'a> PackContext<'a> {
    pub fn new(
        grant: &'a SessionGrant,
        store: Arc<dyn SessionRecordStore>,
        session_id: impl Into<String>,
    ) -> Self {
        Self {
            grant,
            store,
            session_id: session_id.into(),
        }
    }
}

impl PackContext<'_> {
    /// Record a **turn** — something said — in this session's transcript.
    ///
    /// The distinction a pack has to get right is *observation* versus *conversation*. A tool call is
    /// an observation: emit it on `events` (`SessionEventKind::ToolStarted`). A clarifying question,
    /// the human's answer, a drafted contract — those are **turns**. They are the dialogue, and they
    /// belong in the message DAG, which is what makes them searchable and the session forkable.
    ///
    /// The store parents each turn onto the session's newest node, so a pack's transcript is a
    /// straight line and the pack never tracks a leaf itself.
    pub async fn record_turn(&self, author: TurnAuthor, content: impl Into<String>) {
        self.store
            .append_turn(&self.session_id, author, content.into())
            .await;
    }

    /// Whether this session holds `cap`. The one call a pack should make before a consequential act.
    pub fn can(&self, cap: &Capability) -> bool {
        self.grant.capabilities.contains(cap)
    }

    /// The pack's opaque overrides (an empty table when the profile set none).
    pub fn overrides(&self) -> &serde_json::Value {
        &self.grant.overrides
    }

    /// The profile name this session runs under, if any.
    pub fn profile(&self) -> Option<&str> {
        self.grant.profile.as_deref()
    }
}

/// One domain pack's goal-session implementation (coding, life, …).
#[async_trait]
pub trait DomainPackRunner: Send + Sync {
    fn domain_id(&self) -> &str;

    /// Run until terminal result. Emit events on `events`. Interactive packs await human input via
    /// `inputs` — but only if the session's grant permits [`Capability::AskHuman`]; without it the
    /// hub hands over an already-closed channel, so `inputs.recv()` yields
    /// [`InputOutcome::Closed`] immediately and the pack must proceed without a human. Poll
    /// `cancel` and exit with [`PackError::Cancelled`] when true.
    async fn run(
        &self,
        session_id: &str,
        goal: &GoalSpec,
        ctx: &PackContext<'_>,
        events: Sender<SessionEvent>,
        inputs: InputChannel,
        cancel: tokio::sync::watch::Receiver<bool>,
    ) -> Result<GoalResult, PackError>;
}
