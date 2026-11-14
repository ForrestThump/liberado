use std::path::Path;

use chrono::{DateTime, Duration, NaiveDate, NaiveDateTime, Utc};
use liberado_config::VaultRemindersConfig;
use liberado_outbox::{
    EnqueueOutcome, NewReminder, Outbox, OutboxSource, PRECISION_DATE, PRECISION_TAP,
    PRECISION_TICK, RES_COMPLETED, SOURCE_LIBERADO, SOURCE_OBSIDIAN, TaskCompletion,
    liberado_tap_completion,
};

use super::super::ReminderTick;
use super::record_task_completions;

struct World {
    _db: tempfile::TempDir,
    vault: tempfile::TempDir,
    outbox: Outbox,
}

impl World {
    fn new() -> Self {
        let db = tempfile::tempdir().unwrap();
        let outbox = Outbox::open(&db.path().join("liberado.db")).unwrap();
        Self {
            _db: db,
            vault: tempfile::tempdir().unwrap(),
            outbox,
        }
    }

    fn write(&self, rel: &str, body: &str) {
        let path = self.vault.path().join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, body).unwrap();
    }

    fn run(&self, now: NaiveDateTime) {
        self.run_with(now, true, true);
    }

    fn run_with(&self, now: NaiveDateTime, enabled: bool, completions: bool) {
        let tick = ReminderTick {
            now,
            settings: VaultRemindersConfig {
                enabled,
                completions,
                ..VaultRemindersConfig::default()
            },
            state_path: self.vault.path().join("fires.json"),
            zone: "UTC".into(),
        };
        record_task_completions(self.vault.path(), &tick, &self.outbox).unwrap();
    }

    fn rows(&self) -> Vec<TaskCompletion> {
        self.outbox.list_completions(None, 50).unwrap()
    }

    fn live(&self) -> usize {
        self.rows()
            .iter()
            .filter(|row| row.voided_at.is_none())
            .count()
    }
}

fn noon() -> NaiveDateTime {
    NaiveDate::from_ymd_opt(2026, 11, 10)
        .unwrap()
        .and_hms_opt(15, 0, 0)
        .unwrap()
}

fn detected(now: NaiveDateTime) -> DateTime<Utc> {
    now.and_utc()
}

fn resolve(outbox: &Outbox, when: DateTime<Utc>, path: &str, text: &str) -> i64 {
    let row = NewReminder {
        idempotency_key: format!("hint-{}", when.timestamp()),
        channel: "webpush".into(),
        target: "sub:1".into(),
        title: "Due".into(),
        body: "body".into(),
        due_at: when,
        catch_up: false,
        kind: Some("task_due".into()),
        source: Some(OutboxSource::VaultNote {
            path: path.into(),
            label: "due".into(),
            text: text.into(),
        }),
    };
    let EnqueueOutcome::Inserted(id) = outbox.enqueue(&row, when).unwrap() else {
        panic!("duplicate hint");
    };
    outbox.try_resolve(id, RES_COMPLETED, when).unwrap();
    id
}

#[test]
fn a_plain_checkoff_logs_one_tick_row() {
    let world = World::new();
    world.write("Home.md", "- [ ] Call dentist\n");
    world.run(noon());
    assert!(world.rows().is_empty());
    world.write("Home.md", "- [x] Call dentist\n");
    world.run(noon());
    let rows = world.rows();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].precision, PRECISION_TICK);
    assert_eq!(rows[0].source, SOURCE_OBSIDIAN);
    assert_eq!(rows[0].task_text, "Call dentist");
    assert_eq!(rows[0].path, "Home.md");
    assert!(rows[0].outbox_id.is_none());
    assert_eq!(rows[0].completed_at, "2026-11-10T15:00:00Z");
}

#[test]
fn a_tasks_rollover_logs_one_row() {
    let world = World::new();
    world.write("Home.md", "- [ ] Call dentist\n");
    world.run(noon());
    world.write(
        "Home.md",
        "- [x] Call dentist ✅ 2026-11-10\n- [ ] Call dentist\n",
    );
    world.run(noon());
    assert_eq!(world.rows().len(), 1);
    assert_eq!(world.rows()[0].done_date.as_deref(), Some("2026-11-10"));
    assert_eq!(world.rows()[0].precision, PRECISION_TICK);
}

