//! # liberado-daemon
//!
//! The long-running core of Liberado (Decision 2: daemon-first). This is the v1 **vertical
//! slice**: it watches the vault, attributes every observed change (loop-breaking, Decision 5),
//! and forwards the changes that came from *outside* our own write path as standardized
//! [`Event`]s. Downstream (the dispatcher, ACPs) consume those events; here we just produce them.
//!
//! The reactive decision is split into a pure, deterministic [`Daemon::process_change`] (testable
//! without the filesystem) and the watcher plumbing in [`Daemon::run`] — mirroring how the vault
//! crate separates attribution from I/O.

mod debounce;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use debounce::Debouncer;
use liberado_common::{CapabilitySet, DispatchDecision, Event, EventPayload, event_source};
use liberado_dispatcher::{DispatchRequest, Dispatcher, McpDescriptor};
use liberado_orchestrator::{Disposition, Orchestrator};
use liberado_vault::{Attribution, Vault, VaultError, VaultEvent};
use thiserror::Error;
use tokio::sync::mpsc::UnboundedSender;

/// Default debounce window: long enough to coalesce a `notify` burst, short enough to feel live.
const DEFAULT_DEBOUNCE: Duration = Duration::from_millis(400);

/// Depth assigned to a daemon-originated reaction. It is the first agent step reacting to an
/// external change, so it starts the correlation chain at 1 (the depth cap halts longer cascades).
const DEFAULT_REACTION_DEPTH: u32 = 1;

/// Prefix of the correlation id minted for a vault-change reaction, and how many hex chars of the
/// content hash to append (enough to distinguish edits; the full id stays short for logs/journals).
const CORRELATION_PREFIX: &str = "vault-change";
const CORRELATION_HASH_LEN: usize = 12;

/// What the daemon produces for each external change it reacts to: the standardized event plus how
/// far it took the reaction.
pub struct Reaction {
    pub event: Event,
    pub outcome: ReactionOutcome,
}

/// How far a reaction progressed, depending on what's attached to the daemon.
pub enum ReactionOutcome {
    /// Watch-only — no dispatcher attached. The event is surfaced but not routed.
    Observed,
    /// A dispatcher routed it to a decision, but no orchestrator is attached to execute it.
    Decided(DispatchDecision),
    /// A dispatcher decided and an orchestrator executed it (or surfaced a `Clarify`).
    Acted(Disposition),
}

impl ReactionOutcome {
    /// A short label for tracing.
    pub fn label(&self) -> &'static str {
        match self {
            ReactionOutcome::Observed => "(observed)",
            ReactionOutcome::Decided(d) => d.action.label(),
            ReactionOutcome::Acted(Disposition::Reported(_)) => "acted:reported",
            ReactionOutcome::Acted(Disposition::Clarify { .. }) => "acted:clarify",
        }
    }
}

/// The dispatcher plus the disjoint context the daemon hands it for each reaction.
struct DispatcherContext {
    dispatcher: Dispatcher,
    catalog: Vec<McpDescriptor>,
    capabilities: CapabilitySet,
    reaction_depth: u32,
}

impl DispatcherContext {
    /// Turn a vault-change event into a self-contained dispatch request.
    fn dispatch_request(&self, event: &Event) -> DispatchRequest {
        let path = event.payload.path.as_deref().unwrap_or("(unknown)");
        DispatchRequest {
            goal: format!(
                "A note in the vault was created or edited at '{path}'. Decide how to react to it."
            ),
            catalog: self.catalog.clone(),
            capabilities: self.capabilities.clone(),
            reaction_depth: self.reaction_depth,
        }
    }
}

/// The event type emitted for an attributed-external vault change.
pub const VAULT_NOTE_CHANGED: &str = "VaultNoteChanged";

