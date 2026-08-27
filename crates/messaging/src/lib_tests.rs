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
