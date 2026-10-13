//! Message ids of Telegram permission cards sent by this process.
//!
//! The main bot's notifiers share one book so a card sent by the daemon and a card sent
//! for the bound chat can both be edited when another surface decides. The reminder bot
//! does not use this book.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use async_trait::async_trait;
use liberado_messaging::{ActionButton, MessagingChannel, ResolvedElsewhere};

use super::TelegramNotifier;

/// Proposal id → Telegram `message_id` for a card this process sent.
pub struct OutboundCards {
    ids: Mutex<HashMap<String, String>>,
}

impl OutboundCards {
    pub fn new() -> Self {
        Self {
            ids: Mutex::new(HashMap::new()),
        }
    }

    pub fn remember(&self, proposal_id: &str, message_id: &str) {
        if let Ok(mut ids) = self.ids.lock() {
            ids.insert(proposal_id.to_string(), message_id.to_string());
        }
    }

    pub fn take(&self, proposal_id: &str) -> Option<String> {
        self.ids.lock().ok()?.remove(proposal_id)
    }
}

impl Default for OutboundCards {
    fn default() -> Self {
        Self::new()
    }
}

pub(super) fn shared_outbound_cards() -> Arc<OutboundCards> {
    static CARDS: OnceLock<Arc<OutboundCards>> = OnceLock::new();
    Arc::clone(CARDS.get_or_init(|| Arc::new(OutboundCards::new())))
}

/// Edits a Telegram permission card when another surface records the decision.
pub struct TelegramCardUpdater {
    notifier: TelegramNotifier,
}

impl TelegramCardUpdater {
    pub fn new(notifier: TelegramNotifier) -> Self {
        Self { notifier }
    }

    /// Same card book as [`TelegramNotifier::from_env`]. `None` when Telegram is unset.
    pub fn from_env() -> Option<Self> {
        Some(Self::new(TelegramNotifier::from_env()?))
    }
}

#[async_trait]
impl ResolvedElsewhere for TelegramCardUpdater {
    fn surface_id(&self) -> &'static str {
        "telegram"
    }

    async fn on_resolved_elsewhere(&self, proposal_id: &str, receipt: &str) {
        let Some(message_id) = self.notifier.take_recorded_card(proposal_id) else {
            return;
        };
        if let Err(error) = self.notifier.edit_message(&message_id, receipt).await {
            tracing::warn!(%error, proposal_id, "telegram card update failed");
        }
    }
}

impl TelegramNotifier {
    /// Remember permission-card message ids in `cards`.
    pub fn with_cards(mut self, cards: Arc<OutboundCards>) -> Self {
        self.cards = Some(cards);
        self
    }

    /// Message id recorded for `proposal_id`, if this notifier has a card book.
    pub fn take_recorded_card(&self, proposal_id: &str) -> Option<String> {
        self.cards.as_ref()?.take(proposal_id)
    }

    pub(super) fn remember_card(&self, rows: &[Vec<ActionButton>], message_id: Option<i64>) {
        let (Some(cards), Some(message_id)) = (&self.cards, message_id) else {
            return;
        };
        let Some(proposal_id) = rows
            .iter()
            .flatten()
            .next()
            .map(|button| button.correlation_id.as_str())
        else {
            return;
        };
        cards.remember(proposal_id, &message_id.to_string());
    }
}

pub(super) fn message_id_from_body(body: &str) -> Option<i64> {
    let value: serde_json::Value = serde_json::from_str(body).ok()?;
    value.get("result")?.get("message_id")?.as_i64()
}

#[cfg(test)]
mod tests {
    use super::OutboundCards;
    use crate::TelegramNotifier;

    #[test]
    fn outbound_cards_remember_and_take_one_message_id() {
        let cards = OutboundCards::new();
        cards.remember("perm-1", "77");
        assert_eq!(cards.take("perm-1").as_deref(), Some("77"));
        assert_eq!(cards.take("perm-1"), None);
    }

    #[test]
    fn a_notifier_without_a_card_book_records_nothing() {
        let notifier = TelegramNotifier::new("tok", "1");
        assert_eq!(notifier.take_recorded_card("perm-1"), None);
    }
}
