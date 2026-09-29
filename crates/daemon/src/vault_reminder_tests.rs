use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use chrono::{NaiveDate, NaiveDateTime, Timelike};
use liberado_common::{Event, EventPayload, UserTimezone};
use liberado_config::VaultRemindersConfig;
use liberado_notify::{Notifier, NotifyError};

use super::super::remind::remind_opt_in;
use super::{ReminderTick, reminder_message};
use crate::types::{Daemon, ReactionOutcome};

fn at(hour: u32, minute: u32) -> NaiveDateTime {
    NaiveDate::from_ymd_opt(2026, 9, 29)
        .expect("date")
        .and_hms_opt(hour, minute, 0)
        .expect("time")
}

fn enabled() -> VaultRemindersConfig {
    VaultRemindersConfig {
        enabled: true,
        ..VaultRemindersConfig::default()
    }
}

fn run(
    root: &Path,
    now: NaiveDateTime,
    settings: VaultRemindersConfig,
    state: &Path,
) -> Option<String> {
    reminder_message(
        root,
        &ReminderTick {
            now,
            settings,
            state_path: state.to_path_buf(),
        },
    )
}

#[test]
fn remind_markers_accept_the_documented_forms_and_the_rightmost_wins() {
    assert!(remind_opt_in("#remind"));
    assert!(remind_opt_in("#Remind"));
    assert!(remind_opt_in("pack #remind/trash now"));
    assert!(remind_opt_in("remind:: true"));
    assert!(remind_opt_in("reminder:: yes"));
    assert!(remind_opt_in("remind: Y"));
    assert!(remind_opt_in("remind:: \"1\""));
    assert!(remind_opt_in("reminder: 'on'"));
    assert!(!remind_opt_in("no marker"));
    assert!(!remind_opt_in("#reminder"));
    assert!(!remind_opt_in("#remind-me"));
    assert!(!remind_opt_in("remind:: false"));
    assert!(!remind_opt_in("remind:: no"));
    assert!(!remind_opt_in("reminder:: n"));
    assert!(!remind_opt_in("remind: 0"));
    assert!(!remind_opt_in("remind:: off"));
    assert!(!remind_opt_in("remind:: maybe"));
    assert!(!remind_opt_in("#remind remind:: false"));
    assert!(remind_opt_in("remind:: false #remind"));
    assert!(remind_opt_in("remind:: no reminder:: yes"));
    assert!(!remind_opt_in("reminder:: yes remind:: off"));
}

#[test]
fn a_marked_task_fires_on_its_local_minute_and_an_unmarked_task_does_not() {
    let root = tempfile::tempdir().unwrap();
    let state = root.path().join("fires.json");
    std::fs::write(
        root.path().join("Home.md"),
        "\
- [ ] trash #remind 📅 2026-09-29 21:00
- [ ] bills 📅 2026-09-29 21:00
- [ ] bins remind:: true ⏳ 2026-09-29T21:00
- [ ] later #remind 📅 2026-09-29
- [ ] other #remind 📅 2026-09-29 21:05
- [x] done #remind 📅 2026-09-29 21:00
- [/] draft #remind 🛫 2026-09-29 21:00:30
",
    )
    .unwrap();
    let message = run(root.path(), at(21, 0), enabled(), &state).expect("fires");
    assert!(message.contains("trash"), "{message}");
    assert!(message.contains("due 2026-09-29 21:00"), "{message}");
    assert!(message.contains("bins"), "{message}");
    assert!(message.contains("scheduled 2026-09-29 21:00"), "{message}");
    assert!(message.contains("draft"), "{message}");
    assert!(message.contains("start 2026-09-29 21:00"), "{message}");
    assert!(!message.contains("bills"), "{message}");
    assert!(!message.contains("later"), "{message}");
    assert!(!message.contains("other"), "{message}");
    assert!(!message.contains("done"), "{message}");
}

