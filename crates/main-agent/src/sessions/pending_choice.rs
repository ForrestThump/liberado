//! The model a turn will use when a human has picked one and not yet sent it.

use liberado_conversation_store::{ChatMetaUpdate, Ulid};

use super::{ChatSessions, ConsumePending};

impl ChatSessions {
    /// In-memory pick first, then `pending_model` on the header.
    ///
    /// A consuming read clears the header field when one is set, including when the in-memory
    /// pick wins. The string that comes back is the one that was stored. This does not map it
    /// onto another model.
    pub(super) async fn pending_choice(
        &self,
        session: Ulid,
        consume: ConsumePending,
    ) -> Option<String> {
        let memory = match consume {
            ConsumePending::Yes => self.pending_model(session),
            ConsumePending::No => self.peek_pending_model(session),
        };
        if let Some(model) = usable_model(memory) {
            if matches!(consume, ConsumePending::Yes) {
                clear_stored_pending(self, session).await;
            }
            return Some(model);
        }
        let stored = self
            .store
            .header(session)
            .await
            .ok()
            .and_then(|header| header.pending_model);
        let stored = usable_model(stored);
        if stored.is_some() && matches!(consume, ConsumePending::Yes) {
            clear_stored_pending(self, session).await;
        }
        stored
    }
}

/// Blank text is not a model id. Anything else is returned unchanged.
fn usable_model(model: Option<String>) -> Option<String> {
    model.filter(|value| !value.trim().is_empty())
}

async fn clear_stored_pending(sessions: &ChatSessions, session: Ulid) {
    let current = sessions
        .store
        .header(session)
        .await
        .ok()
        .and_then(|header| header.pending_model);
    if current.is_none() {
        return;
    }
    let update = ChatMetaUpdate {
        pending_model: Some(None),
        ..ChatMetaUpdate::default()
    };
    if let Err(error) = sessions.store.set_chat_meta(session, update).await {
        tracing::warn!(%error, %session, "could not clear pending_model");
    }
}
