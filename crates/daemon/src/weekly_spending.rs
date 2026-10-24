//! Weekly spending reminder. Mechanical. No model.
//!
//! Monday's message covers the previous Monday–Sunday. A missed Monday can
//! still send later that week, once. A later midweek alert is not this job.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono::{Datelike, NaiveDate, NaiveDateTime, NaiveTime, TimeDelta};
use liberado_config::{
    UnmappedPolicy, WeeklySpendingConfig, has_enabled_weekly_schedule, weekly_open_clock,
};
use liberado_notify::PushHub;

const TITLE: &str = "Weekly spending";

/// One category total from the budget service. `total_cents` is negative when money was spent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CategorySpend {
    pub name: String,
    pub total_cents: i64,
}

/// How the caller wants the job to behave.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunMode {
    /// Fetch and render. Do not send. Allowed while the switch is off.
    DryRun,
    /// Send to subscribers who have not had this week. Quiet when nobody is due.
    Send,
    /// Send again, then record the week. The scheduler never uses this.
    Force,
}

/// What one run decided. Amounts are already in the message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunOutcome {
    pub message: String,
    pub week_id: String,
    pub start: String,
    pub end: String,
    pub unmapped: Vec<String>,
    pub sent: usize,
    pub failed: usize,
    pub skipped: usize,
    pub removed: usize,
}

/// Clock and switch the daemon keeps for the mechanical job.
#[derive(Debug, Clone)]
pub struct WeeklyRuntime {
    pub config: WeeklySpendingConfig,
    pub catchup: bool,
    pub open_hour: u32,
    pub open_minute: u32,
}

/// Live or test spend for an inclusive date range.
#[async_trait]
pub trait SpendingSource: Send + Sync {
    async fn spending(
        &self,
        start: NaiveDate,
        end: NaiveDate,
    ) -> Result<Vec<CategorySpend>, String>;
}

pub fn weekly_runtime(topology: &liberado_config::Topology) -> WeeklyRuntime {
    let (open_hour, open_minute) = weekly_open_clock(topology);
    WeeklyRuntime {
        config: topology.weekly_spending.clone(),
        catchup: topology.weekly_spending.enabled && has_enabled_weekly_schedule(topology),
        open_hour,
        open_minute,
    }
}

pub fn local_now(fixed: Option<NaiveDateTime>, timezone: &str) -> NaiveDateTime {
    if let Some(now) = fixed {
        return now;
    }
    match liberado_common::UserTimezone::parse(timezone) {
        Ok(zone) => zone.now().naive_local(),
        Err(_) => liberado_common::UserTimezone::default_zone()
            .now()
            .naive_local(),
    }
}

struct WeekWindow {
    week_id: String,
    start: NaiveDate,
    end: NaiveDate,
}

pub fn week_window(now: NaiveDateTime, hour: u32, minute: u32) -> (String, NaiveDate, NaiveDate) {
    let window = window_for(now, hour, minute);
    (window.week_id, window.start, window.end)
}

fn window_for(now: NaiveDateTime, hour: u32, minute: u32) -> WeekWindow {
    let monday = this_monday(now.date());
    let send = if before_open(now, hour, minute) {
        monday - TimeDelta::days(7)
    } else {
        monday
    };
    WeekWindow {
        week_id: send.format("%Y-%m-%d").to_string(),
        start: send - TimeDelta::days(7),
        end: send - TimeDelta::days(1),
    }
}

fn this_monday(day: NaiveDate) -> NaiveDate {
    let offset = i64::from(day.weekday().num_days_from_monday());
    day - TimeDelta::days(offset)
}

fn before_open(now: NaiveDateTime, hour: u32, minute: u32) -> bool {
    now.weekday() == chrono::Weekday::Mon && now.time() < open_time(hour, minute)
}

fn open_time(hour: u32, minute: u32) -> NaiveTime {
    NaiveTime::from_hms_opt(hour.min(23), minute.min(59), 0).unwrap_or(NaiveTime::MIN)
}

/// Nearest whole dollar, half away from zero. `cents` is already "spent" (positive spends).
pub fn round_dollars(cents: i64) -> i64 {
    let negative = cents < 0;
    let rounded = cents.unsigned_abs().saturating_add(50) / 100;
    let dollars = i64::try_from(rounded).unwrap_or(i64::MAX);
    if negative {
        dollars.saturating_neg()
    } else {
        dollars
    }
}

struct BucketTotal {
    name: String,
    spent_dollars: i64,
    budget_dollars: u32,
}

struct Allocation {
    buckets: Vec<BucketTotal>,
    unmapped: Vec<String>,
}

fn allocate(rows: &[CategorySpend], cfg: &WeeklySpendingConfig) -> Allocation {
    let mut totals = vec![0i64; cfg.buckets.len()];
    let mut unmapped = Vec::new();
    for row in rows {
        apply_row(row, cfg, &mut totals, &mut unmapped);
    }
    Allocation {
        buckets: finish_buckets(cfg, &totals),
        unmapped,
    }
}

