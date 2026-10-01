use chrono::NaiveDate;

use super::{
    EVENT_HORIZON_DAYS, event_in_window, event_ping_message, parse_task_line, task_ping_message,
};

fn day(year: i32, month: u32, date: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(year, month, date).expect("valid date")
}

fn today() -> NaiveDate {
    day(2026, 9, 28)
}

#[test]
fn an_open_task_keeps_tasks_dates_and_a_closed_task_does_not_parse() {
    let line = "- [ ] Call the clinic ⏫ ⏳ 2026-09-27 📅 2026-09-28 🛫 2026-09-20 🔁 every week";
    let task = parse_task_line(line).expect("open task");
    assert_eq!(task.priority, 3);
    assert_eq!(task.due, Some(day(2026, 9, 28)));
    assert_eq!(task.scheduled, Some(day(2026, 9, 27)));
    assert_eq!(task.start, Some(day(2026, 9, 20)));
    assert_eq!(task.recurrence.as_deref(), Some("every week"));
    assert!(!task.in_progress);
    assert!(parse_task_line("- [x] done 📅 2026-09-28").is_none());
    assert!(parse_task_line("- [X] done too").is_none());
    assert!(parse_task_line("- [-] cancelled 📅 2026-09-28").is_none());
    assert!(parse_task_line("- [ ]").is_none());
}

#[test]
fn an_in_progress_task_parses_and_a_future_start_is_hidden() {
    let task = parse_task_line("- [/] Draft the note 📅 2026-09-28").expect("in progress");
    assert!(task.in_progress);
    let root = tempfile::tempdir().unwrap();
    std::fs::write(
        root.path().join("Tasks.md"),
        "\
- [/] Draft the note 📅 2026-09-28
- [ ] Later 🛫 2026-10-01 📅 2026-10-02
- [ ] Today start 🛫 2026-09-28
",
    )
    .unwrap();
    let message = task_ping_message(root.path(), 10, today());
    assert!(message.contains("Draft the note"), "{message}");
    assert!(message.contains("in progress"), "{message}");
    assert!(message.contains("Today start"), "{message}");
    assert!(!message.contains("Later"), "{message}");
}

#[test]
fn task_message_marks_overdue_and_today_and_keeps_priority_order() {
    let root = tempfile::tempdir().unwrap();
    let tasks = root.path().join("Tasks");
    std::fs::create_dir_all(&tasks).unwrap();
    std::fs::write(
        tasks.join("Main.md"),
        "\
- [ ] later 📅 2026-12-01
- [ ] soon 🔺 📅 2026-10-01
- [ ] medium 🔼 📅 2026-09-01 ⏳ 2026-09-01 🔁 every month
- [ ] plain 📅 2026-09-28
",
    )
    .unwrap();
    let message = task_ping_message(root.path(), 10, today());
    let soon = message.find("soon").expect(&message);
    let medium = message.find("medium").expect(&message);
    let plain = message.find("plain").expect(&message);
    let later = message.find("later").expect(&message);
    assert!(
        soon < medium && medium < plain && plain < later,
        "{message}"
    );
    assert!(message.contains("due 2026-09-01 (overdue)"), "{message}");
    assert!(message.contains("due 2026-09-28 (today)"), "{message}");
    assert!(!message.contains(" at "), "{message}");
    assert!(message.contains("scheduled 2026-09-01"), "{message}");
    assert!(message.contains("repeats every month"), "{message}");
    assert!(message.contains("Tasks/Main.md"), "{message}");
}

#[test]
fn a_due_clock_is_shown_and_a_date_only_task_stays_date_only() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(
        root.path().join("Tasks.md"),
        "\
- [ ] clinic 📅 2026-09-28 14:00
- [ ] standup 📅 2026-09-28T09:30:00
- [ ] tomorrow 📅 2026-09-29
- [ ] bill ⏳ 2026-09-28 16:05
",
    )
    .unwrap();
    let message = task_ping_message(root.path(), 10, today());
    assert!(
        message.contains("due 2026-09-28 at 14:00 (today)"),
        "{message}"
    );
    assert!(
        message.contains("due 2026-09-28 at 09:30 (today)"),
        "{message}"
    );
    assert!(message.contains("due 2026-09-29"), "{message}");
    assert!(!message.contains("due 2026-09-29 at"), "{message}");
    assert!(
        message.contains("scheduled 2026-09-28 at 16:05"),
        "{message}"
    );
}

