//! Atomic insert support, split from `store.rs` for module-health boundaries.

use tokio::sync::broadcast;

use crate::goal::GoalSessionRecord;
use crate::record_store::InsertOutcome;

use super::{EVENT_CHANNEL_CAPACITY, GoalSessionStore, LogLine, SessionInner};

impl GoalSessionStore {
    /// Insert `record` only when its id is absent. Existence check and insertion share one lock.
    pub async fn insert_if_absent(&self, record: GoalSessionRecord) -> InsertOutcome {
        let id = record.id.clone();
        let mut map = self.inner.lock().await;
        if map.contains_key(&id) {
            return InsertOutcome::Existing;
        }
        self.append(
            &id,
            &LogLine::Start {
                record: Box::new(record.clone()),
            },
        );
        let (bus, _) = broadcast::channel(EVENT_CHANNEL_CAPACITY);
        map.insert(
            id,
            SessionInner {
                record,
                events: Vec::new(),
                turns: Vec::new(),
                bus,
            },
        );
        InsertOutcome::Inserted
    }
}

/// Make a session id safe as a filename (ULIDs already are; a client-supplied id might not be).
pub(super) fn sanitize_id(id: &str) -> String {
    id.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}
