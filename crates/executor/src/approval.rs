//! Where a chat-raised permission request is shown.
//!
//! The risk gate lives in this crate. `liberado-main-agent` already depends on it, and it
//! cannot take a ninth dependency on `liberado-notify`. A chat bound to a channel sends the
//! buttons through [`PermissionSink`]. Any other chat only stamps the proposal, and the WebUI
//! card is the surface.

use async_trait::async_trait;
use liberado_common::ApprovalOrigin;

/// Session and origin stamped onto a permission proposal before it is signed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalStamp {
    pub session_id: String,
    pub origin: ApprovalOrigin,
}

/// Sends the channel scope buttons for a request raised in a bound chat.
///
/// The server implements this with the channel notifier. The risk gate calls it only when
/// [`ApprovalStamp::origin`] is [`ApprovalOrigin::Channel`].
#[async_trait]
pub trait PermissionSink: Send + Sync {
    async fn notify_permission(&self, proposal_id: &str, message: &str) -> Result<(), String>;

    /// `session_id` is the chat that raised the request, when the stamp has one.
    /// The default ignores it and calls [`Self::notify_permission`].
    async fn notify_permission_in(
        &self,
        proposal_id: &str,
        message: &str,
        _session_id: Option<&str>,
    ) -> Result<(), String> {
        self.notify_permission(proposal_id, message).await
    }
}
