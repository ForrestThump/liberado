use std::path::PathBuf;

use super::super::config::Config;
use super::super::topology::{CronSchedule, Topology};
use super::{UnmappedPolicy, WeeklySpendingConfig, has_enabled_weekly_schedule, weekly_open_clock};

fn schedule(name: &str, job: &str, enabled: bool) -> CronSchedule {
    CronSchedule {
        name: name.into(),
        enabled,
        cron_expr: String::new(),
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
        days: vec![
            "mon".into(),
            "tue".into(),
            "wed".into(),
            "thu".into(),
            "fri".into(),
            "sat".into(),
            "sun".into(),
        ],
        at: Some("07:00".into()),
        timezone: Some("America/Chicago".into()),
    }
}

fn config_with(schedule: Option<CronSchedule>, spending: WeeklySpendingConfig) -> Config {
    let mut cfg = Config::default();
    cfg.topology.vault_path = PathBuf::from("/home/shiloh/vault");
    if let Some(row) = schedule {
        cfg.topology.schedules = vec![row];
    }
    cfg.topology.weekly_spending = spending;
    cfg
}

fn message(cfg: &Config) -> String {
    cfg.validate().expect_err("load error").to_string()
}

#[test]
fn missing_table_keeps_the_budget_defaults() {
    let defaults = WeeklySpendingConfig::default();
    assert!(!defaults.enabled);
    assert_eq!(defaults.timezone, "America/Chicago");
    assert_eq!(defaults.unmapped, UnmappedPolicy::Count);
    assert_eq!(defaults.unmapped_bucket.as_deref(), Some("Everything else"));
    assert!(defaults.validate().is_ok(), "{:?}", defaults.validate());
    assert_eq!(defaults.buckets.len(), 2);
    assert_eq!(defaults.buckets[0].name, "Groceries");
    assert_eq!(defaults.buckets[0].weekly_dollars, 240);
    assert_eq!(defaults.buckets[1].name, "Everything else");
    assert_eq!(defaults.buckets[1].weekly_dollars, 110);
    assert!(defaults.exclude.iter().any(|name| name == "Transfers"));
    assert!(defaults.exclude.iter().any(|name| name == "Rent"));
    assert!(!Topology::default().weekly_spending.enabled);

    let topology: Topology = toml::from_str("vault_path = \"/v\"\n").expect("toml");
    assert_eq!(topology.weekly_spending, defaults);
}

#[test]
fn enabled_table_without_buckets_keeps_the_defaults() {
    let topology: Topology =
        toml::from_str("[weekly_spending]\nenabled = true\n").expect("partial");
    assert!(topology.weekly_spending.enabled);
    assert_eq!(
        topology.weekly_spending.buckets,
        WeeklySpendingConfig::default().buckets
    );
    assert_eq!(
        topology.weekly_spending.exclude,
        WeeklySpendingConfig::default().exclude
    );
    assert_eq!(topology.weekly_spending.timezone, "America/Chicago");
    assert_eq!(topology.weekly_spending.unmapped, UnmappedPolicy::Count);
    assert_eq!(
        topology.weekly_spending.unmapped_bucket.as_deref(),
        Some("Everything else")
    );

    let ignored: Topology =
        toml::from_str("[weekly_spending]\nunmapped = \"ignore\"\n").expect("ignore");
    assert_eq!(ignored.weekly_spending.unmapped, UnmappedPolicy::Ignore);
    assert_eq!(
        ignored.weekly_spending.unmapped_bucket.as_deref(),
        Some("Everything else")
    );
    assert!(ignored.weekly_spending.validate().is_ok());
}

#[test]
fn explicit_empty_buckets_fail_when_enabled() {
    let topology: Topology =
        toml::from_str("[weekly_spending]\nenabled = true\nbuckets = []\n").expect("empty");
    let cfg = config_with(None, topology.weekly_spending);
    let err = message(&cfg);
    assert!(err.contains("requires at least one bucket"), "{err}");
}

#[test]
fn an_enabled_switch_needs_one_civil_schedule_in_the_same_zone() {
    let on = WeeklySpendingConfig {
        enabled: true,
        ..WeeklySpendingConfig::default()
    };
    let missing = config_with(None, on.clone());
    let err = message(&missing);
    assert!(err.contains("requires one enabled"), "{err}");

    let good = config_with(
        Some(schedule(
            "weekly-spending-push",
            "weekly-spending-push",
            true,
        )),
        on.clone(),
    );
    assert!(good.validate().is_ok(), "{:?}", good.validate());

    let mut second = schedule("weekly-spending-push-2", "weekly-spending-push", true);
    second.name = "weekly-other".into();
    let mut both = good;
    both.topology.schedules.push(second);
    let err = message(&both);
    assert!(err.contains("only one enabled"), "{err}");
}

