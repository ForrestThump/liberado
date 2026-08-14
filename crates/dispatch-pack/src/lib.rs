//! # liberado-dispatch-pack
//!
//! The dispatcher + orchestrator pair, dressed as a [`DomainPackRunner`] so unattended work
//! (cron, webhooks, `delegate`) runs through the **same** `GoalSessionHub` as `/spawn`.
//!
//! This is not a third engine — it is the second one folded into the first
//! (`docs/future-work/archive/one-execution-engine-plan.md` E2). Pools stay: each [`Orchestrator`] owns an
//! `McpRegistry` that is not shareable across instances, so the pack holds one
//! (dispatcher, orchestrator) pair per pool name.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use liberado_coder_core::CommandPolicy;
use liberado_coder_sandbox::{Workspace, WorktreeWorkspace};
use liberado_common::{CapabilityCatalog, DEFAULT_POOL, PROPOSALS_DIR, SignedProposal, WriteClass};
use liberado_dispatcher::{DispatchRequest, Dispatcher};
use liberado_notify::Notifier;
use liberado_orchestrator::{Disposition, Orchestrator, SubDispatch};
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

    /// Production C7 path: `parallel_goals` on the payload reaches `dispatch_parallel`.
    /// When `parent_root` is set, each worker gets a `WorktreeWorkspace`. Worktree
    /// creation is fail-closed — a shared fallback would be silent collision.
    async fn run_parallel(
        pool: &Pool,
        session_id: &str,
        goal: &GoalSpec,
        events: &Sender<SessionEvent>,
        correlation_id: &str,
        parallel_goals: &[serde_json::Value],
    ) -> Result<GoalResult, PackError> {
        if parallel_goals.is_empty() {
            return Err(PackError::Setup(
                "parallel dispatch requires at least one sub-goal".into(),
            ));
        }
        let max_concurrent = goal
            .payload
            .get("max_concurrent_parallel")
            .and_then(|v| v.as_u64())
            .unwrap_or(4)
            .max(1) as usize;
        let parent_root: Option<PathBuf> = goal
            .payload
            .get("parent_root")
            .and_then(|v| v.as_str())
            .map(PathBuf::from);
        let worktrees_base: PathBuf = goal
            .payload
            .get("worktrees_base")
            .and_then(|v| v.as_str())
            .map(PathBuf::from)
            .unwrap_or_else(|| std::env::temp_dir().join("dispatch-worktrees"));

        let _ = events
            .send(SessionEvent::new(
                session_id,
                SessionEventKind::Progress {
                    message: format!(
                        "parallel dispatch: {} sub-goal(s), max_concurrent={max_concurrent}, worktree_isolation={}",
                        parallel_goals.len(),
                        if parent_root.is_some() { "yes" } else { "no" },
                    ),
                },
            ))
            .await;

        let mut worktrees: Vec<Option<WorktreeWorkspace>> =
            Vec::with_capacity(parallel_goals.len());
        if let Some(parent) = parent_root.as_ref() {
            for i in 0..parallel_goals.len() {
                let wt_session = format!("dp-{correlation_id}-{i}");
                let ws = WorktreeWorkspace::new(
                    parent,
                    &wt_session,
                    &worktrees_base,
                    CommandPolicy::default(),
                )
                .await
                .map_err(|e| {
                    PackError::Failed(format!("worktree for parallel sub-goal {i}: {e}"))
                })?;
                worktrees.push(Some(ws));
            }
        } else {
            worktrees.resize_with(parallel_goals.len(), || None);
        }

        let sub_dispatches: Vec<SubDispatch> = parallel_goals
            .iter()
            .enumerate()
            .map(|(i, item)| {
                let goal_text = item
                    .get("goal")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let label = item
                    .get("label")
                    .and_then(|v| v.as_str())
                    .unwrap_or(&format!("sub-{i}"))
                    .to_string();
                let allowed_mcps: Vec<String> = item
                    .get("allowed_mcps")
                    .and_then(|v| v.as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(|v| v.as_str().map(String::from))
                            .collect()
                    })
                    .unwrap_or_default();
                let success_criteria: Vec<String> = item
                    .get("success_criteria")
                    .and_then(|v| v.as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(|v| v.as_str().map(String::from))
                            .collect()
                    })
                    .unwrap_or_default();
                SubDispatch {
                    goal: goal_text,
                    allowed_mcps,
                    success_criteria,
                    correlation_id: format!("{correlation_id}-{i}"),
                    label,
                    workspace_root: worktrees[i].as_ref().map(|ws| ws.root().to_path_buf()),
                }
            })
            .collect();

        let report = pool
            .orchestrator
            .dispatch_parallel(sub_dispatches, max_concurrent)
            .await
            .map_err(|e| PackError::Failed(format!("parallel dispatch failed: {e}")))?;

        let (terminal, summary) = Disposition::Reported(report).terminal_summary();
        Ok(GoalResult {
            terminal,
            summary,
            artifacts: Vec::new(),
            diagnostics: serde_json::json!({
                "pool": Self::pool_name(goal),
                "correlation_id": correlation_id,
                "parallel": true,
            }),
        })
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

        if goal.payload.get("parallel_child").and_then(|v| v.as_bool()) == Some(true)
            && goal
                .payload
                .get("parallel_goals")
                .and_then(|v| v.as_array())
                .is_some()
        {
            return Err(PackError::Setup(
                "parallel dispatch children cannot nest further parallel goals".into(),
            ));
        }
        if let Some(parallel_goals) = goal
            .payload
            .get("parallel_goals")
            .and_then(|v| v.as_array())
            && !parallel_goals.is_empty()
        {
            return Self::run_parallel(
                pool,
                session_id,
                goal,
                &events,
                &correlation_id,
                parallel_goals,
            )
            .await;
        }

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
            // `GoalSpec::max_turns` is documented as the pack's soft turn budget with 0 meaning
            // "pack default"; this is the pack honouring it. Without this the field was accepted
            // from every caller and silently ignored.
            pool.orchestrator.run_with_turn_budget(
                decision,
                &goal.description,
                &correlation_id,
                &ctx.grant.capabilities,
                Some(goal.max_turns).filter(|t| *t > 0),
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
        Capability, CapabilitySet, Delivery, DispatchAction, DispatchDecision, Outcome,
        ProposalSigner, Report,
    };
    use liberado_config_loader::DispatchTuning;
    use liberado_executor::{RuntimeFactory, RuntimeSetupError, SUBMIT_REPORT_TOOL, ToolRuntime};
    use liberado_orchestrator::SubDispatch;
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
                delivery: Delivery::Summarize,
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
                delivery: Delivery::Summarize,
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
            repeat_calls: 0,
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

    struct MarkerRuntime {
        root: std::path::PathBuf,
        marker_name: String,
    }

    #[async_trait]
    impl ToolRuntime for MarkerRuntime {
        fn catalog(&self) -> Vec<liberado_provider::ToolDef> {
            vec![liberado_provider::ToolDef::new(
                "write_marker",
                "Write a marker file into the workspace root.",
                serde_json::json!({ "type": "object", "properties": {}, "required": [] }),
            )]
        }

        async fn invoke(&self, _call: &ToolInvocation) -> Result<String, String> {
            let path = self.root.join(format!("{}.txt", self.marker_name));
            tokio::fs::write(&path, "isolated worker wrote here")
                .await
                .map_err(|e| format!("write marker: {e}"))?;
            Ok(format!("wrote {}", path.display()))
        }
    }

    #[derive(Clone, Default)]
    struct RootedMarkerFactory {
        scoped_roots: Arc<std::sync::Mutex<Vec<std::path::PathBuf>>>,
        unscoped_calls: Arc<std::sync::Mutex<Vec<Vec<String>>>>,
    }

    #[async_trait]
    impl RuntimeFactory for RootedMarkerFactory {
        async fn runtime_for(
            &self,
            allowed_mcps: &[String],
            _provenance: WriteProvenance,
        ) -> Result<Box<dyn ToolRuntime>, RuntimeSetupError> {
            self.unscoped_calls
                .lock()
                .unwrap()
                .push(allowed_mcps.to_vec());
            Err(RuntimeSetupError(
                "the parallel path must scope runtimes via runtime_for_in".into(),
            ))
        }

        async fn runtime_for_in(
            &self,
            _allowed_mcps: &[String],
            _provenance: WriteProvenance,
            workspace_root: Option<std::path::PathBuf>,
        ) -> Result<Box<dyn ToolRuntime>, RuntimeSetupError> {
            let root = workspace_root
                .expect("an isolated worker must receive a workspace root from the caller");
            let mut roots = self.scoped_roots.lock().unwrap();
            roots.push(root.clone());
            let index = roots.len();
            Ok(Box::new(MarkerRuntime {
                root,
                marker_name: format!("worker-{index}"),
            }))
        }
    }

    fn init_parent_repo(dir: &std::path::Path) {
        let run = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .args(args)
                .current_dir(dir)
                .env("GIT_AUTHOR_NAME", "dispatch-pack-test")
                .env("GIT_AUTHOR_EMAIL", "dispatch-pack-test@local")
                .env("GIT_COMMITTER_NAME", "dispatch-pack-test")
                .env("GIT_COMMITTER_EMAIL", "dispatch-pack-test@local")
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "git {args:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        };
        run(&["init", "--quiet"]);
        run(&["config", "user.email", "dispatch-pack-test@local"]);
        run(&["config", "user.name", "dispatch-pack-test"]);
        run(&["config", "commit.gpgsign", "false"]);
        std::fs::write(dir.join("baseline.txt"), "shared baseline\n").unwrap();
        run(&["add", "baseline.txt"]);
        run(&["commit", "--quiet", "-m", "init"]);
    }

    #[tokio::test]
    async fn dispatch_parallel_runs_each_worker_in_its_own_worktree() {
        let parent = tempfile::tempdir().unwrap();
        let base = tempfile::tempdir().unwrap();
        init_parent_repo(parent.path());
        let ws_a = WorktreeWorkspace::new(
            parent.path(),
            "worker-a",
            base.path(),
            CommandPolicy::default(),
        )
        .await
        .expect("worktree A");
        let ws_b = WorktreeWorkspace::new(
            parent.path(),
            "worker-b",
            base.path(),
            CommandPolicy::default(),
        )
        .await
        .expect("worktree B");
        assert_ne!(ws_a.root(), ws_b.root());
        let root_a = ws_a.root().to_path_buf();
        let root_b = ws_b.root().to_path_buf();

        let factory = RootedMarkerFactory::default();
        let scoped_roots = factory.scoped_roots.clone();
        let unscoped_calls = factory.unscoped_calls.clone();
        let script = [
            CompletionResponse::tool_calls(vec![ToolInvocation::new(
                "c1",
                "write_marker",
                serde_json::json!({}),
            )]),
            CompletionResponse::tool_calls(vec![ToolInvocation::new(
                "c1",
                SUBMIT_REPORT_TOOL,
                serde_json::json!({
                    "outcome": "succeeded",
                    "summary": "worker wrote its isolated marker",
                    "artifacts": [],
                    "new_high_signal_facts": [],
                }),
            )]),
            CompletionResponse::tool_calls(vec![ToolInvocation::new(
                "c2",
                "write_marker",
                serde_json::json!({}),
            )]),
            CompletionResponse::tool_calls(vec![ToolInvocation::new(
                "c2",
                SUBMIT_REPORT_TOOL,
                serde_json::json!({
                    "outcome": "succeeded",
                    "summary": "worker wrote its isolated marker",
                    "artifacts": [],
                    "new_high_signal_facts": [],
                }),
            )]),
        ];
        let provider = Arc::new(MockProvider::with_script("mock", script));
        let orch = Orchestrator::new(
            provider,
            factory,
            CapabilitySet::from_iter([Capability::ExecuteTool("write_marker".into())]),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            std::env::temp_dir(),
            ProposalSigner::random(),
            DEFAULT_POOL,
        );
        let report = orch
            .dispatch_parallel(
                vec![
                    SubDispatch {
                        goal: "write your marker in your own tree".into(),
                        allowed_mcps: vec![],
                        success_criteria: vec![],
                        correlation_id: "corr-a".into(),
                        label: "A".into(),
                        workspace_root: Some(root_a.clone()),
                    },
                    SubDispatch {
                        goal: "write your marker in your own tree".into(),
                        allowed_mcps: vec![],
                        success_criteria: vec![],
                        correlation_id: "corr-b".into(),
                        label: "B".into(),
                        workspace_root: Some(root_b.clone()),
                    },
                ],
                1,
            )
            .await
            .expect("dispatch_parallel with isolated workers");
        assert_eq!(report.outcome, Outcome::Succeeded);
        assert!(report.summary.contains("worker wrote its isolated marker"));
        assert!(unscoped_calls.lock().unwrap().is_empty());
        let scoped = scoped_roots.lock().unwrap();
        assert_eq!(scoped.len(), 2);
        assert!(scoped.contains(&root_a));
        assert!(scoped.contains(&root_b));
        assert!(root_a.join("worker-1.txt").exists());
        assert!(root_b.join("worker-2.txt").exists());
        assert!(!root_a.join("worker-2.txt").exists());
        assert!(!root_b.join("worker-1.txt").exists());
    }

    #[tokio::test]
    async fn pack_refuses_nested_parallel_goals() {
        let pack = make_pack(
            DispatchDecision {
                action: DispatchAction::ExecuteDirect {
                    seed_calls: Vec::new(),
                    relevant_mcps: Vec::new(),
                    delivery: Delivery::Summarize,
                },
                confidence: 1.0,
                rationale: "unused".into(),
            },
            vec![],
            CapabilitySet::empty(),
        );
        let mut hub = GoalSessionHub::new(GoalSessionStore::new());
        hub.register_pack(Arc::new(pack));
        let hub = Arc::new(hub);
        let id = hub
            .start_with_grant(
                GoalSpec {
                    id: None,
                    description: "nested".into(),
                    success_criteria: vec![],
                    domain: DomainHint::from(DISPATCH_DOMAIN),
                    max_turns: 0,
                    max_idle_secs: None,
                    origin: None,
                    profile: None,
                    payload: serde_json::json!({
                        "parallel_child": true,
                        "parallel_goals": [{"goal": "a", "label": "A"}],
                    }),
                },
                SessionGrant::default(),
            )
            .await
            .expect("start");
        let snap = wait_terminal(&hub, &id).await;
        assert!(
            matches!(snap.session.status, SessionStatus::Failed),
            "nested fan-out must fail: {:?}",
            snap.session.status
        );
    }
}
