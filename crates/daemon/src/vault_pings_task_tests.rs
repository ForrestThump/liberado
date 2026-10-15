use chrono::NaiveDate;

use super::task_ping_message;

fn day(year: i32, month: u32, date: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(year, month, date).expect("valid date")
}

fn today() -> NaiveDate {
    day(2026, 9, 28)
}

fn write_note(root: &std::path::Path, rel: &str, body: &str) {
    let path = root.join(rel);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, body).unwrap();
}

fn write_tasks_data(root: &std::path::Path, body: &str) {
    write_note(
        root,
        ".obsidian/plugins/obsidian-tasks-plugin/data.json",
        body,
    );
}

#[test]
fn task_ping_applies_the_tasks_global_filter_and_open_status() {
    let root = tempfile::tempdir().unwrap();
    write_tasks_data(
        root.path(),
        r##"{"globalFilter":"#task","removeGlobalFilter":false}"##,
    );
    write_note(
        root.path(),
        "Tasks/Home.md",
        "\
- [ ] call clinic #task 📅 2026-09-28
- [/] draft note #task 📅 2026-09-29
- [-] cancelled thing #task 📅 2026-09-29
- [x] done thing #task 📅 2026-09-29
- [ ] copy #taskforce 📅 2026-09-29
- [ ] no tag copy 📅 2026-09-29
",
    );

    let message = task_ping_message(root.path(), 10, today());
    assert!(message.contains("call clinic"), "{message}");
    assert!(message.contains("draft note"), "{message}");
    assert!(message.contains("in progress"), "{message}");
    assert!(!message.contains("cancelled thing"), "{message}");
    assert!(!message.contains("done thing"), "{message}");
    assert!(!message.contains("#taskforce"), "{message}");
    assert!(!message.contains("no tag copy"), "{message}");
}

#[test]
fn cnh_copies_without_task_tag_do_not_show_in_digest() {
    let root = tempfile::tempdir().unwrap();
    write_tasks_data(
        root.path(),
        r##"{"globalFilter":"#task","removeGlobalFilter":false}"##,
    );
    write_note(
        root.path(),
        "Life/CNH.md",
        "- [x] Look into CNH retirement account rollover #task ✅ 2026-09-12\n",
    );
    write_note(
        root.path(),
        "Journal/Weekly Review.md",
        "- [ ] Look into CNH retirement account rollover\n",
    );
    write_note(
        root.path(),
        "Briefs/Evening Debrief.md",
        "- [ ] Look into CNH retirement account rollover\n",
    );
    write_note(
        root.path(),
        "Tasks/Admin.md",
        "- [ ] Pay electric bill #task 📅 2026-10-01\n",
    );

    let message = task_ping_message(root.path(), 10, today());
    assert!(
        !message.contains("CNH retirement account rollover"),
        "{message}"
    );
    assert!(message.contains("Pay electric bill"), "{message}");
}

#[test]
fn missing_tasks_data_json_means_no_global_filter() {
    let root = tempfile::tempdir().unwrap();
    write_note(
        root.path(),
        "Tasks/Home.md",
        "\
- [ ] first open
- [ ] second open #anything
- [-] cancelled
",
    );

    let message = task_ping_message(root.path(), 10, today());
    assert!(message.contains("first open"), "{message}");
    assert!(message.contains("second open"), "{message}");
    assert!(!message.contains("cancelled"), "{message}");
}

#[test]
fn future_start_stays_hidden_and_due_clock_stays_visible() {
    let root = tempfile::tempdir().unwrap();
    write_tasks_data(
        root.path(),
        r##"{"globalFilter":"#task","removeGlobalFilter":false}"##,
    );
    write_note(
        root.path(),
        "Tasks/Home.md",
        "\
- [ ] clinic #task 📅 2026-09-28 14:00
- [ ] later #task 🛫 2026-10-01 📅 2026-10-02
- [ ] now #task 🛫 2026-09-28
",
    );

    let message = task_ping_message(root.path(), 10, today());
    assert!(
        message.contains("due 2026-09-28 at 14:00 (today)"),
        "{message}"
    );
    assert!(message.contains("now"), "{message}");
    assert!(!message.contains("later"), "{message}");
}