#[test]
fn a_recent_done_hint_supplies_tap_time_and_an_old_hint_does_not() {
    let world = World::new();
    world.write("Home.md", "- [ ] Call dentist\n");
    world.run(noon());
    let id = resolve(
        &world.outbox,
        detected(noon()) - Duration::minutes(10),
        "Home.md",
        "Call dentist",
    );
    world.write("Home.md", "- [x] Call dentist\n");
    world.run(noon());
    let rows = world.rows();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].source, SOURCE_LIBERADO);
    assert_eq!(rows[0].precision, PRECISION_TAP);
    assert_eq!(rows[0].outbox_id, Some(id));
    assert_eq!(rows[0].completed_at, "2026-11-10T14:50:00Z");

    let stale = World::new();
    stale.write("Home.md", "- [ ] Call dentist\n");
    stale.run(noon());
    resolve(
        &stale.outbox,
        detected(noon()) - Duration::minutes(16),
        "Home.md",
        "Call dentist",
    );
    stale.write("Home.md", "- [x] Call dentist\n");
    stale.run(noon());
    let rows = stale.rows();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].source, SOURCE_OBSIDIAN);
    assert_eq!(rows[0].precision, PRECISION_TICK);
    assert!(rows[0].outbox_id.is_none());
}

#[test]
fn uncheck_voids_the_row_and_recheck_adds_a_new_live_row() {
    let world = World::new();
    world.write("Home.md", "- [ ] Call dentist\n");
    world.run(noon());
    world.write("Home.md", "- [x] Call dentist\n");
    world.run(noon());
    world.write("Home.md", "- [ ] Call dentist\n");
    world.run(noon());
    assert_eq!(world.live(), 0);
    assert_eq!(world.rows().len(), 1);
    assert!(world.rows()[0].voided_at.is_some());
    world.write("Home.md", "- [x] Call dentist\n");
    world.run(noon());
    assert_eq!(world.live(), 1);
    assert_eq!(world.rows().len(), 2);
}

#[test]
fn deleting_a_checked_line_leaves_the_log_alone() {
    let world = World::new();
    world.write("Home.md", "- [ ] Call dentist\n");
    world.run(noon());
    world.write("Home.md", "- [x] Call dentist\n");
    world.run(noon());
    world.write("Home.md", "No tasks.\n");
    world.run(noon());
    assert_eq!(world.live(), 1);
    assert!(world.rows()[0].voided_at.is_none());
}

#[test]
fn a_pasted_or_reworded_checked_line_logs_nothing() {
    let pasted = World::new();
    pasted.write("Home.md", "- [x] Already done\n");
    pasted.run(noon());
    assert!(pasted.rows().is_empty());
    assert_eq!(
        pasted
            .outbox
            .snapshot_counts("Home.md")
            .unwrap()
            .get("Already done")
            .map(|row| row.closed),
        Some(1)
    );

    let added = World::new();
    added.write("Home.md", "- [ ] Other\n");
    added.run(noon());
    added.write("Home.md", "- [ ] Other\n- [x] Pasted\n");
    added.run(noon());
    assert!(added.rows().is_empty());

    let reworded = World::new();
    reworded.write("Home.md", "- [ ] Alpha\n");
    reworded.run(noon());
    reworded.write("Home.md", "- [x] Beta\n");
    reworded.run(noon());
    assert!(reworded.rows().is_empty());
}

#[test]
fn the_first_sighting_seeds_the_snapshot_and_a_restart_does_not_add_rows() {
    let dir = tempfile::tempdir().unwrap();
    let vault = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("liberado.db");
    std::fs::write(vault.path().join("Home.md"), "- [ ] Call dentist\n").unwrap();
    {
        let outbox = Outbox::open(&db_path).unwrap();
        record_task_completions(vault.path(), &tick(noon()), &outbox).unwrap();
        assert!(outbox.list_completions(None, 10).unwrap().is_empty());
        assert_eq!(
            outbox
                .snapshot_counts("Home.md")
                .unwrap()
                .get("Call dentist")
                .map(|row| row.open),
            Some(1)
        );
    }
    {
        let outbox = Outbox::open(&db_path).unwrap();
        record_task_completions(vault.path(), &tick(noon()), &outbox).unwrap();
        assert!(outbox.list_completions(None, 10).unwrap().is_empty());
    }
    std::fs::write(vault.path().join("Home.md"), "- [x] Call dentist\n").unwrap();
    {
        let outbox = Outbox::open(&db_path).unwrap();
        record_task_completions(vault.path(), &tick(noon()), &outbox).unwrap();
        assert_eq!(outbox.list_completions(None, 10).unwrap().len(), 1);
    }
}

