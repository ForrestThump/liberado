use super::*;

fn toggle_open_task_line(content: &str, task_text: &str) -> Result<String, String> {
    match plan_task_edit(content, task_text, "2026-11-14")? {
        TaskLineOutcome::Write(text) => Ok(text),
        TaskLineOutcome::Recurring { .. } => Ok(content.to_string()),
    }
}

fn checked(note: &str, task: &str) -> String {
    match plan_task_edit(note, task, "2026-11-14").unwrap() {
        TaskLineOutcome::Write(text) => text,
        TaskLineOutcome::Recurring { identity } => panic!("left the line open: {identity}"),
    }
}

#[test]
fn empty_checkbox_does_not_match_nonempty_task() {
    let note = "- [ ]\n";
    assert!(toggle_open_task_line(note, "Call dentist").is_err());
}

#[test]
fn prefix_only_line_does_not_match_longer_task() {
    let note = "- [ ] Call\n";
    assert!(toggle_open_task_line(note, "Call dentist").is_err());
}

#[test]
fn indented_task_matches_stored_text() {
    let note = "  - [ ] Call dentist\n";
    let out = toggle_open_task_line(note, "Call dentist").unwrap();
    assert!(out.contains("- [x] Call dentist"));
}

#[test]
fn already_checked_is_idempotent() {
    let note = "- [x] Call dentist\n";
    let out = toggle_open_task_line(note, "Call dentist").unwrap();
    assert_eq!(out, note);
}

#[test]
fn ambiguous_open_lines_error() {
    let note = "- [ ] Call dentist\n- [ ] Call dentist\n";
    assert!(toggle_open_task_line(note, "Call dentist").is_err());
}

#[test]
fn text_changed_line_does_not_match() {
    let note = "- [ ] Call doctor\n";
    assert!(toggle_open_task_line(note, "Call dentist").is_err());
}

#[test]
fn metadata_after_description_still_matches() {
    let note = "- [ ] Call dentist 📅 2026-11-01\n";
    let out = toggle_open_task_line(note, "Call dentist").unwrap();
    assert!(out.contains("- [x] Call dentist"));
}

#[test]
fn ordered_list_open_task_matches() {
    let note = "1. [ ] Call dentist\n";
    let out = toggle_open_task_line(note, "Call dentist").unwrap();
    assert!(out.contains("1. [x] Call dentist"));
}

#[test]
fn star_bullet_task_matches() {
    let note = "* [ ] Call dentist\n";
    let out = toggle_open_task_line(note, "Call dentist").unwrap();
    assert!(out.contains("* [x] Call dentist"));
}

#[test]
fn a_recurring_line_is_left_unchanged_and_keeps_its_identity() {
    let note = "- [ ] Call dentist 🔁 every day\n";
    match plan_task_edit(note, "Call dentist", "2026-11-14").unwrap() {
        TaskLineOutcome::Recurring { identity } => assert_eq!(identity, "Call dentist"),
        TaskLineOutcome::Write(text) => panic!("rewrote the line: {text}"),
    }
    assert_eq!(toggle_open_task_line(note, "Call dentist").unwrap(), note);
}

#[test]
fn a_plain_task_gains_the_done_date() {
    let note = "- [ ] Call dentist\n";
    assert_eq!(
        checked(note, "Call dentist"),
        "- [x] Call dentist ✅ 2026-11-14\n"
    );
}

#[test]
fn a_dated_task_keeps_its_time_and_tags_and_gains_the_done_date() {
    let note = "- [ ] Pay bill 📅 2026-11-14 00:20 #task #remind\n";
    assert_eq!(
        checked(note, "Pay bill"),
        "- [x] Pay bill 📅 2026-11-14 00:20 #task #remind ✅ 2026-11-14\n"
    );
}

#[test]
fn the_done_date_is_placed_before_a_trailing_block_id() {
    let note = "- [ ] Pay bill 📅 2026-11-14 #task ^pay-bill\n";
    assert_eq!(
        checked(note, "Pay bill"),
        "- [x] Pay bill 📅 2026-11-14 #task ✅ 2026-11-14 ^pay-bill\n"
    );
}

#[test]
fn a_line_that_already_has_a_done_date_is_not_stamped_again() {
    let note = "- [ ] Call dentist ✅ 2026-11-01\n";
    assert_eq!(
        checked(note, "Call dentist"),
        "- [x] Call dentist ✅ 2026-11-01\n"
    );
}
