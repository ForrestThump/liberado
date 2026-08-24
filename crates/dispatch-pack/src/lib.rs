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

/// Route the payload's `parallel_goals`.
///
/// * `Err` — a parallel child may not nest a further fan-out (presence alone is
///   enough to reject: an empty fan-out on a child is still nesting).
/// * `Some(goals)` — a non-empty fan-out was requested; run it.
/// * `None` — nothing to fan out; the single-goal path proceeds.
fn parallel_route(goal: &GoalSpec) -> Result<Option<Vec<serde_json::Value>>, PackError> {
    let payload = &goal.payload;
    let is_child = payload.get("parallel_child").and_then(|v| v.as_bool()) == Some(true);
    let goals = payload.get("parallel_goals").and_then(|v| v.as_array());
    if is_child && goals.is_some() {
        return Err(PackError::Setup(
            "parallel dispatch children cannot nest further parallel goals".into(),
        ));
    }
    match goals {
        Some(goals) if !goals.is_empty() => Ok(Some(goals.clone())),
        _ => Ok(None),
    }
}

/// `GoalSpec::max_turns` is the pack's soft turn budget with 0 meaning "pack
/// default": any positive count reaches the orchestrator, zero collapses to
/// `None` so the default stands in.
fn effective_turn_budget(max_turns: u32) -> Option<u32> {
    Some(max_turns).filter(|t| *t > 0)
}

/// Correlation id for a dispatch run: the goal's originating correlation when
/// present, else a stable per-session fallback.
fn correlation_id_for(goal: &GoalSpec, session_id: &str) -> String {
    goal.origin
        .as_ref()
        .and_then(|o| o.correlation_id.clone())
        .unwrap_or_else(|| format!("dispatch-session-{session_id}"))
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

        let correlation_id = correlation_id_for(goal, session_id);

        if let Some(parallel_goals) = parallel_route(goal)? {
            return Self::run_parallel(
                pool,
                session_id,
                goal,
                &events,
                &correlation_id,
                &parallel_goals,
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
                effective_turn_budget(goal.max_turns),
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
#[path = "lib_tests.rs"]
mod tests;
