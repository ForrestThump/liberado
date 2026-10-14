//! Permission cards sent in the room, and the edit that closes one.
//!
//! Polls are a later change. This book remembers the event id so a keyword reply and a
//! decision made on another surface can both edit the same message.

use std::sync::Mutex;

use liberado_messaging::{ActionButton, InboundEvent};

struct OpenCard {
    proposal_id: String,
    event_id: String,
    actions: Vec<String>,
}

/// Event ids of cards this process sent.
#[derive(Default)]
pub struct CardBook {
    cards: Mutex<Vec<OpenCard>>,
}

impl CardBook {
    pub fn new() -> Self {
        Self::default()
    }

    /// Remember the buttons from one `send_with_actions` call.
    pub fn remember(&self, rows: &[Vec<ActionButton>], event_id: String) {
        let Some(proposal_id) = rows
            .iter()
            .flatten()
            .next()
            .map(|button| button.correlation_id.clone())
        else {
            return;
        };
        let actions = rows
            .iter()
            .flatten()
            .map(|button| button.action.clone())
            .collect();
        let Ok(mut cards) = self.cards.lock() else {
            return;
        };
        cards.push(OpenCard {
            proposal_id,
            event_id,
            actions,
        });
    }

    /// Latest card whose action word is the whole message.
    pub fn take_keyword(&self, text: &str) -> Option<InboundEvent> {
        let word = text.trim();
        if word.is_empty() || word.split_whitespace().nth(1).is_some() {
            return None;
        }
        let mut cards = self.cards.lock().ok()?;
        let index = cards
            .iter()
            .rposition(|card| action_matches(&card.actions, word))?;
        let card = cards.remove(index);
        let action = card
            .actions
            .into_iter()
            .find(|action| action.eq_ignore_ascii_case(word))?;
        Some(InboundEvent::Action {
            action,
            correlation_id: card.proposal_id,
            event_id: "matrix-keyword".to_string(),
            message_ref: Some(card.event_id),
        })
    }

    /// Event id for `proposal_id`, removed so a later keyword is ordinary chat.
    pub fn take(&self, proposal_id: &str) -> Option<String> {
        let mut cards = self.cards.lock().ok()?;
        let index = cards
            .iter()
            .rposition(|card| card.proposal_id == proposal_id)?;
        Some(cards.remove(index).event_id)
    }
}

fn action_matches(actions: &[String], word: &str) -> bool {
    actions
        .iter()
        .any(|action| action.eq_ignore_ascii_case(word))
}

/// Text appended to a card so the owner can reply with an action word.
pub fn action_suffix(rows: &[Vec<ActionButton>]) -> String {
    let buttons: Vec<&ActionButton> = rows.iter().flatten().collect();
    if buttons.is_empty() {
        return String::new();
    }
    let mut out = String::from("\n\nReply with one word:\n");
    for button in buttons {
        out.push_str("- ");
        out.push_str(&button.action);
        out.push_str(" — ");
        out.push_str(&button.label);
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{CardBook, action_suffix};
    use liberado_messaging::{ActionButton, InboundEvent, permission_action_rows};

    #[test]
    fn the_suffix_uses_the_shared_permission_words() {
        let rows = permission_action_rows("perm-1");
        let suffix = action_suffix(&rows);
        assert!(suffix.contains("- once —"));
        assert!(suffix.contains("- session —"));
        assert!(suffix.contains("- everywhere —"));
        assert!(suffix.contains("- deny —"));
    }

    #[test]
    fn a_keyword_becomes_an_action_for_the_latest_card() {
        let book = CardBook::new();
        book.remember(&permission_action_rows("perm-1"), "$card".into());
        book.remember(
            &[vec![ActionButton::new("Approve", "approve", "other")]],
            "$other".into(),
        );
        let event = book.take_keyword("Once").expect("keyword");
        match event {
            InboundEvent::Action {
                action,
                correlation_id,
                message_ref,
                ..
            } => {
                assert_eq!(action, "once");
                assert_eq!(correlation_id, "perm-1");
                assert_eq!(message_ref.as_deref(), Some("$card"));
            }
            other => panic!("expected action, got {other:?}"),
        }
        assert!(book.take_keyword("once").is_none());
        assert_eq!(book.take("perm-1"), None);
    }
}