#[test]
fn due_wins_over_a_scheduled_time_and_a_date_only_due_does_not_fall_through() {
    let root = tempfile::tempdir().unwrap();
    let state = root.path().join("fires.json");
    std::fs::write(
        root.path().join("Home.md"),
        "\
- [ ] evening #remind 📅 2026-09-29 22:00 ⏳ 2026-09-29 21:00
- [ ] digest #remind 📅 2026-09-29 ⏳ 2026-09-29 21:00
",
    )
    .unwrap();
    assert!(run(root.path(), at(21, 0), enabled(), &state).is_none());
    let message = run(root.path(), at(22, 0), enabled(), &state).expect("due minute");
    assert!(message.contains("evening"), "{message}");
    assert!(!message.contains("digest"), "{message}");
}

#[test]
fn config_off_sends_nothing_and_does_not_write_dedupe_state() {
    let root = tempfile::tempdir().unwrap();
    let state = root.path().join("fires.json");
    std::fs::write(
        root.path().join("Home.md"),
        "- [ ] trash #remind 📅 2026-09-29 21:00\n",
    )
    .unwrap();
    assert!(
        run(
            root.path(),
            at(21, 0),
            VaultRemindersConfig::default(),
            &state
        )
        .is_none()
    );
    assert!(!state.exists());
}

#[test]
fn the_same_local_minute_fires_once() {
    let root = tempfile::tempdir().unwrap();
    let state = root.path().join("state").join("fires.json");
    std::fs::write(
        root.path().join("Home.md"),
        "- [ ] trash #remind 📅 2026-09-29 21:00\n",
    )
    .unwrap();
    assert!(run(root.path(), at(21, 0), enabled(), &state).is_some());
    assert!(state.is_file());
    assert!(run(root.path(), at(21, 0), enabled(), &state).is_none());
    let next = run(root.path(), at(21, 1), enabled(), &state);
    assert!(next.is_none(), "a different minute is not this task");
}

#[test]
fn task_and_event_switches_are_independent() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join("calendar")).unwrap();
    std::fs::write(
        root.path().join("Home.md"),
        "- [ ] trash #remind 📅 2026-09-29 21:00\n",
    )
    .unwrap();
    std::fs::write(
        root.path().join("calendar/dentist.md"),
        "---\ntitle: Dentist\ndate: 2026-09-29\nstartTime: 21:00\nremind: true\n---\n",
    )
    .unwrap();
    let tasks_off = VaultRemindersConfig {
        enabled: true,
        tasks: false,
        events: true,
    };
    let message = run(
        root.path(),
        at(21, 0),
        tasks_off,
        &root.path().join("fires.json"),
    )
    .expect("event");
    assert!(message.contains("Dentist"), "{message}");
    assert!(!message.contains("trash"), "{message}");
}

#[test]
fn calendar_notes_need_a_marker_a_start_time_and_the_right_minute() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join("calendar")).unwrap();
    std::fs::write(
        root.path().join("calendar/dentist.md"),
        "---\ntitle: Dentist\ndate: 2026-09-29\nstartTime: 21:00\nendTime: 21:30\nremind: true\n---\n",
    )
    .unwrap();
    std::fs::write(
        root.path().join("calendar/quiet.md"),
        "---\ntitle: Quiet\ndate: 2026-09-29\nstartTime: 21:00\n---\n",
    )
    .unwrap();
    std::fs::write(
        root.path().join("calendar/all-day.md"),
        "---\ntitle: Holiday\ndate: 2026-09-29\nallDay: true\nremind: true\n---\n",
    )
    .unwrap();
    std::fs::write(
        root.path().join("calendar/trash.md"),
        "---\ntitle: Trash night\ntype: recurring\ndaysOfWeek: [T]\nstartTime: 21:00\n---\n#remind\n",
    )
    .unwrap();
    std::fs::write(
        root.path().join("calendar/monday.md"),
        "---\ntitle: Monday\ntype: recurring\ndaysOfWeek: [M]\nstartTime: 21:00\nreminder: true\n---\n",
    )
    .unwrap();
    std::fs::write(
        root.path().join("calendar/overridden.md"),
        "---\ntitle: Overridden\ndate: 2026-09-29\nstartTime: 21:00\nremind: true\n---\nremind:: false\n",
    )
    .unwrap();
    let message = run(
        root.path(),
        at(21, 0),
        enabled(),
        &root.path().join("fires.json"),
    )
    .expect("events");
    assert!(message.contains("Dentist"), "{message}");
    assert!(message.contains("Trash night"), "{message}");
    assert!(!message.contains("Quiet"), "{message}");
    assert!(!message.contains("Holiday"), "{message}");
    assert!(!message.contains("Monday"), "{message}");
    assert!(!message.contains("Overridden"), "{message}");
}