#[test]
fn an_earlier_done_date_uses_local_noon() {
    let world = World::new();
    world.write("Home.md", "- [ ] Call dentist\n");
    world.run(noon());
    world.write("Home.md", "- [x] Call dentist ✅ 2026-11-09\n");
    world.run(noon());
    let rows = world.rows();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].precision, PRECISION_DATE);
    assert_eq!(rows[0].done_date.as_deref(), Some("2026-11-09"));
    assert_eq!(rows[0].completed_at, "2026-11-09T12:00:00Z");
    assert_eq!(rows[0].local_date, "2026-11-09");
}

#[test]
fn two_identical_open_lines_log_one_row_when_one_is_checked() {
    let world = World::new();
    world.write("Home.md", "- [ ] Call dentist\n- [ ] Call dentist\n");
    world.run(noon());
    world.write("Home.md", "- [x] Call dentist\n- [ ] Call dentist\n");
    world.run(noon());
    assert_eq!(world.rows().len(), 1);
}

#[test]
fn a_block_id_keeps_one_identity_when_the_text_changes() {
    let world = World::new();
    world.write("Home.md", "- [ ] Buy milk ^habit\n");
    world.run(noon());
    world.write("Home.md", "- [x] Buy oat milk ^habit\n");
    world.run(noon());
    let rows = world.rows();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].task_text, "^habit");
}

#[test]
fn the_global_filter_rejects_untagged_tasks() {
    let world = World::new();
    world.write(
        ".obsidian/plugins/obsidian-tasks-plugin/data.json",
        "{\"globalFilter\":\"#task\"}",
    );
    world.write("Home.md", "- [ ] Nope\n- [ ] Yes #task\n");
    world.run(noon());
    world.write("Home.md", "- [x] Nope\n- [x] Yes #task\n");
    world.run(noon());
    let rows = world.rows();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].task_text, "Yes");
}

#[test]
fn skipped_directories_are_not_logged() {
    let world = World::new();
    world.write("Notes/a.md", "- [ ] Visible\n");
    world.write("Legal/b.md", "- [ ] Hidden\n");
    world.write(".trash/c.md", "- [ ] Trashed\n");
    world.run(noon());
    world.write("Notes/a.md", "- [x] Visible\n");
    world.write("Legal/b.md", "- [x] Hidden\n");
    world.write(".trash/c.md", "- [x] Trashed\n");
    world.run(noon());
    let rows = world.rows();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].path, "Notes/a.md");
}

#[test]
fn a_second_tick_in_the_same_minute_does_not_duplicate_the_row() {
    let world = World::new();
    world.write("Home.md", "- [ ] Call dentist\n");
    world.run(noon());
    world.write("Home.md", "- [x] Call dentist\n");
    world.run(noon());
    world.run(noon());
    assert_eq!(world.rows().len(), 1);
}

#[test]
fn the_log_stays_off_until_both_switches_are_on() {
    let world = World::new();
    world.write("Home.md", "- [ ] Call dentist\n");
    world.run_with(noon(), true, false);
    world.write("Home.md", "- [x] Call dentist\n");
    world.run_with(noon(), true, false);
    assert!(world.rows().is_empty());
    assert!(world.outbox.snapshot_counts("Home.md").unwrap().is_empty());

    let disabled = World::new();
    disabled.write("Home.md", "- [ ] Call dentist\n");
    disabled.run_with(noon(), false, true);
    disabled.write("Home.md", "- [x] Call dentist\n");
    disabled.run_with(noon(), false, true);
    assert!(disabled.rows().is_empty());
}

#[test]
fn a_recurring_tap_row_is_not_logged_again_while_the_line_stays_open() {
    let world = World::new();
    let original = "- [ ] Call dentist 🔁 every day\n";
    world.write("Home.md", original);
    world.run(noon());
    let row = liberado_tap_completion("Home.md", "Call dentist", 9, detected(noon()), "UTC");
    assert!(world.outbox.insert_completion(&row).unwrap());
    world.run(noon());
    assert_eq!(world.rows().len(), 1);
    assert_eq!(world.rows()[0].outbox_id, Some(9));
    let text = std::fs::read_to_string(world.vault.path().join("Home.md")).unwrap();
    assert_eq!(text, original);
}

fn tick(now: NaiveDateTime) -> ReminderTick {
    ReminderTick {
        now,
        settings: VaultRemindersConfig {
            enabled: true,
            completions: true,
            ..VaultRemindersConfig::default()
        },
        state_path: Path::new("fires.json").to_path_buf(),
        zone: "UTC".into(),
    }
}
