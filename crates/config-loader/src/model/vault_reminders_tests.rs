use std::path::PathBuf;

use super::super::config::Config;
use super::super::topology::{CronSchedule, Topology};
use super::VaultRemindersConfig;

fn schedule(name: &str, job: &str, enabled: bool) -> CronSchedule {
    CronSchedule {
        name: name.into(),
        enabled,
        cron_expr: "0 * * * * *".into(),
        goal: String::new(),
        pool: None,
        profile: None,
        deliver: Some(false),
        max_turns: None,
        job: Some(job.into()),
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

fn with_schedule(schedule: CronSchedule, reminders: VaultRemindersConfig) -> Config {
    let mut cfg = Config::default();
    cfg.topology.vault_path = PathBuf::from("/home/shiloh/vault");
    cfg.topology.schedules = vec![schedule];
    cfg.topology.vault_reminders = reminders;
    cfg
}

#[test]
fn reminders_default_off_and_a_missing_table_stays_off() {
    let defaults = VaultRemindersConfig::default();
    assert!(!defaults.enabled);
    assert!(defaults.tasks);
    assert!(defaults.events);
    assert!(!Topology::default().vault_reminders.enabled);

    let topology: Topology = toml::from_str("vault_path = \"/v\"\n").expect("toml");
    assert!(!topology.vault_reminders.enabled);
    assert!(topology.vault_reminders.tasks);
    assert!(topology.vault_reminders.events);

    let partial: Topology =
        toml::from_str("[vault_reminders]\nenabled = true\n").expect("partial table");
    assert!(partial.vault_reminders.enabled);
    assert!(partial.vault_reminders.tasks);
    assert!(partial.vault_reminders.events);
}

#[test]
fn an_enabled_tick_requires_the_master_switch() {
    let off = with_schedule(
        schedule("vault-reminder-tick", "vault-reminder-tick", true),
        VaultRemindersConfig::default(),
    );
    let err = off.validate().expect_err("tick while off");
    assert!(
        err.to_string()
            .contains("requires [vault_reminders] enabled = true"),
        "{err}"
    );

    let on_settings = VaultRemindersConfig {
        enabled: true,
        ..VaultRemindersConfig::default()
    };
    let on = with_schedule(
        schedule("vault-reminder-tick", "vault-reminder-tick", true),
        on_settings,
    );
    assert!(on.validate().is_ok());

    let parked = with_schedule(
        schedule("vault-reminder-tick", "vault-reminder-tick", false),
        VaultRemindersConfig::default(),
    );
    assert!(parked.validate().is_ok());
}
