//! # liberado-dispatch-pack
//!
//! The dispatcher + orchestrator pair, dressed as a [`DomainPackRunner`] so unattended work
//! (cron, webhooks, `delegate`) runs through the **same** [`GoalSessionHub`] as `/spawn`.
//!
//! This is not a third engine — it is the second one folded into the first
//! (`docs/roadmap/one-execution-engine-plan.md` E2). Pools stay: each [`Orchestrator`] owns an
//! `McpRegistry` that is not shareable across instances, so the pack holds one
//! (dispatcher, orchestrator) pair per pool name.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use liberado_common::{CapabilityCatalog, DEFAULT_POOL, PROPOSALS_DIR, SignedProposal, WriteClass};
use liberado_dispatcher::{DispatchRequest, Dispatcher};
use liberado_notify::Notifier;
use liberado_orchestrator::{Disposition, Orchestrator};
use liberado_session::{
    DomainPackRunner, GoalResult, GoalSpec, InputChannel, PackContext, PackError, SessionEvent,
    SessionEventKind, TurnAuthor,
};
use tokio::sync::mpsc::Sender;

/// Stable domain id for this pack — what a reaction's `GoalSpec.domain` names, and what surfaces
/// show as the pack that ran it. No longer a lie: joining one is a real hosted session.
pub const DISPATCH_DOMAIN: &str = "dispatch";

/// One named pool's dispatcher + orchestrator pair.
struct Pool {
    dispatcher: Dispatcher,
    orchestrator: Orchestrator,
}

/// Runs dispatch → orchestrate as a goal-session pack.
///
/// Holds pools by name. `goal.payload["pool"]` selects which; missing/empty → [`DEFAULT_POOL`].
pub struct DispatchPack {
    pools: HashMap<String, Pool>,
    /// Live catalog snapshotted fresh per run (same object the daemon/API share).
    catalog: Arc<CapabilityCatalog>,
    /// `(zone, write_class)` pairs for the zone-write-class guard — default-pool values; additional
    /// pools currently share this (v1, same as the daemon's pre-pack configuration).
    zone_write_classes: Vec<(String, WriteClass)>,
    reaction_depth: u32,
    /// Where a `Propose` disposition writes its draft note (`proposals_dir/proposals/<id>.md`).
    proposals_dir: PathBuf,
    notifier: Option<Arc<dyn Notifier>>,
}

impl DispatchPack {
    /// Build an empty pack; call [`with_pool`](Self::with_pool) for each pool before registering.
    pub fn new(
        catalog: Arc<CapabilityCatalog>,
        zone_write_classes: Vec<(String, WriteClass)>,
        reaction_depth: u32,
        proposals_dir: PathBuf,
    ) -> Self {
        Self {
            pools: HashMap::new(),
            catalog,
            zone_write_classes,
            reaction_depth,
            proposals_dir,
            notifier: None,
        }
    }

    pub fn with_notifier(mut self, notifier: Arc<dyn Notifier>) -> Self {
        self.notifier = Some(notifier);
        self
    }

    /// Attach (or replace) the named pool's dispatcher/orchestrator.
    pub fn with_pool(
        mut self,
        name: impl Into<String>,
        dispatcher: Dispatcher,
        orchestrator: Orchestrator,
    ) -> Self {
        self.pools.insert(
            name.into(),
            Pool {
                dispatcher,
                orchestrator,
            },
        );
        self
    }

    fn pool_name(goal: &GoalSpec) -> &str {
        goal.payload
            .get("pool")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .unwrap_or(DEFAULT_POOL)
    }

    /// Write a pre-flight `Propose` note and notify the human. Returns whether the out-of-band
    /// notification actually went out — the caller folds that into `GoalResult`'s `deferred_to_human`
    /// so a chat surface suppresses the redundant reply only when the human really got the ping
    /// (Gap 2). A best-effort notify failure is not fatal (the note is already safely written).
    async fn write_proposal(&self, proposal: &SignedProposal) -> Result<bool, String> {
        let proposals_subdir = self.proposals_dir.join(PROPOSALS_DIR);
        let proposal_path = proposals_subdir.join(format!("{}.md", proposal.id));
        tokio::fs::create_dir_all(&proposals_subdir)
            .await
            .map_err(|e| format!("create proposals dir: {e}"))?;
        tokio::fs::write(&proposal_path, proposal.to_note())
            .await
            .map_err(|e| format!("write proposal: {e}"))?;

        let mut notified = false;
        if let Some(notifier) = &self.notifier {
            let stem = proposal.id.replace([':', '/'], "-");
            let message = format!(
                "Liberado: a new proposal needs your review.\n{}\nSaved at: {}",
                proposal.rationale,
                proposal_path.display()
            );
            match notifier.notify_proposal(&stem, &message).await {
                Ok(()) => notified = true,
                Err(e) => tracing::warn!(error = %e, "failed to send proposal notification"),
            }
        }
        Ok(notified)
    }
}