fn apply_row(
    row: &CategorySpend,
    cfg: &WeeklySpendingConfig,
    totals: &mut [i64],
    unmapped: &mut Vec<String>,
) {
    let name = row.name.trim();
    let spent = row.total_cents.saturating_neg();
    if let Some(index) = bucket_index(cfg, name) {
        add_spent(totals, index, spent);
        return;
    }
    note_unmapped(cfg, name, spent, totals, unmapped);
}

fn note_unmapped(
    cfg: &WeeklySpendingConfig,
    name: &str,
    spent: i64,
    totals: &mut [i64],
    unmapped: &mut Vec<String>,
) {
    if excluded(cfg, name) || spent == 0 {
        return;
    }
    unmapped.push(name.to_string());
    add_counted(cfg, totals, spent);
}

fn add_counted(cfg: &WeeklySpendingConfig, totals: &mut [i64], spent: i64) {
    if cfg.unmapped != UnmappedPolicy::Count {
        return;
    }
    let Some(name) = cfg.unmapped_bucket.as_deref() else {
        return;
    };
    if let Some(index) = bucket_by_name(cfg, name.trim()) {
        add_spent(totals, index, spent);
    }
}

fn bucket_by_name(cfg: &WeeklySpendingConfig, name: &str) -> Option<usize> {
    cfg.buckets
        .iter()
        .position(|bucket| bucket.name.trim() == name)
}

fn add_spent(totals: &mut [i64], index: usize, spent: i64) {
    if let Some(slot) = totals.get_mut(index) {
        *slot = slot.saturating_add(spent);
    }
}

fn bucket_index(cfg: &WeeklySpendingConfig, name: &str) -> Option<usize> {
    for (index, bucket) in cfg.buckets.iter().enumerate() {
        if category_in(&bucket.categories, name) {
            return Some(index);
        }
    }
    None
}

fn category_in(categories: &[String], name: &str) -> bool {
    for category in categories {
        if category.trim() == name {
            return true;
        }
    }
    false
}

fn excluded(cfg: &WeeklySpendingConfig, name: &str) -> bool {
    for item in &cfg.exclude {
        if item.trim() == name {
            return true;
        }
    }
    false
}

fn finish_buckets(cfg: &WeeklySpendingConfig, totals: &[i64]) -> Vec<BucketTotal> {
    let mut out = Vec::new();
    for (index, bucket) in cfg.buckets.iter().enumerate() {
        out.push(BucketTotal {
            name: bucket.name.clone(),
            spent_dollars: round_dollars(*totals.get(index).unwrap_or(&0)),
            budget_dollars: bucket.weekly_dollars,
        });
    }
    out
}

fn render(buckets: &[BucketTotal]) -> String {
    format!(
        "Last week: {}. This week starts fresh: {}.",
        last_week(buckets),
        fresh(buckets)
    )
}

fn last_week(buckets: &[BucketTotal]) -> String {
    let mut parts = Vec::new();
    for bucket in buckets {
        parts.push(format!(
            "{} {} of {}",
            lower(&bucket.name),
            money(bucket.spent_dollars),
            money(i64::from(bucket.budget_dollars))
        ));
    }
    parts.join(", ")
}

fn fresh(buckets: &[BucketTotal]) -> String {
    let mut parts = Vec::new();
    for bucket in buckets {
        parts.push(format!(
            "{} {}",
            money(i64::from(bucket.budget_dollars)),
            lower(&bucket.name)
        ));
    }
    parts.join(", ")
}

fn lower(name: &str) -> String {
    name.to_lowercase()
}

fn money(dollars: i64) -> String {
    if dollars < 0 {
        return format!("-${}", dollars.unsigned_abs());
    }
    format!("${dollars}")
}

pub async fn run_weekly(
    cfg: &WeeklySpendingConfig,
    local_now: NaiveDateTime,
    open_hour: u32,
    open_minute: u32,
    source: Arc<dyn SpendingSource>,
    hub: Arc<PushHub>,
    mode: RunMode,
) -> Result<RunOutcome, String> {
    prepare_run(cfg, mode)?;
    let (week_id, start, end) = week_window(local_now, open_hour, open_minute);
    if skip_fetch(mode, &hub, &week_id) {
        return Ok(quiet(&week_id, start, end));
    }
    let rows = source.spending(start, end).await?;
    let allocation = allocate(&rows, cfg);
    let message = render(&allocation.buckets);
    deliver(
        mode,
        &hub,
        &week_id,
        start,
        end,
        &message,
        allocation.unmapped,
    )
    .await
}

fn prepare_run(cfg: &WeeklySpendingConfig, mode: RunMode) -> Result<(), String> {
    if cfg.enabled || matches!(mode, RunMode::DryRun) {
        return Ok(());
    }
    Err("weekly spending is off".into())
}

fn skip_fetch(mode: RunMode, hub: &PushHub, week_id: &str) -> bool {
    matches!(mode, RunMode::Send) && !hub.needs_week(week_id)
}