/// Errors from the daemon.
#[derive(Debug, Error)]
pub enum DaemonError {
    #[error(transparent)]
    Vault(#[from] VaultError),
}

/// The Liberado daemon.
pub struct Daemon {
    vault: Vault,
    debounce: Duration,
    dispatcher: Option<DispatcherContext>,
    orchestrator: Option<Orchestrator>,
}

impl Daemon {
    /// Open the daemon over the vault at `vault_path` (enables the audit log).
    pub async fn open(
        name: impl Into<String>,
        vault_path: impl Into<PathBuf>,
    ) -> Result<Self, DaemonError> {
        Ok(Self {
            vault: Vault::open(name, vault_path).await?,
            debounce: DEFAULT_DEBOUNCE,
            dispatcher: None,
            orchestrator: None,
        })
    }

    /// Override the debounce window (e.g. a short window in tests).
    pub fn with_debounce(mut self, debounce: Duration) -> Self {
        self.debounce = debounce;
        self
    }

    /// Attach a dispatcher so reactable changes are routed to a [`DispatchDecision`]. Without one,
    /// the daemon runs in watch-only mode ([`ReactionOutcome::Observed`]). The `catalog` +
    /// `capabilities` form the disjoint context the dispatcher reasons over.
    pub fn with_dispatcher(
        mut self,
        dispatcher: Dispatcher,
        catalog: Vec<McpDescriptor>,
        capabilities: CapabilitySet,
    ) -> Self {
        self.dispatcher = Some(DispatcherContext {
            dispatcher,
            catalog,
            capabilities,
            reaction_depth: DEFAULT_REACTION_DEPTH,
        });
        self
    }

    /// Attach an orchestrator so decisions are **executed** (the reaction yields
    /// [`ReactionOutcome::Acted`]). Only meaningful alongside [`with_dispatcher`](Self::with_dispatcher):
    /// without a dispatcher there is no decision to execute; with a dispatcher but no orchestrator,
    /// reactions stop at [`ReactionOutcome::Decided`].
    pub fn with_orchestrator(mut self, orchestrator: Orchestrator) -> Self {
        self.orchestrator = Some(orchestrator);
        self
    }

    /// The underlying vault handle (cheap to clone).
    pub fn vault(&self) -> &Vault {
        &self.vault
    }

    /// The pure reactive decision: given an observed change to `rel_path`, return a reactable
    /// [`Event`], or `None` if the change was one of our own writes (suppressed by the hash-join)
    /// or the path is gone. No filesystem watching here — this is the unit-testable core.
    pub async fn process_change(&self, rel_path: &Path) -> Result<Option<Event>, DaemonError> {
        match self.vault.attribute(rel_path).await? {
            Attribution::External => Ok(Some(self.build_event(rel_path).await)),
            // Our own write (don't react to ourselves) or a vanished path.
            Attribution::Agent(_) | Attribution::Missing => Ok(None),
        }
    }

    /// Build the standardized event for an attributed-external change. The `correlation_id` keys
    /// idempotency/loop-breaking downstream; it is derived from the path + a short content hash so
    /// distinct edits are distinct events while a redelivery of the same state is not.
    async fn build_event(&self, rel_path: &Path) -> Event {
        let content = self.vault.read(rel_path).await.unwrap_or_default();
        let hash = Vault::content_hash(&content);
        let rel = rel_path.to_string_lossy().replace('\\', "/");
        // `get(..N)` (not `[..N]`) is panic-safe regardless of the hash's byte boundaries.
        let short_hash = hash.get(..CORRELATION_HASH_LEN).unwrap_or(&hash);
        let correlation_id = format!("{CORRELATION_PREFIX}:{rel}:{short_hash}");
        Event::trigger(
            VAULT_NOTE_CHANGED,
            event_source::TURBOVAULT_SUBSCRIPTION,
            correlation_id,
            EventPayload {
                path: Some(rel),
                ..Default::default()
            },
        )
    }

