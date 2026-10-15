//! Session lookup for [`TextChatBridge`].
//!
//! These helpers live beside the bridge so the bridge file stays inside its
//! module-health ceiling. They are the binding the chat, cron, and slash commands share.

use liberado_conversation_store::Ulid;

use super::TextChatBridge;

#[cfg(test)]
use crate::bindings::BindingKey;
#[cfg(test)]
use crate::state::AppState;
#[cfg(test)]
use std::sync::Arc;

impl TextChatBridge {
    /// In-memory binding tests use. The label is still `Telegram`, so replies match production.
    #[cfg(test)]
    pub fn for_test(state: Arc<AppState>) -> Self {
        Self {
            state,
            bindings: crate::bindings::ChannelBindings::ephemeral(),
            key: BindingKey::telegram("test-chat", "test-bot"),
        }
    }

    pub(super) fn label(&self) -> &'static str {
        self.key.channel.label()
    }

    pub(super) async fn bound_session(&self) -> Option<Ulid> {
        self.bindings.get(&self.key).await
    }

    pub(super) async fn store_session(&self, id: Option<Ulid>) {
        self.bindings.set(&self.key, id).await;
    }

    pub(super) async fn session_or_create<F, Fut>(&self, create: F) -> Result<Ulid, String>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<Ulid, String>>,
    {
        self.bindings.get_or_create(&self.key, create).await
    }
}
