//! Channel label for decisions, and which channels keep the canned greeting.
//!
//! Telegram records `telegram` and answers `/help` itself. Matrix records `matrix` and
//! lets the shared command handler list the commands.

use liberado_common::ChannelKind;

use crate::ApprovalBot;

impl ApprovalBot {
    /// Record decisions as `channel`. Telegram is the default set in [`ApprovalBot::new`].
    #[must_use]
    pub fn with_decision_channel(mut self, channel: ChannelKind) -> Self {
        self.decision_channel = channel;
        self
    }

    /// Telegram answers `/start` and `/help` with a short greeting.
    /// A channel that defers help leaves those words for the shared command list.
    pub(crate) fn show_canned_greeting(&self, text: &str) -> bool {
        if self.channel.defer_help_to_chat() {
            return false;
        }
        text == "/start" || text == "/help"
    }
}