fn write_note(root: &std::path::Path, rel: &str, body: &str) {
    let path = root.join(rel);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, body).unwrap();
}

#[test]
fn copied_open_tasks_collapse_to_the_finance_note() {
    let root = tempfile::tempdir().unwrap();
    write_note(
        root.path(),
        "Life/Systems/Finance/CNH 401k Rollover to IRA.md",
        "- [ ] Move CNH retirement account into an IRA ⏫ 📅 2026-10-22 #task #finance #retirement\n",
    );
    write_note(
        root.path(),
        "Journal/Weekly Review 2026-09-20.md",
        "- [ ] Move CNH retirement account into an IRA ⏫ 📅 2026-10-22 *(Life/Systems/Finance/CNH 401k Rollover to IRA.md)* #finance #retirement\n",
    );
    write_note(
        root.path(),
        "Journal/Weekly Review 2026-09-13.md",
        "- [ ] Move CNH retirement account into an IRA ⏫ 📅 2026-10-22 [[Life/Systems/Finance/CNH 401k Rollover to IRA]]\n",
    );
    write_note(
        root.path(),
        "Briefs/2026-09-17-Evening-Debrief.md",
        "- [ ] Move CNH 401k into an IRA — due Oct 22 ⏫ *(Life/Systems/Finance/CNH 401k Rollover to IRA.md)*\n",
    );
    write_note(
        root.path(),
        "Briefs/2026-09-19-Evening-Debrief.md",
        "- [ ] Move CNH retirement account into an IRA — **due Oct 22** ⏫ *(Life/Systems/Finance/CNH 401k Rollover to IRA.md)*\n",
    );
    write_note(
        root.path(),
        "Tasks/Admin.md",
        "- [ ] Email the plan administrator 📅 2026-10-01\n",
    );
    let message = task_ping_message(root.path(), 10, today());
    assert_eq!(message.matches("Move CNH").count(), 1, "{message}");
    assert!(
        message.contains("Life/Systems/Finance/CNH 401k Rollover to IRA.md"),
        "{message}"
    );
    assert!(
        message.contains("retirement account into an IRA"),
        "{message}"
    );
    assert!(!message.contains("401k into an IRA"), "{message}");
    assert!(!message.contains("Weekly Review"), "{message}");
    assert!(!message.contains("Evening-Debrief"), "{message}");
    assert!(
        message.contains("Email the plan administrator"),
        "{message}"
    );
    assert!(message.contains("Tasks/Admin.md"), "{message}");
}

#[test]
fn task_limit_counts_unique_tasks_after_dedupe() {
    let root = tempfile::tempdir().unwrap();
    write_note(
        root.path(),
        "Life/Chore.md",
        "- [ ] Take out the trash ⏫ 📅 2026-10-22\n",
    );
    write_note(
        root.path(),
        "Journal/Weekly Review 2026-09-20.md",
        "- [ ] Take out the trash ⏫ 📅 2026-10-22 *(Life/Chore.md)*\n",
    );
    write_note(
        root.path(),
        "Briefs/2026-09-19-Evening-Debrief.md",
        "- [ ] Take out the trash ⏫ *(Life/Chore.md)*\n",
    );
    write_note(
        root.path(),
        "Tasks/Next.md",
        "- [ ] Email the plan administrator 🔼 📅 2026-10-01\n",
    );
    write_note(
        root.path(),
        "Tasks/Later.md",
        "- [ ] Order filters 📅 2026-12-01\n",
    );
    let message = task_ping_message(root.path(), 2, today());
    assert_eq!(
        message.matches("Take out the trash").count(),
        1,
        "{message}"
    );
    assert!(message.contains("Life/Chore.md"), "{message}");
    assert!(
        message.contains("Email the plan administrator"),
        "{message}"
    );
    assert!(!message.contains("Order filters"), "{message}");
    assert!(!message.contains("Weekly Review"), "{message}");
    assert!(!message.contains("Evening-Debrief"), "{message}");
}

