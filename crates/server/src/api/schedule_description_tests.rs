use liberado_config::CronSchedule;

use super::schedule_description;

fn bare() -> CronSchedule {
    CronSchedule {
        name: "job".into(),
        enabled: true,
        cron_expr: "0 0 9 * * * *".into(),
        goal: String::new(),
        pool: None,
        profile: None,
        deliver: None,
        max_turns: None,
        job: None,
        habit_text: None,
        task_limit: None,
        capture_path: None,
        git_remote: None,
        git_branch: None,
        git_user_name: None,
        git_user_email: None,
        run_on_start: None,
        direct: None,
    }
}

#[test]
fn prompt_jobs_quote_the_profile_and_the_goal() {
    let mut schedule = bare();
    schedule.profile = Some("maintenance".into());
    schedule.goal = "Summarize inbox".into();
    assert_eq!(
        schedule_description(&schedule),
        "Runs the \"maintenance\" agent with the prompt: \"Summarize inbox\". The result is pushed to the notifier when the run finishes."
    );
}

#[test]
fn a_prompt_without_a_profile_does_not_invent_one() {
    let mut schedule = bare();
    schedule.goal = "Ping".into();
    assert_eq!(
        schedule_description(&schedule),
        "Runs an agent with the prompt: \"Ping\". The result is pushed to the notifier when the run finishes."
    );
}

#[test]
fn direct_and_deliver_false_are_stated() {
    let mut schedule = bare();
    schedule.profile = Some("ops".into());
    schedule.goal = "Ping".into();
    schedule.direct = Some(true);
    schedule.deliver = Some(false);
    assert_eq!(
        schedule_description(&schedule),
        "Skips the router and runs the \"ops\" agent with the prompt: \"Ping\". The result is not pushed to the notifier."
    );
}

#[test]
fn a_long_prompt_is_cut_and_marked() {
    let mut schedule = bare();
    schedule.goal = "word ".repeat(40);
    let text = schedule_description(&schedule);
    assert!(text.contains('…'), "{text}");
    assert!(!text.contains(&"word ".repeat(40)));
    assert!(!text.contains("Life OS"));
}

#[test]
fn an_empty_prompt_is_called_empty() {
    let schedule = bare();
    assert!(schedule_description(&schedule).contains("with an empty prompt"));
}

#[test]
fn newlines_in_a_prompt_collapse_to_spaces() {
    let mut schedule = bare();
    schedule.goal = "Summarize\ntoday's notes".into();
    assert!(
        schedule_description(&schedule).contains("with the prompt: \"Summarize today's notes\"")
    );
}

#[test]
fn event_ping_lists_the_calendar_window() {
    let mut schedule = bare();
    schedule.job = Some("event-ping".into());
    schedule.goal = "this goal is not run".into();
    let text = schedule_description(&schedule);
    assert_eq!(
        text,
        "Sends a reminder listing up to 10 calendar events from today through the next 7 days. No model session. The reminder goes to the reminder channel, not the sticky chat."
    );
    assert!(!text.contains("this goal is not run"));
}

#[test]
fn task_ping_uses_the_configured_limit() {
    let mut schedule = bare();
    schedule.job = Some("task-ping".into());
    assert!(schedule_description(&schedule).contains("up to 10 open vault tasks"));
    schedule.task_limit = Some(3);
    assert!(schedule_description(&schedule).contains("up to 3 open vault tasks"));
    schedule.task_limit = Some(0);
    assert!(schedule_description(&schedule).contains("up to 10 open vault tasks"));
}

#[test]
fn habit_ping_quotes_the_habit_text() {
    let mut schedule = bare();
    schedule.job = Some("habit-ping".into());
    schedule.habit_text = Some("Write tonight's journal.".into());
    let text = schedule_description(&schedule);
    assert!(
        text.starts_with("Sends this reminder: \"Write tonight's journal.\" No model session."),
        "{text}"
    );
    assert!(!text.contains("\"."), "{text}");
}

