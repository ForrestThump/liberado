//! Background sessions: making unattended work *visible* (session-focus S5′ step 5).
//!
//! Before this, a cron firing went straight into the dispatcher and vanished. It dispatched, it
//! orchestrated, it may have written to the vault — and left nothing behind but a log line. There
//! was no row for it in the session list, nothing to join, nothing to review afterwards. The same
//! was true of a webhook and of a `delegate`d subagent. Only work a human had personally started
//! through `/spawn` was a *session*; everything else fired into the void.
//!
//! That was never a claim the model made. D7 says a session's differences are **attributes, not
//! subtypes**, and "who started it" is just another attribute —
//! [`Visibility`](crate::Visibility). So the fix is not a new kind of thing to store; it is to
//! *record what already happens* through the store that already exists.
//!
//! [`BackgroundRun`] is that recording, as a lifecycle: open a session, note what it did, close it
//! with an outcome. It is deliberately a thin wrapper over [`SessionRecordStore`] rather than a new
//! trait — the store seam S5′ step 1 introduced is already exactly the right shape, and a second
//! trait beside it would only be a synonym.
//!
//! # What this is not
//!
//! A background run is **recorded**, not **hosted**. The daemon's dispatcher/orchestrator still
//! executes it; the hub and its [`DomainPackRunner`](crate::DomainPackRunner)s are not involved. So
//! joining one is read-only — you watch what it did, you do not steer it. Routing unattended
//! triggers through the hub as real packs is a *later* convergence and a much bigger change; this
//! slice buys the visibility without pretending to have done that.

use std::sync::Arc;

use crate::event::{SessionEvent, SessionEventKind};
use crate::goal::{
    GoalResult, GoalSessionRecord, GoalSpec, SessionGrant, SessionStatus, TerminalKind,
};
use crate::record_store::SessionRecordStore;

/// One unattended run, recorded as a session for its whole life.
///
/// Open it when the trigger fires, [`event`](Self::event) whatever it does, and
/// [`finish`](Self::finish) it with an outcome. Dropping it without finishing leaves the session
/// `Running` — which the store's replay coerces to `Failed` on the next boot, since a goal-bearing
/// session nobody is running is not honestly "running". That is the desired behavior for a crash,
/// so there is deliberately no `Drop` impl trying to be clever about it.
pub struct BackgroundRun {
    store: Arc<dyn SessionRecordStore>,
    id: String,
}

impl BackgroundRun {
    /// Open a background session for `goal` and mark it running.
    ///
    /// `grant` is the authority the run actually executes under. For a daemon reaction that is the
    /// dispatcher's pool capabilities — recorded rather than re-derived, so the transcript answers
    /// "what was this allowed to do" as well as "what did it do".
    pub async fn open(
        store: Arc<dyn SessionRecordStore>,
        goal: GoalSpec,
        grant: SessionGrant,
    ) -> Self {
        let domain = goal.domain.as_str().to_string();
        let description = goal.description.clone();
        let record = GoalSessionRecord::background(goal, grant);
        let id = record.id.clone();

        store.insert(record).await;
        store.set_status(&id, SessionStatus::Running).await;

        let run = Self { store, id };
        run.event(SessionEventKind::SessionStarted {
            domain,
            description,
        })
        .await;
        run
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    /// Append one observation to the transcript.
    pub async fn event(&self, kind: SessionEventKind) {
        self.store
            .push_event(SessionEvent::new(self.id.clone(), kind))
            .await;
    }

    /// A human-readable step. The common case for a background run, whose "transcript" is mostly a
    /// narration of decisions taken on its behalf.
    pub async fn progress(&self, message: impl Into<String>) {
        self.event(SessionEventKind::Progress {
            message: message.into(),
        })
        .await;
    }

    /// Close the session with an outcome. Records both the terminal `SessionFinished` event (what a
    /// live subscriber sees) and the terminal status + result on the header (what a *listing* sees),
    /// which are two different reads of the same fact and must not be allowed to disagree.
    pub async fn finish(self, terminal: TerminalKind, summary: impl Into<String>) {
        let summary = summary.into();
        let status = match terminal {
            TerminalKind::Succeeded => SessionStatus::Succeeded,
            TerminalKind::Failed => SessionStatus::Failed,
            TerminalKind::Cancelled => SessionStatus::Cancelled,
            TerminalKind::BudgetExhausted => SessionStatus::BudgetExhausted,
        };
        self.event(SessionEventKind::SessionFinished {
            status: format!("{status:?}").to_lowercase(),
            summary: summary.clone(),
        })
        .await;
        self.store
            .finish(
                &self.id,
                status,
                GoalResult {
                    terminal,
                    summary,
                    artifacts: Vec::new(),
                    diagnostics: serde_json::Value::Null,
                },
            )
            .await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::goal::{DomainHint, Visibility};
    use crate::store::GoalSessionStore;

    fn goal(description: &str) -> GoalSpec {
        GoalSpec {
            id: None,
            description: description.into(),
            success_criteria: vec![],
            domain: DomainHint::Custom("dispatch".into()),
            max_turns: 0,
            max_idle_secs: None,
            origin: None,
            profile: None,
            payload: serde_json::Value::Null,
        }
    }

    #[tokio::test]
    async fn a_background_run_is_recorded_as_a_background_session_from_the_start() {
        let store = Arc::new(GoalSessionStore::new());
        let run = BackgroundRun::open(
            store.clone(),
            goal("summarize today"),
            SessionGrant::default(),
        )
        .await;
        let id = run.id().to_string();

        // Visible *while running*, not only once it's over — the whole point is watching a cron work.
        let rec = store
            .get(&id)
            .await
            .expect("the run is a session immediately");
        assert_eq!(rec.visibility, Visibility::Background);
        assert_eq!(rec.status, SessionStatus::Running);

        run.progress("dispatched: execute_direct").await;
        run.finish(TerminalKind::Succeeded, "wrote the summary")
            .await;

        let rec = store.get(&id).await.unwrap();
        assert_eq!(rec.status, SessionStatus::Succeeded);
        assert_eq!(rec.result.unwrap().summary, "wrote the summary");
        // Still background after finishing — visibility is who *started* it, not a live-ness flag.
        assert_eq!(rec.visibility, Visibility::Background);

        let kinds = store.events(&id).await.unwrap();
        assert!(matches!(
            kinds.first().map(|e| &e.kind),
            Some(SessionEventKind::SessionStarted { .. })
        ));
        assert!(matches!(
            kinds.last().map(|e| &e.kind),
            Some(SessionEventKind::SessionFinished { .. })
        ));
    }

    #[tokio::test]
    async fn a_run_dropped_without_finishing_stays_running_for_replay_to_coerce() {
        // Not a bug: a crashed daemon must leave evidence a run *started*. The store's replay is
        // what turns a non-terminal goal session into `Failed` on the next boot — if this cleaned up
        // after itself, a crash mid-cron would look exactly like a cron that never fired.
        let store = Arc::new(GoalSessionStore::new());
        let run = BackgroundRun::open(store.clone(), goal("g"), SessionGrant::default()).await;
        let id = run.id().to_string();
        drop(run);

        assert_eq!(store.get(&id).await.unwrap().status, SessionStatus::Running);
    }
}