#[test]
fn two_near_tasks_in_one_note_stay_two_lines() {
    let root = tempfile::tempdir().unwrap();
    write_note(
        root.path(),
        "Life/Systems/Finance/CNH.md",
        "\
- [ ] Move CNH retirement account into an IRA ⏫ 📅 2026-10-22
- [ ] Move CNH retirement account into a Roth ⏫ 📅 2026-11-01
",
    );
    write_note(
        root.path(),
        "Briefs/2026-09-17-Evening-Debrief.md",
        "\
- [ ] Move CNH 401k into an IRA — due Oct 22 *(Life/Systems/Finance/CNH.md)*
- [ ] Move CNH 401k into a Roth — due Nov 1 *(Life/Systems/Finance/CNH.md)*
",
    );
    let message = task_ping_message(root.path(), 10, today());
    assert_eq!(message.matches("into an IRA").count(), 1, "{message}");
    assert_eq!(message.matches("into a Roth").count(), 1, "{message}");
    assert!(!message.contains("401k into"), "{message}");
    assert!(!message.contains("Evening-Debrief"), "{message}");
    assert_eq!(
        message.matches("Life/Systems/Finance/CNH.md").count(),
        2,
        "{message}"
    );
}

#[test]
fn a_slash_due_collapses_onto_the_emoji_date() {
    let root = tempfile::tempdir().unwrap();
    write_note(
        root.path(),
        "Life/House.md",
        "\
- [ ] Pay the water bill 📅 2026-10-22
- [ ] File the form 📅 2026-01-02
",
    );
    write_note(
        root.path(),
        "Journal/Weekly Review 2026-09-20.md",
        "\
- [ ] Pay the water bill — due 10/22
- [ ] Pay the water bill — due 10/22/26
- [ ] Pay the water bill — due 10/22/2026
- [ ] File the form — due 1/2
",
    );
    write_note(
        root.path(),
        "Tasks/Slash.md",
        "\
- [ ] Leave the long month — due 100/22
- [ ] Leave the long day — due 10/222
- [ ] Leave the letters — due ab/22
- [ ] Leave the day letters — due 10/cd
- [ ] Leave the short year — due 10/22/202
- [ ] Leave the word year — due 10/22/yy
- [ ] Leave the extra part — due 10/22/2026/1
- [ ] Clinic follow up 📅 10/22
",
    );
    let message = task_ping_message(root.path(), 20, today());
    assert_eq!(
        message.matches("Pay the water bill").count(),
        1,
        "{message}"
    );
    assert_eq!(message.matches("File the form").count(), 1, "{message}");
    assert!(message.contains("Life/House.md"), "{message}");
    assert!(!message.contains("Weekly Review"), "{message}");
    assert!(message.contains("100/22"), "{message}");
    assert!(message.contains("10/222"), "{message}");
    assert!(message.contains("ab/22"), "{message}");
    assert!(message.contains("10/cd"), "{message}");
    assert!(message.contains("10/22/202"), "{message}");
    assert!(message.contains("10/22/yy"), "{message}");
    assert!(message.contains("10/22/2026/1"), "{message}");
    assert!(message.contains("Clinic follow up"), "{message}");
    assert!(message.contains("Tasks/Slash.md"), "{message}");
}

#[test]
fn a_lowercase_wiki_link_collapses_onto_the_note() {
    let root = tempfile::tempdir().unwrap();
    write_note(
        root.path(),
        "Life/CNH.md",
        "- [ ] Move CNH retirement account into an IRA ⏫ 📅 2026-10-22\n",
    );
    write_note(
        root.path(),
        "Journal/Weekly Review 2026-09-20.md",
        "- [ ] Move CNH 401k into an IRA — due Oct 22 [[cnh]]\n",
    );
    let message = task_ping_message(root.path(), 10, today());
    assert_eq!(message.matches("into an IRA").count(), 1, "{message}");
    assert!(
        message.contains("retirement account into an IRA"),
        "{message}"
    );
    assert!(message.contains("Life/CNH.md"), "{message}");
    assert!(!message.contains("401k into an IRA"), "{message}");
    assert!(!message.contains("Weekly Review"), "{message}");
}

#[test]
fn task_limit_truncates_and_skipped_folders_stay_out() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(
        root.path().join("Tasks.md"),
        "- [ ] first ⏫\n- [ ] second 🔼\n- [ ] third\n",
    )
    .unwrap();
    std::fs::create_dir_all(root.path().join("Legal")).unwrap();
    std::fs::write(root.path().join("Legal/secret.md"), "- [ ] locked\n").unwrap();
    std::fs::create_dir_all(root.path().join(".obsidian")).unwrap();
    std::fs::write(root.path().join(".obsidian/hidden.md"), "- [ ] hidden\n").unwrap();
    let message = task_ping_message(root.path(), 2, today());
    assert!(message.contains("1. first"), "{message}");
    assert!(message.contains("2. second"), "{message}");
    assert!(!message.contains("third"), "{message}");
    assert!(!message.contains("locked"), "{message}");
    assert!(!message.contains("hidden"), "{message}");
}

