//! Weekly spending reminder. Mechanical. No model.
//!
//! The table is off until `enabled = true`. An enabled schedule that names
//! [`WEEKLY_SPENDING_JOB`] is a load error while the switch is off. A later
//! midweek alert (a bucket already over, or pacing more than 25% over) is not
//! this job. Never one push per transaction.

use std::collections::HashSet;

use liberado_common::{Error, Result, UserTimezone};
use serde::{Deserialize, Serialize};

use super::topology::{CronSchedule, Topology};

mod weekly_spending_defaults;

/// Job kind the daemon runs for the weekly spending reminder.
pub const WEEKLY_SPENDING_JOB: &str = "weekly-spending-push";

/// What to do with a category that is neither in a bucket nor on the exclude list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum UnmappedPolicy {
    /// Leave the spend out of the totals. Still log the category and its amount.
    Ignore,
    /// Add the spend to [`WeeklySpendingConfig::unmapped_bucket`].
    #[default]
    Count,
}

/// One weekly budget and the category names that count toward it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpendingBucket {
    /// Shown in the push, lowercased.
    pub name: String,
    /// Whole dollars for the week. Zero is a load error.
    pub weekly_dollars: u32,
    /// Exact category names after trim. No case fold.
    pub categories: Vec<String>,
}

/// Budgets for the Monday Web Push.
///
/// A missing table uses [`Default`]. A present table that omits `buckets` or
/// `exclude` still gets those defaults. `buckets = []` is explicit and, when
/// `enabled` is true, a load error.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct WeeklySpendingConfig {
    /// Master switch. Default off.
    pub enabled: bool,
    /// IANA zone for the week window. Default `America/Chicago`.
    #[serde(default = "default_timezone")]
    pub timezone: String,
    /// Categories in neither a bucket nor `exclude`. Default count.
    #[serde(default)]
    pub unmapped: UnmappedPolicy,
    /// Bucket that receives unmapped spend when `unmapped` is `count`.
    /// Default `Everything else`. When `unmapped` is `ignore`, a set value
    /// must still name a bucket.
    #[serde(
        default = "weekly_spending_defaults::unmapped_bucket",
        skip_serializing_if = "Option::is_none"
    )]
    pub unmapped_bucket: Option<String>,
    /// Category names that never count. Transfers stay here even though the
    /// budget service already drops them.
    #[serde(default = "default_exclude")]
    pub exclude: Vec<String>,
    /// Named budgets. Defaults are Groceries and Everything else.
    #[serde(default = "default_buckets")]
    pub buckets: Vec<SpendingBucket>,
}

fn default_timezone() -> String {
    "America/Chicago".to_string()
}

fn default_exclude() -> Vec<String> {
    [
        "Rent",
        "Tithing",
        "Utilities",
        "Loans",
        "Insurance",
        "Subscriptions",
        "Inference",
        "Payroll",
        "Transfers",
        "Credit Card",
        "Medical",
    ]
    .into_iter()
    .map(str::to_string)
    .collect()
}

fn default_buckets() -> Vec<SpendingBucket> {
    vec![
        SpendingBucket {
            name: "Groceries".to_string(),
            weekly_dollars: 240,
            categories: vec!["Groceries".to_string()],
        },
        SpendingBucket {
            name: "Everything else".to_string(),
            weekly_dollars: 110,
            categories: [
                "Dining Out",
                "Vending",
                "Housewares",
                "Fuel & Gas",
                "Entertainment",
                "Home Improvement",
                "Pet Care",
                "Travel",
                "Uncategorized",
            ]
            .into_iter()
            .map(str::to_string)
            .collect(),
        },
    ]
}

impl Default for WeeklySpendingConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            timezone: default_timezone(),
            unmapped: UnmappedPolicy::default(),
            unmapped_bucket: weekly_spending_defaults::unmapped_bucket(),
            exclude: default_exclude(),
            buckets: default_buckets(),
        }
    }
}

impl WeeklySpendingConfig {
    /// Load-time checks for this table. Schedule shape is [`validate_section`].
    pub fn validate(&self) -> Result<()> {
        self.validate_identity()?;
        self.validate_names_and_mapping()?;
        Ok(())
    }

    fn validate_identity(&self) -> Result<()> {
        require_zone(&self.timezone)?;
        require_switch_and_unmapped(self)?;
        Ok(())
    }

    fn validate_names_and_mapping(&self) -> Result<()> {
        let mut names = HashSet::new();
        let mut categories = HashSet::new();
        for bucket in &self.buckets {
            check_bucket(bucket, &self.exclude, &mut names, &mut categories)?;
        }
        Ok(())
    }
}

/// Table checks plus the weekly schedule rows.
pub(super) fn validate_section(topology: &Topology) -> Result<()> {
    topology.weekly_spending.validate()?;
    validate_weekly_schedules(topology)?;
    Ok(())
}

fn require_zone(timezone: &str) -> Result<()> {
    parsed_zone(timezone).map(|_| ()).map_err(zone_error)
}

fn zone_error(err: String) -> Error {
    Error::Config(format!("weekly_spending.timezone: {err}"))
}

