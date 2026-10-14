//! Non-blocking Matrix reconcile hook.
//!
//! The server installs one hook when Matrix is on. Creating an agent, renaming
//! any chat, or deleting a chat schedules it. The hook returns immediately.
//! With no hook, those paths do nothing extra.

use std::sync::Arc;

use liberado_conversation_store::{ConversationHeader, SurfaceMode, Ulid};

use super::{ChatSessions, SessionResult};

/// `POST /api/conversations` and reconcile both use this.
///
/// An agent DM is an Agents-shelf session. The Agent Creator session is not one:
/// it is the shelf's make-an-agent chat, and it keeps no puppet.
pub fn is_agent_dm_session(header: &ConversationHeader) -> bool {
    header.surface_mode == SurfaceMode::Agent && !header.agent_creator
}

pub(super) type MatrixHook = Arc<dyn Fn() + Send + Sync>;

impl ChatSessions {
    /// Replace the hook. The server calls this once, after `Arc::new`.
    pub fn on_matrix_reconcile(&self, hook: MatrixHook) {
        let mut slot = self
            .matrix_hook
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *slot = Some(hook);
    }

    /// Run the hook when one is installed. Does not wait for Matrix.
    pub fn schedule_matrix_reconcile(&self) {
        let hook = self
            .matrix_hook
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        if let Some(hook) = hook {
            hook();
        }
    }

    /// Permanently delete a conversation. Thin passthrough: the store owns the semantics (see
    /// [`ConversationStore::delete`](liberado_conversation_store::ConversationStore::delete)) and
    /// there is no cached conversation state here to invalidate — every turn rehydrates from the
    /// store. A Matrix reconcile is scheduled after the row is gone.
    pub async fn delete(&self, id: Ulid) -> SessionResult<()> {
        self.store.delete(id).await?;
        self.schedule_matrix_reconcile();
        Ok(())
    }

    /// Set the title of a conversation. Idempotent — subsequent calls overwrite the same field.
    ///
    /// Intended writers: first-line default seed, future flash-title agent, HTTP `PATCH`,
    /// future `/title` slash command. Always overwrites; never blocked by the default seed
    /// (seed only runs when the current title is `None`). A rename schedules a Matrix reconcile
    /// so a puppet display name can follow the title.
    pub async fn set_title(&self, session: Ulid, title: String) -> SessionResult<()> {
        self.store.set_title(session, title).await?;
        self.schedule_matrix_reconcile();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use liberado_conversation_store::{ConversationHeader, SurfaceMode};

    use super::is_agent_dm_session;

    #[test]
    fn an_agent_seat_is_not_the_creator() {
        let mut header = ConversationHeader {
            surface_mode: SurfaceMode::Agent,
            ..ConversationHeader::default()
        };
        assert!(is_agent_dm_session(&header));
        header.agent_creator = true;
        assert!(!is_agent_dm_session(&header));
        header.agent_creator = false;
        header.surface_mode = SurfaceMode::Chat;
        assert!(!is_agent_dm_session(&header));
    }
}
