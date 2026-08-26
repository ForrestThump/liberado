//! The inbox adapter (plan §9): which worker events deserve the delegator's attention.
//!
//! A pure mapping so both the worker's own spool and any future daemon-side adapter
//! agree on what wakes a delegator: questions, PR-ready, blocked, and honest
//! failures/cancellations. Plain progress transitions (`queued`, `running`, answers)
//! stay out — not bothering the delegator too often is a design requirement, and the
//! status poll already exists for reconciliation.

use liberado_delegate_contract::{EventKind, WorkerEvent};
use liberado_inbox_spool::{Appended, ItemKind, Spool};

/// Map one worker event to an inbox kind, or `None` for events that are not worth a
/// wake. `Note` is deliberately unreachable for now — milestone pings stay off by
/// default until someone asks for them.
pub fn inbox_kind(event: &WorkerEvent) -> Option<ItemKind> {
    match event.kind {
        EventKind::Question => Some(ItemKind::Question),
        EventKind::PrReady => Some(ItemKind::PrReady),
        // Blocked arrives as its own kind from the question-cap path; failed and
        // cancelled arrive as plain status changes and map here too — a task that died
        // is exactly what the delegator must hear about.
        EventKind::Blocked => Some(ItemKind::Blocked),
        EventKind::StatusChanged => {
            let state = event
                .payload
                .get("status")
                .and_then(|status| status.get("state"))
                .and_then(|state| state.as_str());
            match state {
                Some("failed") | Some("cancelled") | Some("blocked") => Some(ItemKind::Blocked),
                _ => None,
            }
        }
    }
}