fn parsed_zone(raw: &str) -> std::result::Result<String, String> {
    UserTimezone::parse(raw)
        .map(|zone| zone.iana_name().to_string())
        .map_err(|err| err.to_string())
}

fn require_switch_and_unmapped(cfg: &WeeklySpendingConfig) -> Result<()> {
    require_buckets_when_enabled(cfg)?;
    require_unmapped_target(cfg)?;
    Ok(())
}

fn require_buckets_when_enabled(cfg: &WeeklySpendingConfig) -> Result<()> {
    if cfg.enabled && cfg.buckets.is_empty() {
        return Err(Error::Config(
            "[weekly_spending] enabled = true requires at least one bucket".into(),
        ));
    }
    Ok(())
}

fn require_unmapped_target(cfg: &WeeklySpendingConfig) -> Result<()> {
    match cfg.unmapped {
        UnmappedPolicy::Ignore => optional_bucket(cfg),
        UnmappedPolicy::Count => count_target(cfg),
    }
}

fn optional_bucket(cfg: &WeeklySpendingConfig) -> Result<()> {
    match cfg.unmapped_bucket.as_deref() {
        Some(name) => require_named_bucket(cfg, name),
        None => Ok(()),
    }
}

fn count_target(cfg: &WeeklySpendingConfig) -> Result<()> {
    let Some(name) = cfg.unmapped_bucket.as_deref() else {
        return Err(Error::Config(
            "[weekly_spending] unmapped = \"count\" requires unmapped_bucket".into(),
        ));
    };
    require_named_bucket(cfg, name)
}

fn require_named_bucket(cfg: &WeeklySpendingConfig, name: &str) -> Result<()> {
    if bucket_named(cfg, name) {
        return Ok(());
    }
    Err(Error::Config(format!(
        "[weekly_spending] unmapped_bucket '{name}' is not a bucket name"
    )))
}

fn bucket_named(cfg: &WeeklySpendingConfig, name: &str) -> bool {
    let want = name.trim();
    for bucket in &cfg.buckets {
        if bucket.name.trim() == want {
            return true;
        }
    }
    false
}

fn check_bucket(
    bucket: &SpendingBucket,
    exclude: &[String],
    names: &mut HashSet<String>,
    categories: &mut HashSet<String>,
) -> Result<()> {
    require_bucket_identity(bucket)?;
    claim_name(bucket, names)?;
    claim_categories(bucket, exclude, categories)?;
    Ok(())
}

fn require_bucket_identity(bucket: &SpendingBucket) -> Result<()> {
    if bucket.name.trim().is_empty() {
        return Err(Error::Config(
            "[weekly_spending] bucket name must not be empty".into(),
        ));
    }
    if bucket.weekly_dollars == 0 {
        return Err(Error::Config(format!(
            "[weekly_spending] bucket '{}' weekly_dollars must be greater than 0",
            bucket.name
        )));
    }
    Ok(())
}

fn claim_name(bucket: &SpendingBucket, names: &mut HashSet<String>) -> Result<()> {
    if names.insert(bucket.name.trim().to_string()) {
        return Ok(());
    }
    Err(Error::Config(format!(
        "[weekly_spending] bucket '{}' is listed more than once",
        bucket.name.trim()
    )))
}

fn claim_categories(
    bucket: &SpendingBucket,
    exclude: &[String],
    seen: &mut HashSet<String>,
) -> Result<()> {
    for category in &bucket.categories {
        claim_category(bucket, category, exclude, seen)?;
    }
    Ok(())
}

fn claim_category(
    bucket: &SpendingBucket,
    category: &str,
    exclude: &[String],
    seen: &mut HashSet<String>,
) -> Result<()> {
    let name = category.trim();
    if name.is_empty() {
        return Err(Error::Config(format!(
            "[weekly_spending] bucket '{}' has an empty category",
            bucket.name
        )));
    }
    if excluded(exclude, name) {
        return Err(Error::Config(format!(
            "[weekly_spending] category '{name}' is in a bucket and the exclude list"
        )));
    }
    if seen.insert(name.to_string()) {
        return Ok(());
    }
    Err(Error::Config(format!(
        "[weekly_spending] category '{name}' is listed more than once"
    )))
}

fn excluded(exclude: &[String], name: &str) -> bool {
    for item in exclude {
        if item.trim() == name {
            return true;
        }
    }
    false
}

fn validate_weekly_schedules(topology: &Topology) -> Result<()> {
    let count = count_enabled_weekly(topology)?;
    require_one_enabled_when_on(&topology.weekly_spending, count)?;
    Ok(())
}

fn count_enabled_weekly(topology: &Topology) -> Result<u32> {
    let mut count = 0u32;
    for schedule in &topology.schedules {
        count = note_weekly_row(
            schedule,
            &topology.weekly_spending,
            &topology.timezone,
            count,
        )?;
    }
    Ok(count)
}