    /// Take a reactable change as far as the attached components allow: observe → decide → act.
    /// Failures at any stage are logged and degrade the outcome (never abort the watch loop).
    async fn react(&self, event: &Event) -> ReactionOutcome {
        let Some(ctx) = self.dispatcher.as_ref() else {
            return ReactionOutcome::Observed; // watch-only
        };

        let request = ctx.dispatch_request(event);
        let decision = match ctx.dispatcher.dispatch(&request).await {
            Ok(decision) => decision,
            Err(e) => {
                tracing::warn!(error = %e, "dispatch failed");
                return ReactionOutcome::Observed;
            }
        };

        let Some(orchestrator) = self.orchestrator.as_ref() else {
            return ReactionOutcome::Decided(decision); // decided, nothing to execute with
        };

        match orchestrator
            .run(decision.clone(), &request.goal, &event.correlation_id)
            .await
        {
            Ok(disposition) => ReactionOutcome::Acted(disposition),
            Err(e) => {
                // Couldn't execute — surface the decision we did reach.
                tracing::warn!(error = %e, "orchestration failed");
                ReactionOutcome::Decided(decision)
            }
        }
    }

    /// Run the watch loop until the watcher shuts down. Raw filesystem events are debounced per
    /// path (coalescing a notify burst into one settled change); each resulting external change is
    /// attributed, routed through the dispatcher (if attached), and the [`Reaction`] forwarded to
    /// `reactions`. Returns when the channel's receiver is dropped or the watcher closes.
    pub async fn run(self, reactions: UnboundedSender<Reaction>) -> Result<(), DaemonError> {
        let mut watch = self.vault.watch().await?;
        let mut debouncer = Debouncer::new(self.debounce);
        tracing::info!(
            vault = %self.vault.root().display(),
            debounce_ms = self.debounce.as_millis() as u64,
            "daemon watching vault"
        );

        loop {
            // Copy out the next deadline so the timer future borrows nothing from `debouncer`,
            // leaving the select arms free to mutate it.
            let next_deadline = debouncer.next_deadline();

            tokio::select! {
                maybe_event = watch.next_event() => {
                    let Some(event) = maybe_event else { break }; // watcher shut down
                    // Deletions carry no content to hash-join; reacting to them is a later iteration.
                    if let VaultEvent::FileDeleted(_) = event {
                        continue;
                    }
                    if let Some(rel) = self.vault.to_relative(event.path()) {
                        debouncer.observe(rel, Instant::now());
                    }
                }

                _ = sleep_until(next_deadline) => {
                    for rel in debouncer.drain_ready(Instant::now()) {
                        match self.process_change(&rel).await {
                            Ok(Some(event)) => {
                                let outcome = self.react(&event).await;
                                tracing::info!(
                                    path = event.payload.path.as_deref().unwrap_or_default(),
                                    outcome = outcome.label(),
                                    "reacting to external change"
                                );
                                if reactions.send(Reaction { event, outcome }).is_err() {
                                    return Ok(()); // receiver gone
                                }
                            }
                            Ok(None) => {} // our own write or vanished path — suppressed
                            Err(e) => tracing::warn!(error = %e, ?rel, "attribution failed"),
                        }
                    }
                }
            }
        }

        Ok(())
    }
}

/// Sleep until `deadline`, or forever when `None` (so the watch loop's select only wakes on
/// incoming events while nothing is pending).
async fn sleep_until(deadline: Option<Instant>) {
    match deadline {
        Some(deadline) => {
            tokio::time::sleep(deadline.saturating_duration_since(Instant::now())).await
        }
        None => std::future::pending::<()>().await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use liberado_common::WriteProvenance;
    use std::time::Duration;
    use tempfile::TempDir;
    use tokio::sync::mpsc::unbounded_channel;

    async fn temp_daemon() -> (Daemon, TempDir) {
        let dir = TempDir::new().unwrap();
        let daemon = Daemon::open("test", dir.path()).await.unwrap();
        (daemon, dir)
    }

    #[tokio::test]
    async fn external_change_produces_reaction() {
        let (daemon, dir) = temp_daemon().await;
        // A human writes a note directly (not through the adapter) — no matching audit entry.
        std::fs::write(dir.path().join("note.md"), "a human wrote this").unwrap();

        let event = daemon
            .process_change(Path::new("note.md"))
            .await
            .unwrap()
            .expect("external change should produce a reaction");
        assert_eq!(event.event_type, VAULT_NOTE_CHANGED);
        assert_eq!(event.source, event_source::TURBOVAULT_SUBSCRIPTION);
        assert_eq!(event.payload.path.as_deref(), Some("note.md"));
        assert!(event.is_reactable());
    }

    #[tokio::test]
    async fn our_own_write_is_suppressed() {
        let (daemon, _dir) = temp_daemon().await;
        let prov = WriteProvenance::agent("tasks-mcp", "c1");
        daemon
            .vault()
            .write("tasks/today.md", "- [ ] x", None, &prov)
            .await
            .unwrap();

        assert!(
            daemon
                .process_change(Path::new("tasks/today.md"))
                .await
                .unwrap()
                .is_none(),
            "agent write must not trigger a reaction"
        );
    }

    #[tokio::test]
    async fn missing_path_is_suppressed() {
        let (daemon, _dir) = temp_daemon().await;
        assert!(
            daemon
                .process_change(Path::new("nope.md"))
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn watcher_coalesces_burst_into_single_reaction() {
        let (daemon, dir) = temp_daemon().await;
        let daemon = daemon.with_debounce(Duration::from_millis(80));
        let vault_dir = dir.path().to_path_buf();
        let (tx, mut rx) = unbounded_channel();
        let handle = tokio::spawn(daemon.run(tx));

        // Give the watcher a moment to establish before writing.
        tokio::time::sleep(Duration::from_millis(300)).await;
        std::fs::write(vault_dir.join("captured.md"), "dropped in from Obsidian").unwrap();

        // Exactly one reaction, despite notify firing Create + Modify + ... for one write.
        let reaction = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("timed out waiting for reaction")
            .expect("reaction channel closed");
        assert_eq!(reaction.event.payload.path.as_deref(), Some("captured.md"));
        assert!(
            matches!(reaction.outcome, ReactionOutcome::Observed),
            "watch-only: no dispatcher attached"
        );

        // No duplicate arrives within a generous margin past the debounce window.
        tokio::time::sleep(Duration::from_millis(400)).await;
        assert!(
            rx.try_recv().is_err(),
            "the notify burst should have coalesced into a single reaction"
        );

        handle.abort();
    }

    #[tokio::test]
    async fn daemon_routes_reaction_through_dispatcher() {
        use liberado_common::config::DispatchTuning;
        use liberado_common::{BlockReason, DispatchAction, DispatchDecision};
        use liberado_provider::{CompletionResponse, MockProvider};
        use std::sync::Arc;

        let (daemon, dir) = temp_daemon().await;

        // A dispatcher whose (mock) classifier returns a Clarify — the safe outcome with no MCPs.
        let canned = DispatchDecision {
            action: DispatchAction::Clarify {
                questions: vec!["how should I handle this note?".into()],
                what_blocked: BlockReason::Ambiguous,
            },
            confidence: 0.9,
            rationale: "test".into(),
        };
        let mock = Arc::new(MockProvider::with_script(
            "mock",
            [CompletionResponse::text(
                serde_json::to_string(&canned).unwrap(),
            )],
        ));
        let dispatcher = Dispatcher::new(mock, DispatchTuning::default(), 4);

        let daemon = daemon
            .with_debounce(Duration::from_millis(80))
            .with_dispatcher(dispatcher, vec![], CapabilitySet::empty());

        let vault_dir = dir.path().to_path_buf();
        let (tx, mut rx) = unbounded_channel();
        let handle = tokio::spawn(daemon.run(tx));

        tokio::time::sleep(Duration::from_millis(300)).await;
        std::fs::write(vault_dir.join("idea.md"), "a captured thought").unwrap();

        let reaction = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("timed out waiting for reaction")
            .expect("reaction channel closed");

        assert_eq!(reaction.event.payload.path.as_deref(), Some("idea.md"));
        // Dispatcher attached, but no orchestrator → decided, not acted.
        let ReactionOutcome::Decided(decision) = reaction.outcome else {
            panic!("expected Decided, got {}", reaction.outcome.label());
        };
        assert!(matches!(decision.action, DispatchAction::Clarify { .. }));

        handle.abort();
    }

    #[tokio::test]
    async fn daemon_acts_on_a_decision_via_the_orchestrator() {
        use liberado_common::config::DispatchTuning;
        use liberado_common::{DispatchAction, DispatchDecision, Outcome};
        use liberado_executor::{SUBMIT_REPORT_TOOL, ToolRuntime};
        use liberado_orchestrator::{Orchestrator, RuntimeFactory, RuntimeSetupError};
        use liberado_provider::{CompletionResponse, MockProvider, ToolDef, ToolInvocation};
        use std::sync::Arc;

        // A tool runtime + factory that need no real MCP (the scripted model just files a report).
        struct NoopRuntime;
        #[async_trait::async_trait]
        impl ToolRuntime for NoopRuntime {
            fn catalog(&self) -> Vec<ToolDef> {
                Vec::new()
            }
            async fn invoke(&self, _call: &ToolInvocation) -> Result<String, String> {
                Ok("ok".into())
            }
        }
        struct NoopFactory;
        #[async_trait::async_trait]
        impl RuntimeFactory for NoopFactory {
            async fn runtime_for(
                &self,
                _allowed_mcps: &[String],
                _provenance: liberado_common::WriteProvenance,
            ) -> Result<Box<dyn ToolRuntime>, RuntimeSetupError> {
                Ok(Box::new(NoopRuntime))
            }
        }

        let (daemon, dir) = temp_daemon().await;

        // Dispatcher classifies to ExecuteDirect (no MCPs referenced → passes the guards).
        let decision = DispatchDecision {
            action: DispatchAction::ExecuteDirect {
                seed_calls: Vec::new(),
            },
            confidence: 0.95,
            rationale: "trivial".into(),
        };
        let dispatch_provider = Arc::new(MockProvider::with_script(
            "dispatch",
            [CompletionResponse::text(
                serde_json::to_string(&decision).unwrap(),
            )],
        ));
        let dispatcher = Dispatcher::new(dispatch_provider, DispatchTuning::default(), 4);

        // The orchestrator's executor: the model immediately files a report.
        let exec_provider = Arc::new(MockProvider::with_script(
            "exec",
            [CompletionResponse::tool_calls(vec![ToolInvocation::new(
                "c",
                SUBMIT_REPORT_TOOL,
                serde_json::json!({ "outcome": "succeeded", "summary": "done" }),
            )])],
        ));
        let orchestrator = Orchestrator::new(exec_provider, NoopFactory);

        let daemon = daemon
            .with_debounce(Duration::from_millis(80))
            .with_dispatcher(dispatcher, vec![], CapabilitySet::empty())
            .with_orchestrator(orchestrator);

        let vault_dir = dir.path().to_path_buf();
        let (tx, mut rx) = unbounded_channel();
        let handle = tokio::spawn(daemon.run(tx));

        tokio::time::sleep(Duration::from_millis(300)).await;
        std::fs::write(vault_dir.join("act.md"), "do something").unwrap();

        let reaction = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("timed out")
            .expect("channel closed");

        let ReactionOutcome::Acted(Disposition::Reported(report)) = reaction.outcome else {
            panic!("expected Acted/Reported, got {}", reaction.outcome.label());
        };
        assert_eq!(report.outcome, Outcome::Succeeded);

        handle.abort();
    }
}
