use std::process::Command;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use liberado_common::{Event, EventPayload};
use liberado_notify::{Notifier, NotifyError};

use super::{JobEffect, JobOptions, JobRequest, capture_is_empty, execute_on};
use crate::types::{Daemon, ReactionOutcome};

struct RecordingNotifier {
    sent: Arc<Mutex<Vec<String>>>,
}

#[async_trait]
impl Notifier for RecordingNotifier {
    async fn notify(&self, message: &str) -> Result<(), NotifyError> {
        self.sent.lock().expect("lock").push(message.to_string());
        Ok(())
    }
}

fn init_repo(dir: &std::path::Path) {
    let status = Command::new("git")
        .args(["init", "-b", "master"])
        .current_dir(dir)
        .status()
        .expect("git init");
    assert!(status.success());
}

fn job(kind: &str) -> JobRequest {
    JobRequest::from_options("job", kind, JobOptions::default())
}

fn idle_tick() -> crate::vault_pings::ReminderTick {
    crate::vault_pings::ReminderTick {
        now: chrono::NaiveDate::from_ymd_opt(2026, 9, 29)
            .expect("date")
            .and_hms_opt(12, 0, 0)
            .expect("time"),
        settings: liberado_config::VaultRemindersConfig::default(),
        state_path: std::path::PathBuf::from("vault-reminder-fires.json"),
    }
}

fn execute(dir: &std::path::Path, request: &JobRequest) -> JobEffect {
    let zone = liberado_common::UserTimezone::default_zone();
    execute_on(
        dir,
        request,
        zone.now().date_naive(),
        zone.iana_name(),
        &idle_tick(),
    )
}

#[test]
fn a_clean_repository_does_not_commit() {
    let dir = tempfile::tempdir().unwrap();
    init_repo(dir.path());
    std::fs::write(dir.path().join("note.md"), "hello\n").unwrap();
    let committed = Command::new("git")
        .args([
            "-c",
            "user.name=Liberado",
            "-c",
            "user.email=liberado@localhost",
            "add",
            "-A",
        ])
        .current_dir(dir.path())
        .status()
        .unwrap();
    assert!(committed.success());
    let committed = Command::new("git")
        .args([
            "-c",
            "user.name=Liberado",
            "-c",
            "user.email=liberado@localhost",
            "commit",
            "-m",
            "seed",
        ])
        .current_dir(dir.path())
        .status()
        .unwrap();
    assert!(committed.success());
    assert_eq!(execute(dir.path(), &job("git-snapshot")), JobEffect::Quiet);
    let log = Command::new("git")
        .args(["rev-list", "--count", "HEAD"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&log.stdout).trim(), "1");
}

#[test]
fn a_dirty_repository_commits_and_pushes() {
    let dir = tempfile::tempdir().unwrap();
    let bare = dir.path().join("bare.git");
    let work = dir.path().join("work");
    std::fs::create_dir_all(&work).unwrap();
    assert!(
        Command::new("git")
            .args(["init", "--bare", "-b", "master"])
            .arg(&bare)
            .status()
            .unwrap()
            .success()
    );
    init_repo(&work);
    assert!(
        Command::new("git")
            .args(["remote", "add", "origin"])
            .arg(&bare)
            .current_dir(&work)
            .status()
            .unwrap()
            .success()
    );
    std::fs::write(work.join("note.md"), "changed\n").unwrap();
    assert_eq!(execute(&work, &job("git-snapshot")), JobEffect::Quiet);
    let log = Command::new("git")
        .args(["log", "-1", "--format=%s"])
        .current_dir(&bare)
        .output()
        .unwrap();
    let subject = String::from_utf8_lossy(&log.stdout);
    assert!(subject.contains("vault snapshot"), "{subject}");
}

#[test]
fn a_failed_push_keeps_the_local_commit_and_reports_it() {
    let dir = tempfile::tempdir().unwrap();
    init_repo(dir.path());
    std::fs::write(dir.path().join("note.md"), "x\n").unwrap();
    let mut request = job("git-snapshot");
    request.git_remote = "missing-remote".into();
    let effect = execute(dir.path(), &request);
    match effect {
        JobEffect::Remind(text) => assert!(text.contains("push failed"), "{text}"),
        other => panic!("expected a reminder, got {other:?}"),
    }
    let log = Command::new("git")
        .args(["rev-list", "--count", "HEAD"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&log.stdout).trim(), "1");
}

#[test]
fn tasks_rank_by_priority_then_due_date() {
    let dir = tempfile::tempdir().unwrap();
    let tasks = dir.path().join("Tasks");
    std::fs::create_dir_all(&tasks).unwrap();
    std::fs::write(
        tasks.join("Main.md"),
        "\
- [ ] later 📅 2026-12-01
- [x] done already
- [ ] soon ⏫ 📅 2026-10-01
- [ ] medium 🔼 📅 2026-09-01
",
    )
    .unwrap();
    std::fs::create_dir_all(dir.path().join("Legal")).unwrap();
    std::fs::write(dir.path().join("Legal/secret.md"), "- [ ] locked task\n").unwrap();
    let message = match execute(dir.path(), &job("task-ping")) {
        JobEffect::Remind(text) => text,
        other => panic!("expected a list, got {other:?}"),
    };
    let soon = message.find("soon").expect(&message);
    let medium = message.find("medium").expect(&message);
    let later = message.find("later").expect(&message);
    assert!(soon < medium && medium < later, "{message}");
    assert!(!message.contains("locked task"), "{message}");
    assert!(!message.contains("done already"), "{message}");
}

