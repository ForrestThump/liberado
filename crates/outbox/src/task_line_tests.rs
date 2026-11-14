use super::{collapse_whitespace, line_is_recurring, strip_task_metadata, task_identity_key};

#[test]
fn strip_drops_dates_priority_tags_and_the_block_id() {
    let body = "Call dentist 📅 2026-11-01 ⏫ #task ^habit";
    assert_eq!(strip_task_metadata(body), "Call dentist");
}

#[test]
fn identity_collapses_whitespace_and_ignores_metadata() {
    let dated = "- [ ] Call   dentist 📅 2026-11-01 #task";
    let plain = "- [x] Call dentist";
    assert_eq!(task_identity_key(dated).as_deref(), Some("Call dentist"));
    assert_eq!(task_identity_key(plain).as_deref(), Some("Call dentist"));
    assert_eq!(collapse_whitespace("  a \t b  "), "a b");
}

#[test]
fn a_block_id_replaces_the_text() {
    let first = "- [ ] Buy milk ^habit";
    let reworded = "- [x] Buy oat milk 📅 2026-11-02 ^habit";
    assert_eq!(task_identity_key(first).as_deref(), Some("^habit"));
    assert_eq!(task_identity_key(reworded).as_deref(), Some("^habit"));
}

#[test]
fn an_empty_checkbox_has_no_identity() {
    assert_eq!(task_identity_key("- [ ]"), None);
    assert_eq!(task_identity_key("not a task"), None);
}

#[test]
fn recurrence_is_the_emoji_on_the_raw_line() {
    assert!(line_is_recurring("- [ ] Call dentist 🔁 every day"));
    assert!(!line_is_recurring("- [ ] Call dentist"));
}