#[test]
fn skipped_vault_folders_do_not_fire() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join("Legal")).unwrap();
    std::fs::write(
        root.path().join("Legal/secret.md"),
        "- [ ] secret #remind 📅 2026-09-29 21:00\n",
    )
    .unwrap();
    assert!(
        run(
            root.path(),
            at(21, 0),
            enabled(),
            &root.path().join("fires.json")
        )
        .is_none()
    );
}

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

fn chicago_minute() -> (UserTimezone, NaiveDateTime) {
    let zone = UserTimezone::parse("America/Chicago").expect("zone");
    loop {
        let now = zone.now().naive_local();
        if now.time().second() < 40 {
            return (zone, now);
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

#[tokio::test]
async fn the_tick_uses_the_reminder_channel_and_not_the_sticky_chat() {
    let (zone, now) = chicago_minute();
    let root = tempfile::tempdir().unwrap();
    let state = root.path().join("fires.json");
    std::fs::write(
        root.path().join("Home.md"),
        format!(
            "- [ ] trash #remind 📅 {} {}\n",
            now.date(),
            now.format("%H:%M")
        ),
    )
    .unwrap();
    let sticky = Arc::new(Mutex::new(Vec::new()));
    let reminder = Arc::new(Mutex::new(Vec::new()));
    let daemon = Daemon::open("vault", root.path())
        .await
        .unwrap()
        .with_user_timezone(zone)
        .with_vault_reminders(enabled(), &state)
        .with_notifier(Arc::new(RecordingNotifier {
            sent: sticky.clone(),
        }))
        .with_reminder_notifier(Arc::new(RecordingNotifier {
            sent: reminder.clone(),
        }));
    let event = Event::trigger(
        "CronFired",
        "cron:vault-reminder-tick",
        "cron:vault-reminder-tick:t",
        EventPayload {
            data: serde_json::json!({"job": "vault-reminder-tick", "deliver": false}),
            ..EventPayload::default()
        },
    );
    assert!(matches!(
        daemon.handle_mechanical(&event),
        Some(ReactionOutcome::Observed)
    ));
    tokio::time::sleep(Duration::from_millis(50)).await;
    let sent = reminder.lock().unwrap().clone();
    assert!(sent.iter().any(|text| text.contains("trash")), "{sent:?}");
    assert!(sticky.lock().unwrap().is_empty());

    assert!(matches!(
        daemon.handle_mechanical(&event),
        Some(ReactionOutcome::Observed)
    ));
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(reminder.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn a_disabled_tick_does_not_ping_the_reminder_channel() {
    let (zone, now) = chicago_minute();
    let root = tempfile::tempdir().unwrap();
    std::fs::write(
        root.path().join("Home.md"),
        format!(
            "- [ ] trash #remind 📅 {} {}\n",
            now.date(),
            now.format("%H:%M")
        ),
    )
    .unwrap();
    let reminder = Arc::new(Mutex::new(Vec::new()));
    let daemon = Daemon::open("vault", root.path())
        .await
        .unwrap()
        .with_user_timezone(zone)
        .with_reminder_notifier(Arc::new(RecordingNotifier {
            sent: reminder.clone(),
        }));
    let event = Event::trigger(
        "CronFired",
        "cron:vault-reminder-tick",
        "cron:vault-reminder-tick:off",
        EventPayload {
            data: serde_json::json!({"job": "vault-reminder-tick", "deliver": false}),
            ..EventPayload::default()
        },
    );
    assert!(matches!(
        daemon.handle_mechanical(&event),
        Some(ReactionOutcome::Observed)
    ));
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(reminder.lock().unwrap().is_empty());
}
