//! `event-ping` is a mechanical job with no extra required fields.
//!
//! This test lives beside `builder.rs` so the schedule-kind check does not push
//! that file over its module-health cyclomatic waiver.

use std::path::PathBuf;

use crate::model::config::Config;
use crate::model::topology::CronSchedule;

fn cron_schedule(name: &str, cron_expr: &str) -> CronSchedule {
    CronSchedule {
        name: name.into(),
        enabled: true,
        cron_expr: cron_expr.into(),
        goal: "do something".into(),
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
fn event_ping_passes_with_no_extra_fields() {
    let mut cfg = Config::default();
    cfg.topology.vault_path = PathBuf::from("/home/shiloh/vault");
    let mut schedule = cron_schedule("events", "0 50 11 * * * *");
    schedule.job = Some("event-ping".into());
    schedule.goal.clear();
    cfg.topology.schedules = vec![schedule];
    assert!(cfg.validate().is_ok());
}
