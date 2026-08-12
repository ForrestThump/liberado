//! Reaction path: attribute changes, dispatch, cron delivery, inline act.

use std::path::Path;

use liberado_common::{DEFAULT_POOL, Event, PROPOSALS_DIR};
use liberado_dispatcher::DispatchRequest;
use liberado_orchestrator::Disposition;
use liberado_session::{SessionGrant, TerminalKind};

use crate::helpers::{cron_schedule_name, format_cron_delivery, reaction_goal};
use crate::types::{
    Daemon, DaemonError, DaemonPool, DispatcherContext, PROPOSALS_ARCHIVE_DIR, ReactionOutcome,
};
use crate::vault_source;

impl Daemon {
    /// The pure reactive decision: given an observed change to `rel_path`, return a reactable
    /// [`Event`], or `None` if the change was one of our own writes (suppressed by the hash-join)
    /// or the path is gone. No filesystem watching here — this is the unit-testable core. A thin
    /// wrapper over [`vault_source::attribute_and_build_event`], which `VaultEventSource`'s watch
    /// loop also calls — kept as its own public method so this stays directly testable without a
    /// filesystem, as before this crate had an `EventSource` seam.
    pub async fn process_change(&self, rel_path: &Path) -> Result<Option<Event>, DaemonError> {
        Ok(
            vault_source::attribute_and_build_event(
                &self.vault,
                rel_path,
                &self.inbox_ignore_globs,
            )
            .await?,
        )
    }

    /// Take a reactable change as far as the attached components allow: observe → decide → act.
    /// Failures at any stage are logged and degrade the outcome (never abort the watch loop).
    ///
    /// Edits under `proposals/` bypass the dispatcher — they are evaluated directly as potential
    /// proposal approvals (the human's Obsidian edit is the authorization).
    pub(crate) async fn react(&self, event: &Event) -> ReactionOutcome {
        // Before any dispatch: check if this is a proposal note change. The human's edit (status
        // approval) is the authorization — no need to re-dispatch (which would re-propose).
        if let Some(path) = event.payload.path.as_deref() {
            // The path was normalized to forward slashes in build_event, so starts_with works on
            // both platforms. Exclude the exact `proposals` directory path to avoid attempting to
            // read a directory as a proposal note on directory-creation watch events. Exclude the
            // archive subtree too: archived notes are terminal by construction and must never
            // re-enter the pipeline (belt-and-suspenders — the archiving move is already suppressed
            // as a DAEMON_SOURCE write, but a human poking an archived file must be a no-op as well).
            if path.starts_with(PROPOSALS_DIR)
                && path != Path::new(PROPOSALS_DIR)
                && !path.starts_with(PROPOSALS_ARCHIVE_DIR)
            {
                // Infra errors (orchestrator runtime_for failure) are logged but degraded to
                // Observed so the watch loop never crashes. The proposal is NOT marked done when an
                // error occurs, so a human re-triggering the file (or a future retry mechanism) can
                // pick it up again.
                return self
                    .handle_proposal_change(Path::new(path))
                    .await
                    .unwrap_or_else(|e| {
                        tracing::error!(error = %e, "proposal change handling failed — not marked done, retriable");
                        ReactionOutcome::Observed
                    });
            }
        }

        // Which named pool (Decision 18 checkpoint #3) handles this event — the producer sets
        // `payload.pool` explicitly (cron/webhook); an unset pool (vault-watch, or anything that
        // doesn't opt in) routes to the always-present "default" pool.
        let pool_name = event.payload.pool.as_deref().unwrap_or(DEFAULT_POOL);
        let Some(pool) = self.pools.get(pool_name) else {
            tracing::warn!(
                pool = pool_name,
                "event names an unknown pool — observed only"
            );
            return ReactionOutcome::Observed;
        };

        let Some(ctx) = pool.dispatcher.as_ref() else {
            return ReactionOutcome::Observed; // watch-only
        };

        let mut request = ctx.dispatch_request(event);
        // Cron / webhook / wake-up: stamp local wall-clock so "today" / "this evening" means the
        // operator's zone without baking time into every system prompt. Vault-path reactions skip.
        if let Some(stamped) = self.stamp_local_time_if_needed(event, &request.goal) {
            request.goal = stamped;
        }

        // Preferred path (E3): start a hosted session on the hub. The dispatch pack classifies and
        // executes; this method returns as soon as the session exists. Reactions therefore run
        // concurrently with each other — no more awaiting one orchestrator before the next event.
        if let Some(hub) = &self.goals {
            let goal = reaction_goal(event, &request.goal, pool_name);
            // Named profile must resolve to a boot-time grant. Fail closed on unknown /
            // disabled / typo'd names — never silently widen to the full pool ceiling.
            let capabilities = match goal.profile.as_deref() {
                None => ctx.capabilities.clone(),
                Some(name) => match self.session_profile_caps.get(name) {
                    Some(caps) => caps.clone(),
                    None => {
                        tracing::warn!(
                            profile = name,
                            pool = pool_name,
                            "event names unknown or disabled session profile — not starting session"
                        );
                        return ReactionOutcome::Observed;
                    }
                },
            };
            let grant = SessionGrant {
                capabilities,
                profile: goal.profile.clone(),
                overrides: serde_json::Value::Null,
                ..Default::default()
            };
            match hub.start_background(goal, grant).await {
                Ok(session_id) => {
                    tracing::info!(
                        %session_id,
                        pool = pool_name,
                        "reaction dispatched as hosted session"
                    );
                    // A background session normally stores its summary silently. A cron firing is
                    // the exception: it exists to hand that summary *back to the human*, so deliver
                    // a cron-sourced session's result through the notifier when it finishes.
                    self.maybe_deliver_cron_result(event, &session_id);
                    return ReactionOutcome::Dispatched { session_id };
                }
                Err(e) => {
                    tracing::error!(error = %e, "failed to start reaction session on hub");
                    return ReactionOutcome::Observed;
                }
            }
        }

        // Fallback: no hub attached — inline decide/act without a session (unit tests, watch tools).
        self.dispatch_and_act(pool, ctx, &request, event).await
    }