#[test]
fn a_disabled_schedule_may_name_the_job_and_an_enabled_one_may_not() {
    let parked = config_with(
        Some(schedule(
            "weekly-spending-push",
            "weekly-spending-push",
            false,
        )),
        WeeklySpendingConfig::default(),
    );
    assert!(parked.validate().is_ok(), "{:?}", parked.validate());

    let forced = config_with(
        Some(schedule(
            "weekly-spending-push",
            "weekly-spending-push",
            true,
        )),
        WeeklySpendingConfig::default(),
    );
    let err = message(&forced);
    assert!(
        err.contains("requires [weekly_spending] enabled = true"),
        "{err}"
    );
}

#[test]
fn weekly_rows_reject_cron_and_a_different_zone_even_when_disabled() {
    let mut cron = schedule("weekly-spending-push", "weekly-spending-push", false);
    cron.cron_expr = "0 0 7 * * *".into();
    cron.days.clear();
    cron.at = None;
    let err = message(&config_with(Some(cron), WeeklySpendingConfig::default()));
    assert!(err.contains("requires days and at"), "{err}");

    let mut utc = schedule("weekly-spending-push", "weekly-spending-push", false);
    utc.timezone = Some("UTC".into());
    let err = message(&config_with(Some(utc), WeeklySpendingConfig::default()));
    assert!(
        err.contains("must match [weekly_spending].timezone"),
        "{err}"
    );
}

#[test]
fn categories_cannot_overlap_and_names_cannot_be_blank() {
    let mut overlap = WeeklySpendingConfig::default();
    overlap.buckets[1].categories.push("Groceries".into());
    let err = message(&config_with(None, overlap));
    assert!(err.contains("listed more than once"), "{err}");

    let mut excluded = WeeklySpendingConfig::default();
    excluded.buckets[0].categories.push(" Rent ".into());
    let err = message(&config_with(None, excluded));
    assert!(err.contains("exclude list"), "{err}");

    let mut blank = WeeklySpendingConfig::default();
    blank.buckets[0].name = "  ".into();
    let err = message(&config_with(None, blank));
    assert!(err.contains("must not be empty"), "{err}");

    let mut zero = WeeklySpendingConfig::default();
    zero.buckets[0].weekly_dollars = 0;
    let err = message(&config_with(None, zero));
    assert!(err.contains("greater than 0"), "{err}");

    let mut empty_category = WeeklySpendingConfig::default();
    empty_category.buckets[0].categories = vec!["  ".into()];
    let err = message(&config_with(None, empty_category));
    assert!(err.contains("empty category"), "{err}");

    let mut duplicate_name = WeeklySpendingConfig::default();
    duplicate_name.buckets[1].name = "Groceries".into();
    duplicate_name.unmapped_bucket = Some("Groceries".into());
    let err = message(&config_with(None, duplicate_name));
    assert!(
        err.contains("bucket 'Groceries' is listed more than once"),
        "{err}"
    );

    let zone = WeeklySpendingConfig {
        timezone: "Not/AZone".into(),
        ..WeeklySpendingConfig::default()
    };
    let err = message(&config_with(None, zone));
    assert!(err.contains("weekly_spending.timezone"), "{err}");
}

#[test]
fn count_needs_a_real_bucket_and_ignore_still_checks_a_named_one() {
    let mut count = WeeklySpendingConfig {
        unmapped: UnmappedPolicy::Count,
        unmapped_bucket: None,
        ..WeeklySpendingConfig::default()
    };
    let err = message(&config_with(None, count.clone()));
    assert!(err.contains("requires unmapped_bucket"), "{err}");

    count.unmapped_bucket = Some("Missing".into());
    let err = message(&config_with(None, count.clone()));
    assert!(err.contains("is not a bucket name"), "{err}");

    count.unmapped_bucket = Some(" Everything else ".into());
    assert!(config_with(None, count).validate().is_ok());

    let ignore = WeeklySpendingConfig {
        unmapped: UnmappedPolicy::Ignore,
        unmapped_bucket: Some("Nope".into()),
        ..WeeklySpendingConfig::default()
    };
    let err = message(&config_with(None, ignore));
    assert!(err.contains("is not a bucket name"), "{err}");

    let mut dropped = WeeklySpendingConfig::default();
    dropped.buckets.pop();
    let err = message(&config_with(None, dropped));
    assert!(err.contains("is not a bucket name"), "{err}");
}

#[test]
fn open_clock_comes_from_the_enabled_row() {
    let mut topology = Topology::default();
    assert_eq!(weekly_open_clock(&topology), (7, 0));
    assert!(!has_enabled_weekly_schedule(&topology));

    let mut parked = schedule("weekly-spending-push", "weekly-spending-push", false);
    parked.at = Some("09:15".into());
    topology.schedules.push(parked);
    assert_eq!(weekly_open_clock(&topology), (7, 0));

    let mut live = schedule("weekly-live", "weekly-spending-push", true);
    live.at = Some("07:30:00".into());
    topology.schedules.push(live);
    assert_eq!(weekly_open_clock(&topology), (7, 30));
    assert!(has_enabled_weekly_schedule(&topology));
}

#[test]
fn the_shipped_example_matches_the_defaults() {
    let text = include_str!("../../../../config.example/topology.toml");
    let topology: Topology = toml::from_str(text).expect("example topology");
    assert_eq!(topology.weekly_spending, WeeklySpendingConfig::default());
}