/// Forward one event into the delegator's spool, skipping non-actionable kinds.
/// Correlation ids make redelivery harmless: the second copy lands as `Duplicate`.
pub fn forward_to_spool(
    spool: &mut Spool,
    event: &WorkerEvent,
) -> Result<Option<Appended>, liberado_inbox_spool::SpoolError> {
    match inbox_kind(event) {
        Some(kind) => Ok(Some(spool.append(
            kind,
            event.task_id.0.clone(),
            event.correlation_id.clone(),
            event.payload.clone(),
        )?)),
        None => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::inbox_kind;
    use liberado_delegate_contract::{EventKind, TaskId, WorkerEvent};

    fn event(kind: EventKind, payload: serde_json::Value) -> WorkerEvent {
        WorkerEvent {
            kind,
            correlation_id: "delegate:t:1".into(),
            task_id: TaskId("t".into()),
            payload,
        }
    }

    #[test]
    fn actionable_events_map_and_progress_noise_does_not() {
        let question = event(
            EventKind::Question,
            serde_json::json!({"question": {"id": "q"}}),
        );
        assert_eq!(
            inbox_kind(&question),
            Some(liberado_inbox_spool::ItemKind::Question)
        );

        let pr = event(EventKind::PrReady, serde_json::json!({}));
        assert_eq!(
            inbox_kind(&pr),
            Some(liberado_inbox_spool::ItemKind::PrReady)
        );

        let blocked = event(EventKind::Blocked, serde_json::json!({"reason": "cap"}));
        assert_eq!(
            inbox_kind(&blocked),
            Some(liberado_inbox_spool::ItemKind::Blocked)
        );
    }

    #[test]
    fn deaths_wake_the_delegator_lifecycle_noise_stays_silent() {
        let failed = event(
            EventKind::StatusChanged,
            serde_json::json!({"status": {"state": "failed", "detail": "boom"}}),
        );
        assert_eq!(
            inbox_kind(&failed),
            Some(liberado_inbox_spool::ItemKind::Blocked)
        );

        let cancelled = event(
            EventKind::StatusChanged,
            serde_json::json!({"status": {"state": "cancelled"}}),
        );
        assert_eq!(
            inbox_kind(&cancelled),
            Some(liberado_inbox_spool::ItemKind::Blocked)
        );

        let blocked_status = event(
            EventKind::StatusChanged,
            serde_json::json!({"status": {"state": "blocked", "detail": "cap"}}),
        );
        assert_eq!(
            inbox_kind(&blocked_status),
            Some(liberado_inbox_spool::ItemKind::Blocked)
        );

        for quiet in ["queued", "running", "pr_opened"] {
            let progress = event(
                EventKind::StatusChanged,
                serde_json::json!({"status": {"state": quiet}}),
            );
            assert_eq!(inbox_kind(&progress), None, "{quiet} must not wake anyone");
        }

        let answered = event(
            EventKind::StatusChanged,
            serde_json::json!({"answered": {}}),
        );
        assert_eq!(inbox_kind(&answered), None);
    }
}

#[cfg(test)]
mod forward_tests {
    use super::forward_to_spool;
    use liberado_delegate_contract::{Acceptance, TaskBudget, TaskGrant, TaskId, TaskSpec};
    use liberado_inbox_spool::{ItemKind, Spool};

    fn spec(id: &str) -> TaskSpec {
        TaskSpec {
            id: TaskId(id.to_string()),
            project: "p".into(),
            repository: "o/r".into(),
            base_branch: "main".into(),
            goal: "g".into(),
            success_criteria: vec![],
            acceptance: Acceptance::default(),
            budget: TaskBudget::default(),
            grant: TaskGrant::default(),
        }
    }

    /// The D2 acceptance shape: two tasks interleave events on one worker; the
    /// delegator's spool drains them FIFO by sequence, and replaying the same
    /// journals enqueues nothing twice. This is the drain a daemon activation runs.
    #[test]
    fn interleaved_tasks_drain_fifo_and_replays_dedupe() {
        let tmp = tempfile::tempdir().unwrap();
        let store = crate::queue::TaskStore::open(tmp.path().join("worker").as_path()).unwrap();
        let _ = store.submit(&spec("01AAAA")).unwrap();
        let _ = store.submit(&spec("01BBBB")).unwrap();

        // Task B asks; task A dies. Running/queued transitions stay out of the spool.
        let _running = store.mark_running(&TaskId("01BBBB".into()), "s1").unwrap();
        let _question = store
            .record_question(
                &TaskId("01BBBB".into()),
                liberado_delegate_contract::Question {
                    id: "q1".into(),
                    correlation_id: String::new(),
                    task_id: TaskId("01BBBB".into()),
                    session_id: "s1".into(),
                    body: "which?".into(),
                    options: vec![],
                    default_option: Some("a".into()),
                },
            )
            .unwrap();
        let _failed = store
            .finish(
                &TaskId("01AAAA".into()),
                liberado_delegate_contract::TaskStatus::Failed {
                    reason: "boom".into(),
                },
            )
            .unwrap();

        // Forward both journals in emission order (the adapter's real input).
        let tmp2 = tempfile::tempdir().unwrap();
        let mut spool = Spool::open(tmp2.path()).unwrap();
        for id in ["01AAAA", "01BBBB"] {
            for event in store.replay(id).unwrap() {
                let _ = forward_to_spool(&mut spool, &event).unwrap();
            }
        }
        let pending = spool.pending().unwrap();
        let kinds: Vec<ItemKind> = pending.iter().map(|i| i.kind).collect();
        assert_eq!(
            kinds,
            vec![ItemKind::Blocked, ItemKind::Question],
            "arrival order: A's death first (seq 3), then B's question (seq 4)"
        );
        assert_eq!(pending[0].correlation_id, "delegate:01AAAA:2");
        assert_eq!(
            pending[1].correlation_id, "delegate:01BBBB:3",
            "the spool item keys on the correlation the worker minted"
        );

        // Drain: settle everything, then replay the journals — nothing re-enqueues.
        for item in &pending {
            spool.settle(item.seq).unwrap();
        }
        assert_eq!(spool.pending_count().unwrap(), 0);
        for id in ["01AAAA", "01BBBB"] {
            for event in store.replay(id).unwrap() {
                let _ = forward_to_spool(&mut spool, &event).unwrap();
            }
        }
        assert_eq!(spool.pending_count().unwrap(), 0, "replay dedupes");
    }
}