#[async_trait]
impl DomainPackRunner for DispatchPack {
    fn domain_id(&self) -> &str {
        DISPATCH_DOMAIN
    }

    async fn run(
        &self,
        session_id: &str,
        goal: &GoalSpec,
        ctx: &PackContext<'_>,
        events: Sender<SessionEvent>,
        _inputs: InputChannel,
        cancel: tokio::sync::watch::Receiver<bool>,
    ) -> Result<GoalResult, PackError> {
        if *cancel.borrow() {
            return Err(PackError::Cancelled);
        }

        let pool_name = Self::pool_name(goal);
        let pool = self.pools.get(pool_name).ok_or_else(|| {
            PackError::Setup(format!(
                "no dispatch pool '{pool_name}' (have: {:?})",
                self.pools.keys().collect::<Vec<_>>()
            ))
        })?;

        let correlation_id = goal
            .origin
            .as_ref()
            .and_then(|o| o.correlation_id.clone())
            .unwrap_or_else(|| format!("dispatch-session-{session_id}"));

        // The session grant is the per-run authority (E1). Classification and execution both see it;
        // the orchestrator still intersects with its pool ceiling so a grant can never widen.
        let request = DispatchRequest {
            goal: goal.description.clone(),
            // M1b: do not offer degraded MCP peers to the classifier.
            catalog: self.catalog.routing_descriptors(),
            capabilities: ctx.grant.capabilities.clone(),
            reaction_depth: self.reaction_depth,
            zone_write_classes: self.zone_write_classes.clone(),
        };

        // Tag every inference this pack triggers (dispatcher classification + orchestrator loop)
        // with this run's correlation id, so the latency journal joins them to the parent chat turn.
        // The role is already fixed on each per-role provider; only the correlation is task-local.
        // Goal already recorded as the human's first turn by the hub. Record the decision rationale
        // and outcome as turns so the transcript is searchable dialogue, not only events.
        let decision = match liberado_provider::latency::with_correlation(
            correlation_id.clone(),
            pool.dispatcher.dispatch(&request),
        )
        .await
        {
            Ok(d) => d,
            Err(e) => {
                return Err(PackError::Failed(format!("dispatch failed: {e}")));
            }
        };

        if *cancel.borrow() {
            return Err(PackError::Cancelled);
        }

        let progress = format!(
            "dispatched: {} (confidence {:.2}) — {}",
            decision.action.label(),
            decision.confidence,
            decision.rationale
        );
        ctx.record_turn(TurnAuthor::Assistant, progress.clone())
            .await;
        let _ = events
            .send(SessionEvent::new(
                session_id,
                SessionEventKind::Progress { message: progress },
            ))
            .await;

        if *cancel.borrow() {
            return Err(PackError::Cancelled);
        }

        let disposition = liberado_provider::latency::with_correlation(
            correlation_id.clone(),
            pool.orchestrator.run(
                decision,
                &goal.description,
                &correlation_id,
                &ctx.grant.capabilities,
            ),
        )
        .await
        .map_err(|e| PackError::Failed(format!("orchestration failed: {e}")))?;

        // Whether this run deferred the action to the human AND surfaced it out-of-band, so the
        // face agent (via `delegate`) can drop the redundant reply (Gap 2). A `Reported` carries the
        // runtime's own flag; a `Propose`'s ping is sent here, by `write_proposal`, so its notified-
        // state is only known once that returns.
        let deferred_to_human = if let Disposition::Propose(ref proposal) = disposition {
            self.write_proposal(proposal)
                .await
                .map_err(PackError::Failed)?
        } else {
            disposition.deferred_to_human()
        };

        let (terminal, summary) = disposition.terminal_summary();
        // Outcome turn is also recorded by the hub's run_session; recording the disposition-shaped
        // summary here would duplicate. The hub always closes with goal_result.summary — so just
        // return it.
        let _ = events
            .send(SessionEvent::new(
                session_id,
                SessionEventKind::Progress {
                    message: summary.clone(),
                },
            ))
            .await;

        Ok(GoalResult {
            terminal,
            summary,
            artifacts: Vec::new(),
            diagnostics: serde_json::json!({
                "pool": pool_name,
                "correlation_id": correlation_id,
                "deferred_to_human": deferred_to_human,
            }),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use liberado_common::WriteProvenance;
    use liberado_common::{
        Capability, CapabilitySet, DispatchAction, DispatchDecision, Outcome, ProposalSigner,
        Report,
    };
    use liberado_config_loader::DispatchTuning;
    use liberado_executor::{RuntimeFactory, RuntimeSetupError, SUBMIT_REPORT_TOOL, ToolRuntime};
    use liberado_provider::{CompletionResponse, MockProvider, ToolInvocation};
    use liberado_session::{
        DomainHint, GoalSessionHub, GoalSessionStore, SessionGrant, SessionStatus, TerminalKind,
    };
    use std::sync::Arc;

    struct NoopFactory;
    #[async_trait]
    impl RuntimeFactory for NoopFactory {
        async fn runtime_for(
            &self,
            _allowed_mcps: &[String],
            _provenance: WriteProvenance,
        ) -> Result<Box<dyn ToolRuntime>, RuntimeSetupError> {
            Ok(Box::new(NoopRuntime))
        }
    }

    struct NoopRuntime;
    #[async_trait]
    impl ToolRuntime for NoopRuntime {
        fn catalog(&self) -> Vec<liberado_provider::ToolDef> {
            Vec::new()
        }
        async fn invoke(&self, _call: &ToolInvocation) -> Result<String, String> {
            Err("noop".into())
        }
    }

    fn make_pack(
        decision: DispatchDecision,
        exec_script: Vec<CompletionResponse>,
        pool_caps: CapabilitySet,
    ) -> DispatchPack {
        let dispatcher = Dispatcher::new(
            Arc::new(MockProvider::with_script(
                "dispatch",
                [CompletionResponse::text(
                    serde_json::to_string(&decision).unwrap(),
                )],
            )),
            DispatchTuning::default(),
            4,
        );
        let orchestrator = Orchestrator::new(
            Arc::new(MockProvider::with_script("exec", exec_script)),
            NoopFactory,
            pool_caps,
            Vec::new(),
            Vec::new(),
            Vec::new(),
            std::env::temp_dir(),
            ProposalSigner::random(),
            DEFAULT_POOL,
        );
        DispatchPack::new(
            Arc::new(CapabilityCatalog::new()),
            Vec::new(),
            1,
            std::env::temp_dir(),
        )
        .with_pool(DEFAULT_POOL, dispatcher, orchestrator)
    }

    #[tokio::test]
    async fn pack_runs_execute_direct_to_a_terminal_session() {
        let decision = DispatchDecision {
            action: DispatchAction::ExecuteDirect {
                seed_calls: Vec::new(),
                relevant_mcps: Vec::new(),
            },
            confidence: 0.95,
            rationale: "routine work".into(),
        };
        let pack = make_pack(
            decision,
            vec![CompletionResponse::tool_calls(vec![ToolInvocation::new(
                "c",
                SUBMIT_REPORT_TOOL,
                serde_json::json!({ "outcome": "succeeded", "summary": "done via pack" }),
            )])],
            CapabilitySet::empty(),
        );

        let mut hub = GoalSessionHub::new(GoalSessionStore::new());
        hub.register_pack(Arc::new(pack));
        let hub = Arc::new(hub);

        let id = hub
            .start_with_grant(
                GoalSpec {
                    id: None,
                    description: "summarize today".into(),
                    success_criteria: vec![],
                    domain: DomainHint::from(DISPATCH_DOMAIN),
                    max_turns: 0,
                    max_idle_secs: None,
                    origin: None,
                    profile: None,
                    payload: serde_json::json!({}),
                },
                SessionGrant::default(),
            )
            .await
            .expect("start");

        // Poll until terminal.
        let snap = wait_terminal(&hub, &id).await;
        assert_eq!(snap.session.status, SessionStatus::Succeeded);
        assert_eq!(
            snap.session.result.as_ref().unwrap().summary,
            "done via pack"
        );
        assert_eq!(
            snap.session.result.as_ref().unwrap().terminal,
            TerminalKind::Succeeded
        );
        // Decision narrated as a progress event.
        assert!(
            snap.events.iter().any(|e| matches!(
                &e.kind,
                SessionEventKind::Progress { message }
                    if message.contains("ExecuteDirect") && message.contains("routine work")
            )),
            "decision must be narrated: {:#?}",
            snap.events
        );
    }

    #[tokio::test]
    async fn pack_honours_a_narrow_session_grant_over_a_wide_pool() {
        // Pool would allow tasks-mcp; session grant is empty — ExecuteDirect must not open a runtime
        // for any MCP (same proof as E1, through the pack).
        let decision = DispatchDecision {
            action: DispatchAction::ExecuteDirect {
                seed_calls: Vec::new(),
                relevant_mcps: Vec::new(),
            },
            confidence: 0.9,
            rationale: "would write if allowed".into(),
        };
        let pool_caps = CapabilitySet::from_iter([Capability::ExecuteMcp("tasks-mcp".into())]);
        let pack = make_pack(
            decision,
            vec![CompletionResponse::tool_calls(vec![ToolInvocation::new(
                "c",
                SUBMIT_REPORT_TOOL,
                serde_json::json!({ "outcome": "succeeded", "summary": "ran with no tools" }),
            )])],
            pool_caps,
        );

        let mut hub = GoalSessionHub::new(GoalSessionStore::new());
        hub.register_pack(Arc::new(pack));
        let hub = Arc::new(hub);

        // Empty grant — narrower than the pool.
        let id = hub
            .start_with_grant(
                GoalSpec {
                    id: None,
                    description: "try a write".into(),
                    success_criteria: vec![],
                    domain: DomainHint::from(DISPATCH_DOMAIN),
                    max_turns: 0,
                    max_idle_secs: None,
                    origin: None,
                    profile: None,
                    payload: serde_json::json!({}),
                },
                SessionGrant {
                    capabilities: CapabilitySet::empty(),
                    profile: Some("read-only-cron".into()),
                    overrides: serde_json::Value::Null,
                    ..Default::default()
                },
            )
            .await
            .expect("start");

        let snap = wait_terminal(&hub, &id).await;
        assert_eq!(snap.session.status, SessionStatus::Succeeded);
        // With zero grants the orchestrator uses NoMcpRuntime; the mock still submits a report.
        assert!(
            snap.session
                .result
                .as_ref()
                .unwrap()
                .summary
                .contains("ran with no tools")
        );
    }

    #[tokio::test]
    async fn clarify_fails_honestly() {
        use liberado_common::BlockReason;
        let decision = DispatchDecision {
            action: DispatchAction::Clarify {
                questions: vec!["which project?".into()],
                what_blocked: BlockReason::Ambiguous,
            },
            confidence: 0.5,
            rationale: "ambiguous".into(),
        };
        let pack = make_pack(decision, vec![], CapabilitySet::empty());
        let mut hub = GoalSessionHub::new(GoalSessionStore::new());
        hub.register_pack(Arc::new(pack));
        let hub = Arc::new(hub);
        let id = hub
            .start_with_grant(
                GoalSpec {
                    id: None,
                    description: "do the thing".into(),
                    success_criteria: vec![],
                    domain: DomainHint::from(DISPATCH_DOMAIN),
                    max_turns: 0,
                    max_idle_secs: None,
                    origin: None,
                    profile: None,
                    payload: serde_json::json!({}),
                },
                SessionGrant::default(),
            )
            .await
            .unwrap();
        let snap = wait_terminal(&hub, &id).await;
        assert_eq!(snap.session.status, SessionStatus::Failed);
        assert!(
            snap.session
                .result
                .as_ref()
                .unwrap()
                .summary
                .contains("which project?")
        );
    }

    async fn wait_terminal(
        hub: &Arc<GoalSessionHub>,
        id: &str,
    ) -> liberado_session::SessionSnapshot {
        for _ in 0..200 {
            if let Some(snap) = hub.snapshot(id).await
                && snap.session.status.is_terminal()
            {
                return snap;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        panic!("session {id} did not reach terminal");
    }

    #[test]
    fn disposition_reported_maps_through_terminal_summary() {
        let d = Disposition::Reported(Report {
            outcome: Outcome::Succeeded,
            summary: "ok".into(),
            artifacts: vec![],
            new_high_signal_facts: vec![],
            follow_up: None,
            deferred_to_human: false,
        });
        let (t, s) = d.terminal_summary();
        assert_eq!(t, TerminalKind::Succeeded);
        assert_eq!(s, "ok");
    }

    #[test]
    fn pool_name_defaults_and_reads_payload() {
        let mut goal = GoalSpec {
            id: None,
            description: "g".into(),
            success_criteria: vec![],
            domain: DomainHint::from(DISPATCH_DOMAIN),
            max_turns: 0,
            max_idle_secs: None,
            origin: None,
            profile: None,
            payload: serde_json::json!({}),
        };
        assert_eq!(DispatchPack::pool_name(&goal), DEFAULT_POOL);
        goal.payload = serde_json::json!({ "pool": "research" });
        assert_eq!(DispatchPack::pool_name(&goal), "research");
    }
}