#[test]
fn an_empty_capture_is_quiet_and_text_continues() {
    let dir = tempfile::tempdir().unwrap();
    let inbox = dir.path().join("Inbox");
    std::fs::create_dir_all(&inbox).unwrap();
    std::fs::write(
        inbox.join("Capture.md"),
        "---\ntitle: x\n---\n## Capture\n\n",
    )
    .unwrap();
    assert!(capture_is_empty(dir.path(), "Inbox/Capture.md"));
    assert_eq!(
        execute(dir.path(), &job("inbox-if-present")),
        JobEffect::Quiet
    );
    std::fs::write(inbox.join("Capture.md"), "## Capture\n\nbuy screws\n").unwrap();
    assert_eq!(
        execute(dir.path(), &job("inbox-if-present")),
        JobEffect::Continue
    );
}

#[tokio::test]
async fn an_event_ping_uses_the_reminder_channel_and_not_the_sticky_notifier() {
    let dir = tempfile::tempdir().unwrap();
    let calendar = dir.path().join("calendar");
    std::fs::create_dir_all(&calendar).unwrap();
    let today = liberado_common::UserTimezone::parse("America/Chicago")
        .expect("zone")
        .now()
        .date_naive();
    std::fs::write(
        calendar.join("dentist.md"),
        format!(
            "---\ntitle: Dentist\ndate: {today}\nallDay: false\nstartTime: 09:00\nendTime: 10:00\n---\n"
        ),
    )
    .unwrap();
    let sticky = Arc::new(Mutex::new(Vec::new()));
    let reminder = Arc::new(Mutex::new(Vec::new()));
    let daemon = Daemon::open("vault", dir.path())
        .await
        .unwrap()
        .with_user_timezone(liberado_common::UserTimezone::parse("America/Chicago").expect("zone"))
        .with_notifier(Arc::new(RecordingNotifier {
            sent: sticky.clone(),
        }))
        .with_reminder_notifier(Arc::new(RecordingNotifier {
            sent: reminder.clone(),
        }));
    let event = Event::trigger(
        "CronFired",
        "cron:morning-events",
        "cron:morning-events:t",
        EventPayload {
            data: serde_json::json!({"job": "event-ping", "deliver": false}),
            ..EventPayload::default()
        },
    );
    assert!(matches!(
        daemon.handle_mechanical(&event),
        Some(ReactionOutcome::Observed)
    ));
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    let reminder = reminder.lock().unwrap().clone();
    assert!(
        reminder.iter().any(|text| text.contains("Dentist")),
        "{reminder:?}"
    );
    assert!(
        reminder.iter().any(|text| text.contains("America/Chicago")),
        "{reminder:?}"
    );
    assert!(sticky.lock().unwrap().is_empty());
}

#[test]
fn event_ping_message_names_the_zone_for_a_fixed_day() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("calendar")).unwrap();
    std::fs::write(
        dir.path().join("calendar/dentist.md"),
        "---\ntitle: Dentist\ndate: 2026-09-28\nstartTime: 09:00\nendTime: 10:00\n---\n",
    )
    .unwrap();
    let today = chrono::NaiveDate::from_ymd_opt(2026, 9, 28).expect("date");
    let message = match execute_on(
        dir.path(),
        &job("event-ping"),
        today,
        "America/Chicago",
        &idle_tick(),
    ) {
        JobEffect::Remind(text) => text,
        other => panic!("expected a reminder, got {other:?}"),
    };
    assert!(message.contains("Dentist"), "{message}");
    assert!(message.contains("America/Chicago"), "{message}");
    assert!(!message.contains("No events"), "{message}");
}

#[test]
fn a_habit_ping_sends_its_text_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    let mut request = job("habit-ping");
    request.habit_text = Some("Plan the day.".into());
    assert_eq!(
        execute(dir.path(), &request),
        JobEffect::Remind("Plan the day.".into())
    );
}

#[tokio::test]
async fn a_snapshot_runs_without_a_dispatcher_and_a_habit_uses_the_reminder_channel() {
    let dir = tempfile::tempdir().unwrap();
    init_repo(dir.path());
    std::fs::write(dir.path().join("note.md"), "hello\n").unwrap();
    for args in [
        ["add", "-A", "", ""].as_slice(),
        ["commit", "-m", "seed", ""].as_slice(),
    ] {
        let args: Vec<&str> = args.iter().copied().filter(|a| !a.is_empty()).collect();
        assert!(
            Command::new("git")
                .args([
                    "-c",
                    "user.name=Liberado",
                    "-c",
                    "user.email=liberado@localhost",
                ])
                .args(args)
                .current_dir(dir.path())
                .status()
                .unwrap()
                .success()
        );
    }
    let sent = Arc::new(Mutex::new(Vec::new()));
    let daemon = Daemon::open("vault", dir.path())
        .await
        .unwrap()
        .with_reminder_notifier(Arc::new(RecordingNotifier { sent: sent.clone() }));

    let snap = Event::trigger(
        "CronFired",
        "cron:vault-snapshot",
        "cron:vault-snapshot:t",
        EventPayload {
            data: serde_json::json!({"job": "git-snapshot"}),
            ..EventPayload::default()
        },
    );
    assert!(matches!(
        daemon.handle_mechanical(&snap),
        Some(ReactionOutcome::Observed)
    ));
    let count = Command::new("git")
        .args(["rev-list", "--count", "HEAD"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&count.stdout).trim(), "1");

    let habit = Event::trigger(
        "CronFired",
        "cron:plan",
        "cron:plan:t",
        EventPayload {
            data: serde_json::json!({"job": "habit-ping", "habit_text": "Plan the day."}),
            ..EventPayload::default()
        },
    );
    assert!(daemon.handle_mechanical(&habit).is_some());
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    let messages = sent.lock().unwrap().clone();
    assert_eq!(messages, vec!["Plan the day.".to_string()]);
}