fn quiet(week_id: &str, start: NaiveDate, end: NaiveDate) -> RunOutcome {
    RunOutcome {
        message: String::new(),
        week_id: week_id.to_string(),
        start: start.to_string(),
        end: end.to_string(),
        unmapped: Vec::new(),
        sent: 0,
        failed: 0,
        skipped: 0,
        removed: 0,
    }
}

async fn deliver(
    mode: RunMode,
    hub: &PushHub,
    week_id: &str,
    start: NaiveDate,
    end: NaiveDate,
    message: &str,
    unmapped: Vec<String>,
) -> Result<RunOutcome, String> {
    let report = match mode {
        RunMode::DryRun => liberado_notify::SendReport::default(),
        RunMode::Send => hub.send_week(week_id, TITLE, message, false).await,
        RunMode::Force => hub.send_week(week_id, TITLE, message, true).await,
    };
    Ok(outcome(week_id, start, end, message, unmapped, report))
}

fn outcome(
    week_id: &str,
    start: NaiveDate,
    end: NaiveDate,
    message: &str,
    unmapped: Vec<String>,
    report: liberado_notify::SendReport,
) -> RunOutcome {
    RunOutcome {
        message: message.to_string(),
        week_id: week_id.to_string(),
        start: start.to_string(),
        end: end.to_string(),
        unmapped,
        sent: report.sent,
        failed: report.failed,
        skipped: report.skipped,
        removed: report.removed,
    }
}

pub async fn run_weekly_job(
    cfg: WeeklySpendingConfig,
    now: NaiveDateTime,
    hour: u32,
    minute: u32,
    source: Option<Arc<dyn SpendingSource>>,
    hub: Arc<PushHub>,
    mode: RunMode,
) {
    let source = match resolve_source(source) {
        Ok(source) => source,
        Err(err) => {
            tracing::warn!("{err}");
            return;
        }
    };
    match run_weekly(&cfg, now, hour, minute, source, hub, mode).await {
        Ok(outcome) => log_outcome(&outcome),
        Err(err) => tracing::warn!("{err}"),
    }
}

fn resolve_source(
    source: Option<Arc<dyn SpendingSource>>,
) -> Result<Arc<dyn SpendingSource>, String> {
    if let Some(source) = source {
        return Ok(source);
    }
    Ok(Arc::new(HttpBudget::from_env()?))
}

fn log_outcome(outcome: &RunOutcome) {
    if !outcome.unmapped.is_empty() {
        tracing::warn!(
            categories = %outcome.unmapped.join(", "),
            "weekly spending has unmapped categories"
        );
    }
    tracing::info!(message = %outcome.message, "weekly spending reminder");
}

pub struct HttpBudget {
    base: String,
    key: String,
    client: reqwest::Client,
}

impl HttpBudget {
    pub fn from_parts(base: &str, key: &str) -> Result<Self, String> {
        let base = base.trim().trim_end_matches('/').to_string();
        if base.is_empty() || key.trim().is_empty() {
            return Err("budget API is not configured".into());
        }
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .build()
            .map_err(|_| "budget client failed".to_string())?;
        Ok(Self {
            base,
            key: key.trim().to_string(),
            client,
        })
    }

    pub fn from_env() -> Result<Self, String> {
        let base = std::env::var("LIBERADO_BUDGET_API_URL").unwrap_or_default();
        let key = std::env::var("LIBERADO_BUDGET_API_KEY").unwrap_or_default();
        Self::from_parts(&base, &key)
    }
}

#[async_trait]
impl SpendingSource for HttpBudget {
    async fn spending(
        &self,
        start: NaiveDate,
        end: NaiveDate,
    ) -> Result<Vec<CategorySpend>, String> {
        let url = format!(
            "{}/api/v1/spending/by-category?start_date={start}&end_date={end}",
            self.base
        );
        let response = self
            .client
            .get(&url)
            .header("X-API-Key", &self.key)
            .send()
            .await
            .map_err(transport_error)?;
        let status = response.status();
        if !status.is_success() {
            return Err(format!("budget API status {}", status.as_u16()));
        }
        let body: serde_json::Value = response
            .json()
            .await
            .map_err(|_| "budget API response was not JSON".to_string())?;
        Ok(parse_categories(&body))
    }
}

fn transport_error(err: reqwest::Error) -> String {
    liberado_common::redact_secrets(&err.without_url().to_string())
}

fn parse_categories(value: &serde_json::Value) -> Vec<CategorySpend> {
    let Some(rows) = value
        .get("categories")
        .and_then(serde_json::Value::as_array)
    else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for row in rows {
        if let Some(spend) = one_category(row) {
            out.push(spend);
        }
    }
    out
}

fn one_category(row: &serde_json::Value) -> Option<CategorySpend> {
    let name = row.get("category_name")?.as_str()?.to_string();
    let total_cents = row.get("total_cents")?.as_i64()?;
    Some(CategorySpend { name, total_cents })
}

#[cfg(test)]
#[path = "weekly_spending_tests.rs"]
mod weekly_spending_tests;
