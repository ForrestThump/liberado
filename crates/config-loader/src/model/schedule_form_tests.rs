use super::super::config::Config;
use super::super::topology::CronSchedule;

fn base() -> Config {
    let mut cfg = Config::default();
    cfg.topology.vault_path = std::path::PathBuf::from("/home/shiloh/vault");
    cfg
}

fn named(name: &str) -> CronSchedule {
    CronSchedule {
        name: name.into(),
        enabled: true,
        cron_expr: String::new(),
        days: Vec::new(),
        at: None,
        timezone: None,
        goal: "bins".into(),
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

fn refuse(schedule: CronSchedule) -> String {
    let mut cfg = base();
    cfg.topology.schedules = vec![schedule];
    cfg.validate().unwrap_err().to_string()
}

#[test]
fn exactly_one_of_cron_or_the_structured_form() {
    let mut both = named("both");
    both.cron_expr = "0 0 9 * * * *".into();
    both.days = vec!["tue".into()];
    both.at = Some("20:00".into());
    assert!(refuse(both).contains("exactly one"));

    assert!(refuse(named("neither")).contains("exactly one"));

    let mut days_only = named("days");
    days_only.days = vec!["tue".into()];
    assert!(refuse(days_only).contains("both days and at"));

    let mut at_only = named("at");
    at_only.at = Some("20:00".into());
    assert!(refuse(at_only).contains("both days and at"));
}

#[test]
fn a_structured_row_compiles_in_the_topology_timezone() {
    let mut cfg = base();
    cfg.topology.timezone = "Europe/Paris".into();
    let mut row = named("trash");
    row.days = vec!["tue".into()];
    row.at = Some("20:00".into());
    cfg.topology.schedules = vec![row];
    cfg.validate().unwrap();
    let timing = cfg.topology.schedules[0].timing("Europe/Paris").unwrap();
    assert_eq!(timing.expr, "0 0 20 * * 3 *");
    assert_eq!(timing.civil_zone.as_deref(), Some("Europe/Paris"));
}

#[test]
fn an_explicit_timezone_and_a_zoned_cron_expression_are_civil() {
    let mut row = named("trash");
    row.days = vec!["Tuesday".into()];
    row.at = Some("20:00".into());
    row.timezone = Some("America/Chicago".into());
    let timing = row.timing("Europe/Paris").unwrap();
    assert_eq!(timing.civil_zone.as_deref(), Some("America/Chicago"));

    let mut cron = named("utc-clock");
    cron.cron_expr = "0 0 20 * * 3 *".into();
    cron.timezone = Some("America/Chicago".into());
    let zoned = cron.timing("Europe/Paris").unwrap();
    assert_eq!(zoned.expr, "0 0 20 * * 3 *");
    assert_eq!(zoned.civil_zone.as_deref(), Some("America/Chicago"));

    let mut plain = named("utc");
    plain.cron_expr = "0 0 9 * * * *".into();
    assert!(
        plain
            .timing("America/Chicago")
            .unwrap()
            .civil_zone
            .is_none()
    );
}

#[test]
fn structured_toml_round_trips_and_bad_tokens_fail_validation() {
    let raw = r#"
name = "trash"
days = ["tue"]
at = "20:00"
timezone = "America/Chicago"
goal = "bins"
"#;
    let schedule: CronSchedule = toml::from_str(raw).unwrap();
    let again: CronSchedule = toml::from_str(&toml::to_string(&schedule).unwrap()).unwrap();
    assert_eq!(again.days, vec!["tue".to_string()]);
    assert_eq!(again.at.as_deref(), Some("20:00"));
    assert_eq!(again.timezone.as_deref(), Some("America/Chicago"));
    assert!(again.cron_expr.is_empty());
    assert_eq!(
        again.timing("UTC").unwrap().civil_zone.as_deref(),
        Some("America/Chicago")
    );

    let mut bad_day = named("bad-day");
    bad_day.days = vec!["funday".into()];
    bad_day.at = Some("20:00".into());
    assert!(refuse(bad_day).contains("unknown day"));

    let mut bad_at = named("bad-at");
    bad_at.days = vec!["tue".into()];
    bad_at.at = Some("25:00".into());
    assert!(refuse(bad_at).contains("invalid time"));

    let mut bad_zone = named("bad-zone");
    bad_zone.days = vec!["tue".into()];
    bad_zone.at = Some("20:00".into());
    bad_zone.timezone = Some("Not/AZone".into());
    assert!(refuse(bad_zone).contains("timezone"));
}

#[test]
fn a_structured_row_passes_config_validation() {
    let mut cfg = base();
    let mut row = named("trash");
    row.days = vec!["tue".into()];
    row.at = Some("20:00".into());
    cfg.topology.schedules = vec![row];
    assert!(cfg.validate().is_ok());
}