#[test]
fn a_goal_that_already_ends_in_punctuation_keeps_one_stop() {
    let mut schedule = bare();
    schedule.goal = "Write tonight's journal.".into();
    assert_eq!(
        schedule_description(&schedule),
        "Runs an agent with the prompt: \"Write tonight's journal.\" The result is pushed to the notifier when the run finishes."
    );

    schedule.goal = "Ship it!".into();
    assert!(
        schedule_description(&schedule)
            .contains("with the prompt: \"Ship it!\" The result is pushed")
    );

    schedule.goal = "Is the vault quiet?".into();
    assert!(
        schedule_description(&schedule)
            .contains("with the prompt: \"Is the vault quiet?\" The result is pushed")
    );

    schedule.job = Some("inbox-if-present".into());
    schedule.goal = "File the captures.".into();
    let inbox = schedule_description(&schedule);
    assert!(
        inbox.contains("with the prompt: \"File the captures.\" The result"),
        "{inbox}"
    );
    assert!(!inbox.contains("\"."), "{inbox}");

    schedule.job = Some("custom-job".into());
    schedule.goal = "Sort the inbox.".into();
    schedule.profile = None;
    let unknown = schedule_description(&schedule);
    assert!(
        unknown.contains("Prompt: \"Sort the inbox.\" No behavior"),
        "{unknown}"
    );

    schedule.job = None;
    schedule.goal = format!("word {}.", "x".repeat(200));
    let long = schedule_description(&schedule);
    assert!(long.contains('…'), "{long}");
    assert!(
        long.contains("\"."),
        "a cut goal hides its final stop, so the sentence still closes: {long}"
    );
}

#[test]
fn habit_ping_without_text_does_not_invent_a_habit() {
    let mut schedule = bare();
    schedule.job = Some("habit-ping".into());
    assert!(
        schedule_description(&schedule)
            .starts_with("Sends a habit reminder, but no habit text is set.")
    );
}

#[test]
fn reminder_tick_stays_quiet_when_nothing_is_due() {
    let mut schedule = bare();
    schedule.job = Some("vault-reminder-tick".into());
    let text = schedule_description(&schedule);
    assert!(text.contains("opted in"));
    assert!(text.contains("Stays quiet when nothing is due or reminders are off"));
    assert!(text.contains("No model session"));
}

#[test]
fn git_snapshot_names_the_remote_and_omits_identity() {
    let mut schedule = bare();
    schedule.job = Some("git-snapshot".into());
    schedule.git_remote = Some("origin".into());
    schedule.git_branch = Some("main".into());
    schedule.git_user_name = Some("Secret Name".into());
    schedule.git_user_email = Some("secret@example.com".into());
    let text = schedule_description(&schedule);
    assert!(text.contains("pushes HEAD to branch \"main\" on \"origin\""));
    assert!(text.contains("Also runs once at daemon start"));
    assert!(!text.contains("secret@example.com"));
    assert!(!text.contains("Secret Name"));
    schedule.run_on_start = Some(false);
    schedule.git_branch = None;
    schedule.git_remote = None;
    let quiet = schedule_description(&schedule);
    assert!(quiet.contains("pushes HEAD to \"origin\""));
    assert!(!quiet.contains("daemon start"));
}

#[test]
fn inbox_if_present_names_the_capture_file_and_the_prompt() {
    let mut schedule = bare();
    schedule.job = Some("inbox-if-present".into());
    schedule.goal = "File the captures".into();
    schedule.profile = Some("ops".into());
    schedule.deliver = Some(false);
    let text = schedule_description(&schedule);
    assert!(text.contains("When \"Inbox/Capture.md\" has capture text"));
    assert!(text.contains("runs the \"ops\" agent with the prompt: \"File the captures\""));
    assert!(text.contains("The result is not pushed to the notifier."));
    assert!(text.contains("When that file is empty, does nothing."));
    schedule.capture_path = Some("Inbox/Other.md".into());
    assert!(schedule_description(&schedule).contains("Inbox/Other.md"));
}

#[test]
fn an_unknown_kind_stays_neutral() {
    let mut schedule = bare();
    schedule.job = Some("inbox-filing".into());
    schedule.goal = "Sort the inbox".into();
    schedule.profile = Some("ops".into());
    schedule.git_user_email = Some("secret@example.com".into());
    let text = schedule_description(&schedule);
    assert_eq!(
        text,
        "Configured job \"inbox-filing\". Profile \"ops\". Prompt: \"Sort the inbox\". No behavior is defined for this kind."
    );
    assert!(!text.contains("secret@example.com"));
    assert!(!text.contains("files"));
}
