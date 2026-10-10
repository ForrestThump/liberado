//! Vault-zone permission choices shared by every human surface.
//!
//! Telegram's inline keyboard and the WebUI card both render this table. The ACP command
//! prompt uses a different set (it has Workspace, not Session) and does not read this module.

use super::ActionButton;

/// One choice a human can tap on a vault-zone permission request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PermissionChoice {
    /// Stable action id. The resolver accepts these four and no others.
    pub action: &'static str,
    /// Button label, emoji included. Surfaces render this string. They do not invent their own.
    pub label: &'static str,
    /// Leading emoji, also used on the decided receipt.
    pub emoji: &'static str,
    /// Short decided-state phrase: "Approved once", "Denied".
    pub decided: &'static str,
    /// Telegram keyboard row. Two rows of two, matching the historical layout.
    pub row: usize,
}

const CHOICES: [PermissionChoice; 4] = [
    PermissionChoice {
        action: "once",
        label: "✅ Once",
        emoji: "✅",
        decided: "Approved once",
        row: 0,
    },
    PermissionChoice {
        action: "session",
        label: "🔁 This session",
        emoji: "🔁",
        decided: "Approved for this session",
        row: 0,
    },
    PermissionChoice {
        action: "everywhere",
        label: "♾️ Everywhere",
        emoji: "♾️",
        decided: "Approved everywhere",
        row: 1,
    },
    PermissionChoice {
        action: "deny",
        label: "❌ Deny",
        emoji: "❌",
        decided: "Denied",
        row: 1,
    },
];

/// The four scope choices, in keyboard order: once, session, everywhere, deny.
pub fn permission_choices() -> &'static [PermissionChoice] {
    &CHOICES
}

/// One option on the WebUI card. Same action id and label as the Telegram button.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PermissionCardOption {
    pub action: &'static str,
    pub label: &'static str,
}

/// Card options in the same order as [`permission_choices`].
pub fn permission_card_options() -> Vec<PermissionCardOption> {
    permission_choices()
        .iter()
        .map(|choice| PermissionCardOption {
            action: choice.action,
            label: choice.label,
        })
        .collect()
}

/// Look up one choice by action id.
pub fn permission_choice(action: &str) -> Option<&'static PermissionChoice> {
    permission_choices()
        .iter()
        .find(|choice| choice.action == action)
}

/// Decided-state phrase for an action id. Unknown ids, `expired`, and `approved` have fallbacks
/// so a note written before this table still renders a sentence.
pub fn decided_phrase(action: &str) -> &'static str {
    if let Some(choice) = permission_choice(action) {
        return choice.decided;
    }
    match action {
        "expired" => "Expired",
        "approved" => "Approved",
        _ => "Decided",
    }
}

/// The sentence a second tap shows: `already decided: Approved once`.
pub fn already_decided_phrase(action: &str) -> String {
    format!("already decided: {}", decided_phrase(action))
}

/// Telegram inline keyboard built from [`permission_choices`].
pub fn permission_action_rows(proposal_id: &str) -> Vec<Vec<ActionButton>> {
    let mut rows = vec![Vec::new(), Vec::new()];
    for choice in permission_choices() {
        rows[choice.row].push(ActionButton::new(choice.label, choice.action, proposal_id));
    }
    rows
}