#[test]
fn an_empty_vault_says_there_are_no_open_tasks() {
    let root = tempfile::tempdir().unwrap();
    assert_eq!(
        task_ping_message(root.path(), 10, today()),
        "No open tasks."
    );
}

#[test]
fn a_full_calendar_note_is_listed_and_a_dated_journal_note_is_not() {
    let root = tempfile::tempdir().unwrap();
    let calendar = root.path().join("calendar");
    std::fs::create_dir_all(&calendar).unwrap();
    std::fs::write(
        calendar.join("dentist.md"),
        "\
---
title: Dentist
allDay: false
startTime: 9:00
endTime: 10:30
date: 2026-09-28
completed: null
---
Bring the card.
",
    )
    .unwrap();
    std::fs::create_dir_all(root.path().join("Journal")).unwrap();
    std::fs::write(
        root.path().join("Journal/2026-09-28.md"),
        "---\ndate: 2026-09-28\n---\n# Tuesday\n",
    )
    .unwrap();
    std::fs::write(
        calendar.join("done.md"),
        "---\ntitle: Finished\ndate: 2026-09-28\nallDay: true\ncompleted: true\n---\n",
    )
    .unwrap();
    let message = event_ping_message(root.path(), today(), "America/Chicago");
    assert!(
        message.contains("09:00–10:30 — Dentist — calendar/dentist.md"),
        "{message}"
    );
    assert!(
        message.contains("Events through 2026-10-05 (America/Chicago)"),
        "{message}"
    );
    assert!(!message.contains("Tuesday"), "{message}");
    assert!(!message.contains("Finished"), "{message}");
}

#[test]
fn a_calendar_folder_title_is_an_all_day_event_and_the_horizon_is_inclusive() {
    let root = tempfile::tempdir().unwrap();
    let calendar = root.path().join("Calendar");
    std::fs::create_dir_all(calendar.join("2026/09")).unwrap();
    std::fs::write(
        calendar.join("2026/09/birthday.md"),
        "---\ntitle: Birthday\ndate: 2026-09-30\n---\n",
    )
    .unwrap();
    std::fs::write(
        calendar.join("edge.md"),
        "---\ntitle: Edge\ndate: 2026-10-05\nallDay: true\n---\n",
    )
    .unwrap();
    std::fs::write(
        calendar.join("past.md"),
        "---\ntitle: Past week\nallDay: true\ndate: 2026-10-06\n---\n",
    )
    .unwrap();
    let message = event_ping_message(root.path(), today(), "America/Chicago");
    assert!(
        message.contains("2026-09-30 all day — Birthday — Calendar/2026/09/birthday.md"),
        "{message}"
    );
    assert!(message.contains("Edge"), "{message}");
    assert!(!message.contains("Past week"), "{message}");
    assert_eq!(
        today()
            .checked_add_signed(chrono::Duration::days(EVENT_HORIZON_DAYS))
            .expect("horizon"),
        day(2026, 10, 5)
    );
}

#[test]
fn a_multi_day_event_that_overlaps_today_is_listed() {
    let root = tempfile::tempdir().unwrap();
    let calendar = root.path().join("calendar");
    std::fs::create_dir_all(&calendar).unwrap();
    std::fs::write(
        calendar.join("trip.md"),
        "---\ntitle: \"Trip\"\ndate: 2026-09-26\nendDate: 2026-09-28\nallDay: true\n---\n",
    )
    .unwrap();
    std::fs::write(
        calendar.join("later.md"),
        "---\ntitle: Later trip\ndate: 2026-10-10\nendDate: 2026-10-12\nallDay: true\n---\n",
    )
    .unwrap();
    let message = event_ping_message(root.path(), today(), "UTC");
    assert!(
        message.contains("2026-09-26–2026-09-28 all day — Trip"),
        "{message}"
    );
    assert!(!message.contains("Later trip"), "{message}");
}