    /// Prepend "Local time: …" for non-vault triggers when a timezone is configured.
    pub(crate) fn stamp_local_time_if_needed(&self, event: &Event, goal: &str) -> Option<String> {
        if event.payload.path.is_some() {
            return None;
        }
        let tz = self.user_timezone?;
        // Prefer the event's own timestamp (the fire instant) so a delayed reaction still
        // reports the time that mattered for the schedule, not "whenever we got around to it".
        Some(tz.with_context_at(event.timestamp, goal))
    }

    /// Deliver a cron-fired session's result to the human via the notifier.
    ///
    /// Cron is the one background source whose whole purpose is to *report back* — a morning brief
    /// nobody sees is useless. Every other background session (a `delegate`d subagent, a vault-watch
    /// reaction) folds its outcome elsewhere and must stay silent, so this is gated strictly on the
    /// `cron:` event source. No notifier configured, or a non-cron source → no-op.
    ///
    /// Spawned onto the runtime rather than awaited inline: the reaction loop must not block on a
    /// session that may run for minutes. Best-effort delivery, matching every other `notify` call —
    /// a failed send is logged, never fatal.
    ///
    /// A schedule may opt out with `deliver = false`, which rides on the event payload (the same
    /// channel `profile` uses) because this sees only the `Event`. Absent means deliver, so every
    /// schedule that predates the flag behaves exactly as before.
    pub(crate) fn maybe_deliver_cron_result(&self, event: &Event, session_id: &str) {
        let Some(schedule) = cron_schedule_name(&event.source) else {
            return;
        };
        if crate::helpers::cron_delivery_suppressed(event) {
            tracing::debug!(%schedule, %session_id, "schedule opted out of delivery");
            return;
        }
        let (Some(hub), Some(notifier)) = (self.goals.clone(), self.notifier.clone()) else {
            return;
        };
        let session_id = session_id.to_string();
        let schedule = schedule.to_string();
        tokio::spawn(async move {
            let snap = match hub.await_terminal(&session_id).await {
                Ok(snap) => snap,
                Err(e) => {
                    tracing::warn!(error = %e, schedule = %schedule, %session_id,
                        "cron session never reached terminal; nothing delivered");
                    return;
                }
            };
            let (summary, terminal) = snap
                .session
                .result
                .as_ref()
                .map(|r| (r.summary.clone(), r.terminal))
                .unwrap_or_else(|| {
                    (
                        "(the run finished with no summary)".to_string(),
                        TerminalKind::Failed,
                    )
                });
            let message = format_cron_delivery(&schedule, &summary, terminal);
            // `deliver_cron`, not `notify`: a chat-aware notifier folds this into the sticky
            // conversation and may defer it around the human's activity (see the Notifier trait).
            if let Err(e) = notifier.deliver_cron(&message).await {
                tracing::warn!(error = %e, schedule = %schedule, "cron result delivery failed");
            } else {
                tracing::info!(schedule = %schedule, %session_id, "cron result delivered");
            }
        });
    }

    /// Inline dispatch → orchestrate when no hub is attached. Prefer the hub path in production.
    pub(crate) async fn dispatch_and_act(
        &self,
        pool: &DaemonPool,
        ctx: &DispatcherContext,
        request: &DispatchRequest,
        event: &Event,
    ) -> ReactionOutcome {
        let decision = match ctx.dispatcher.dispatch(request).await {
            Ok(decision) => decision,
            Err(e) => {
                tracing::warn!(error = %e, "dispatch failed");
                return ReactionOutcome::Observed;
            }
        };

        let label = decision.action.label();

        let Some(orchestrator) = pool.orchestrator.as_ref() else {
            return ReactionOutcome::Decided(decision);
        };

        match orchestrator
            .run(
                decision.clone(),
                &request.goal,
                &event.correlation_id,
                &ctx.capabilities,
            )
            .await
        {
            Ok(Disposition::Propose(proposal)) => match self.write_proposal(&proposal).await {
                Ok(()) => ReactionOutcome::Acted(Disposition::Propose(proposal)),
                Err(e) => {
                    tracing::warn!(error = %e, "writing proposal failed");
                    ReactionOutcome::Decided(decision)
                }
            },
            Ok(disposition) => ReactionOutcome::Acted(disposition),
            Err(e) => {
                tracing::warn!(error = %e, "orchestration failed");
                let _ = label;
                ReactionOutcome::Decided(decision)
            }
        }
    }
}
