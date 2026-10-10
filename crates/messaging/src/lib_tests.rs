//! Split from `lib.rs` for module-health boundaries.

use super::*;

#[test]
fn approval_rows_carry_prefixed_actions() {
    let rows = approval_action_rows("prop-1");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].len(), 3);
    assert_eq!(rows[0][0].action, "approve");
    assert_eq!(rows[0][1].action, "revise");
    assert_eq!(rows[0][2].action, "reject");
    assert!(rows[0].iter().all(|b| b.correlation_id == "prop-1"));
}

#[test]
fn permission_rows_have_four_scope_buttons() {
    let rows = permission_action_rows("perm-1");
    let actions: Vec<&str> = rows.iter().flatten().map(|b| b.action.as_str()).collect();
    assert_eq!(actions, vec!["once", "session", "everywhere", "deny"]);
}

#[test]
fn telegram_rows_and_card_options_come_from_one_table() {
    let choices = permission_choices();
    let rows = permission_action_rows("perm-9");
    let buttons: Vec<&ActionButton> = rows.iter().flatten().collect();
    let cards = permission_card_options();
    assert_eq!(buttons.len(), choices.len());
    assert_eq!(cards.len(), choices.len());
    for (choice, button) in choices.iter().zip(buttons.iter()) {
        assert_eq!(button.action, choice.action);
        assert_eq!(button.label, choice.label);
        assert_eq!(button.correlation_id, "perm-9");
    }
    for (choice, card) in choices.iter().zip(cards.iter()) {
        assert_eq!(card.action, choice.action);
        assert_eq!(card.label, choice.label);
    }
    assert_eq!(decided_phrase("once"), "Approved once");
    assert_eq!(decided_phrase("deny"), "Denied");
    assert_eq!(decided_phrase("expired"), "Expired");
    assert_eq!(
        already_decided_phrase("session"),
        "already decided: Approved for this session"
    );
}