fn note_weekly_row(
    schedule: &CronSchedule,
    cfg: &WeeklySpendingConfig,
    topology_tz: &str,
    count: u32,
) -> Result<u32> {
    if schedule.job.as_deref() != Some(WEEKLY_SPENDING_JOB) {
        return Ok(count);
    }
    check_weekly_row(schedule, cfg, topology_tz)?;
    Ok(enabled_count(schedule, count))
}

fn enabled_count(schedule: &CronSchedule, count: u32) -> u32 {
    if schedule.enabled {
        count.saturating_add(1)
    } else {
        count
    }
}

fn check_weekly_row(
    schedule: &CronSchedule,
    cfg: &WeeklySpendingConfig,
    topology_tz: &str,
) -> Result<()> {
    require_civil_clock(schedule)?;
    require_matching_zone(schedule, cfg, topology_tz)?;
    Ok(())
}

fn require_civil_clock(schedule: &CronSchedule) -> Result<()> {
    if civil_clock(schedule) {
        return Ok(());
    }
    Err(Error::Config(format!(
        "topology.schedules['{}'] job {WEEKLY_SPENDING_JOB} requires days and at, with an empty cron_expr",
        schedule.name
    )))
}

fn civil_clock(schedule: &CronSchedule) -> bool {
    has_days(schedule) && has_at(schedule) && !has_cron(schedule)
}

fn has_days(schedule: &CronSchedule) -> bool {
    !schedule.days.is_empty()
}

fn has_at(schedule: &CronSchedule) -> bool {
    match schedule.at.as_deref() {
        Some(value) => !value.trim().is_empty(),
        None => false,
    }
}

fn has_cron(schedule: &CronSchedule) -> bool {
    !schedule.cron_expr.trim().is_empty()
}

fn require_matching_zone(
    schedule: &CronSchedule,
    cfg: &WeeklySpendingConfig,
    topology_tz: &str,
) -> Result<()> {
    let expected = parsed_zone(&cfg.timezone).map_err(zone_error)?;
    let actual = parsed_zone(zone_source(schedule, topology_tz)).map_err(|err| {
        Error::Config(format!(
            "topology.schedules['{}'].timezone: {err}",
            schedule.name
        ))
    })?;
    zones_match(&schedule.name, &expected, &actual)
}

fn zone_source<'a>(schedule: &'a CronSchedule, topology_tz: &'a str) -> &'a str {
    match schedule.timezone.as_deref() {
        Some(raw) if !raw.trim().is_empty() => raw.trim(),
        _ => fallback_zone(topology_tz),
    }
}

fn fallback_zone(topology_tz: &str) -> &str {
    let trimmed = topology_tz.trim();
    if trimmed.is_empty() {
        liberado_common::DEFAULT_TIMEZONE
    } else {
        trimmed
    }
}

fn zones_match(name: &str, expected: &str, actual: &str) -> Result<()> {
    if expected == actual {
        return Ok(());
    }
    Err(Error::Config(format!(
        "topology.schedules['{name}'] timezone '{actual}' must match [weekly_spending].timezone '{expected}'"
    )))
}

fn require_one_enabled_when_on(cfg: &WeeklySpendingConfig, count: u32) -> Result<()> {
    if !cfg.enabled || count == 1 {
        return Ok(());
    }
    Err(Error::Config(enabled_count_message(count)))
}

fn enabled_count_message(count: u32) -> String {
    if count == 0 {
        "[weekly_spending] enabled = true requires one enabled weekly-spending-push schedule"
            .to_string()
    } else {
        "[weekly_spending] allows only one enabled weekly-spending-push schedule".to_string()
    }
}

/// Hour and minute from the enabled weekly row. `(7, 0)` when that row is absent.
pub fn weekly_open_clock(topology: &Topology) -> (u32, u32) {
    match enabled_at(topology) {
        Some(at) => parse_clock(at),
        None => (7, 0),
    }
}

/// True when one enabled row names [`WEEKLY_SPENDING_JOB`].
pub fn has_enabled_weekly_schedule(topology: &Topology) -> bool {
    for schedule in &topology.schedules {
        if is_enabled_weekly(schedule) {
            return true;
        }
    }
    false
}

fn enabled_at(topology: &Topology) -> Option<&str> {
    for schedule in &topology.schedules {
        if let Some(at) = clock_at(schedule) {
            return Some(at);
        }
    }
    None
}

fn clock_at(schedule: &CronSchedule) -> Option<&str> {
    if !is_enabled_weekly(schedule) {
        return None;
    }
    schedule.at.as_deref()
}

fn is_enabled_weekly(schedule: &CronSchedule) -> bool {
    schedule.enabled && schedule.job.as_deref() == Some(WEEKLY_SPENDING_JOB)
}

fn parse_clock(at: &str) -> (u32, u32) {
    let mut parts = at.trim().split(':');
    let hour = parse_part(parts.next()).unwrap_or(7);
    let minute = parse_part(parts.next()).unwrap_or(0);
    (hour, minute)
}

fn parse_part(part: Option<&str>) -> Option<u32> {
    let text = part?;
    text.parse().ok()
}

#[cfg(test)]
#[path = "weekly_spending_tests.rs"]
mod weekly_spending_tests;