#[test]
fn a_recurring_full_calendar_note_lists_weekdays_inside_the_window() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join("calendar")).unwrap();
    std::fs::write(
        root.path().join("calendar/standup.md"),
        "\
---
title: Standup
allDay: false
startTime: 09:00
endTime: 09:15
type: recurring
daysOfWeek: [M, W]
startRecur: 2026-09-01
endRecur: 2026-12-31
---
",
    )
    .unwrap();
    let message = event_ping_message(root.path(), today(), "America/Chicago");
    assert!(
        message.contains("2026-09-28 09:00–09:15 — Standup"),
        "{message}"
    );
    assert!(
        message.contains("2026-09-30 09:00–09:15 — Standup"),
        "{message}"
    );
    assert!(
        message.contains("2026-10-05 09:00–09:15 — Standup"),
        "{message}"
    );
    assert!(!message.contains("2026-10-07"), "{message}");
}

#[test]
fn a_heading_is_the_event_title_when_frontmatter_has_no_title() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join("calendar")).unwrap();
    std::fs::write(
        root.path().join("calendar/dentist.md"),
        "\
---
allDay: false
startTime: 9:00
endTime: 10:30
date: 2026-09-28
---
# Clinic visit
Bring the card.
",
    )
    .unwrap();
    let message = event_ping_message(root.path(), today(), "America/Chicago");
    assert!(message.contains("— Clinic visit —"), "{message}");
    assert!(message.contains("calendar/dentist.md"), "{message}");
    assert!(!message.contains("— dentist —"), "{message}");
}

#[test]
fn an_end_recur_inside_the_window_keeps_that_day_and_drops_the_next() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join("calendar")).unwrap();
    std::fs::write(
        root.path().join("calendar/standup.md"),
        "\
---
title: Standup
allDay: false
startTime: 09:00
endTime: 09:15
type: recurring
daysOfWeek: [M, W]
startRecur: 2026-09-01
endRecur: 2026-09-30
---
",
    )
    .unwrap();
    let message = event_ping_message(root.path(), today(), "America/Chicago");
    assert!(
        message.contains("2026-09-28 09:00–09:15 — Standup"),
        "{message}"
    );
    assert!(
        message.contains("2026-09-30 09:00–09:15 — Standup"),
        "{message}"
    );
    assert!(
        !message.contains("2026-10-05 09:00–09:15 — Standup"),
        "{message}"
    );
}

#[test]
fn a_start_recur_inside_the_window_drops_earlier_days() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join("calendar")).unwrap();
    std::fs::write(
        root.path().join("calendar/standup.md"),
        "\
---
title: Standup
allDay: false
startTime: 09:00
endTime: 09:15
type: recurring
daysOfWeek: [M, W]
startRecur: 2026-09-30
endRecur: 2026-12-31
---
",
    )
    .unwrap();
    let message = event_ping_message(root.path(), today(), "America/Chicago");
    assert!(
        !message.contains("2026-09-28 09:00–09:15 — Standup"),
        "{message}"
    );
    assert!(
        message.contains("2026-09-30 09:00–09:15 — Standup"),
        "{message}"
    );
    assert!(
        message.contains("2026-10-05 09:00–09:15 — Standup"),
        "{message}"
    );
}

#[test]
fn a_completed_date_and_a_false_flag_follow_the_full_calendar_field() {
    let done = "\
---
title: Done
date: 2026-09-28
allDay: true
completed: 2026-09-27
---
";
    let open = "\
---
title: Open
date: 2026-09-28
allDay: true
completed: false
---
";
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join("calendar")).unwrap();
    std::fs::write(root.path().join("calendar/done.md"), done).unwrap();
    std::fs::write(root.path().join("calendar/open.md"), open).unwrap();
    let message = event_ping_message(root.path(), today(), "America/Chicago");
    assert!(message.contains("Open"), "{message}");
    assert!(!message.contains("Done"), "{message}");
}

#[test]
fn an_event_on_the_window_edge_is_inside_and_the_next_day_is_outside() {
    let inside = super::VaultEvent {
        title: "Inside".into(),
        start: day(2026, 10, 5),
        end: day(2026, 10, 5),
        start_time: None,
        end_time: None,
        all_day: true,
        path: "calendar/inside.md".into(),
    };
    let outside = super::VaultEvent {
        start: day(2026, 10, 6),
        ..inside.clone()
    };
    assert!(event_in_window(&inside, today()));
    assert!(!event_in_window(&outside, today()));
}

#[test]
fn no_events_in_the_window_says_so() {
    let root = tempfile::tempdir().unwrap();
    let message = event_ping_message(root.path(), today(), "America/Chicago");
    assert_eq!(
        message,
        "No events today or in the next 7 days (America/Chicago)."
    );
}
