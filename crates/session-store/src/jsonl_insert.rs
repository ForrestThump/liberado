//! Atomic insert support, split from `jsonl.rs` for module-health boundaries.

use liberado_session::{GoalSessionRecord, InsertOutcome};
use tokio::sync::broadcast;
use ulid::Ulid;

use super::{EVENT_CHANNEL_CAPACITY, Live, Record, SessionHeader, SessionStore};

impl SessionStore {
    /// Insert `record` only when its id is absent. Existence check and insertion share one lock.
    pub(super) async fn insert_record_if_absent(&self, record: GoalSessionRecord) -> InsertOutcome {
        // The kernel mints ids as `String`; honor it so a session keeps one identity end to end.
        let id = record.id.parse::<Ulid>().unwrap_or_else(|_| Ulid::new());
        let header = SessionHeader {
            id,
            title: None,
            goal: Some(record.goal.clone()),
            parent_session: record
                .goal
                .origin
                .as_ref()
                .and_then(|o| o.conversation_id.as_deref())
                .and_then(|c| c.parse::<Ulid>().ok()),
            spawned_by: None,
            correlation_id: record
                .goal
                .origin
                .as_ref()
                .and_then(|o| o.correlation_id.clone()),
            // Honor what the caller recorded. This was hardcoded `Foreground`, because the kernel's
            // record could not carry visibility at all — which is what made a background session
            // unrepresentable through the very lens every non-human trigger writes through.
            visibility: record.visibility,
            grant: record.grant.clone(),
            status: record.status,
            created_at: record.created_at,
            finished_at: record.finished_at,
            result: record.result.clone(),
            awaiting_input: record.awaiting_input,
            // A goal session is durable by construction: it is run by a pack, reported on, and
            // resumed. Incognito is a property of a human sitting at a chat surface asking for it.
            ephemeral: false,
        };
        let mut map = self.inner.lock().await;
        if map.contains_key(&id) {
            return InsertOutcome::Existing;
        }
        self.append_line(id, &Record::Header(Box::new(header.clone())));
        let (bus, _) = broadcast::channel(EVENT_CHANNEL_CAPACITY);
        map.insert(
            id,
            Live {
                header,
                nodes: Vec::new(),
                events: Vec::new(),
                bus,
            },
        );
        InsertOutcome::Inserted
    }
}
