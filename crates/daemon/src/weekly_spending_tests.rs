//! Week window, dollar rounding, the budget HTTP client, and the weekly job.
//! No live network.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use chrono::{NaiveDate, NaiveDateTime};
use liberado_config::{UnmappedPolicy, WeeklySpendingConfig};
use liberado_notify::{NewSubscription, PushHub};

use super::{
    CategorySpend, HttpBudget, RunMode, SpendingSource, round_dollars, run_weekly, run_weekly_job,
    week_window,
};

const AS_PRIVATE: &str = "yfWPiYE-n46HLnH0KqZOF1fJJU3MYrct3AELtAQ-oRw";
const AS_PUBLIC: &str =
    "BP4z9KsN6nGRTbVYI_c7VJSPQTBtkgcy27mlmlMoZIIgDll6e3vCYLocInmYWAmS6TlzAC8wEqKK6PBru3jl7A8";
const UA_PUBLIC: &str =
    "BCVxsr7N_eNgVRqvHtD0zTZsEc6-VV-JvLexhqUzORcxaOzi6-AYWXvTBHm4bjyPjs7Vd8pZGH6SRpkNtoIAiw4";
const AUTH: &str = "BTBZMqHH6r4Tts7J_aSIgg";

fn at(year: i32, month: u32, day: u32, hour: u32, minute: u32) -> NaiveDateTime {
    NaiveDate::from_ymd_opt(year, month, day)
        .expect("date")
        .and_hms_opt(hour, minute, 0)
        .expect("time")
}

fn day(year: i32, month: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(year, month, day).expect("date")
}

struct FixedSource {
    rows: Vec<CategorySpend>,
    calls: AtomicUsize,
    error: Option<&'static str>,
}

impl FixedSource {
    fn rows(rows: Vec<CategorySpend>) -> Self {
        Self {
            rows,
            calls: AtomicUsize::new(0),
            error: None,
        }
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    fn fail(error: &'static str) -> Self {
        Self {
            rows: Vec::new(),
            calls: AtomicUsize::new(0),
            error: Some(error),
        }
    }
}

#[async_trait]
impl SpendingSource for FixedSource {
    async fn spending(
        &self,
        _start: NaiveDate,
        _end: NaiveDate,
        _exclude: &[String],
    ) -> Result<Vec<CategorySpend>, String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if let Some(error) = self.error {
            return Err(error.to_string());
        }
        Ok(self.rows.clone())
    }
}

fn spend(name: &str, total_cents: i64) -> CategorySpend {
    CategorySpend {
        name: name.to_string(),
        total_cents,
    }
}

#[test]
fn the_window_covers_the_previous_monday_through_sunday() {
    let friday = week_window(at(2026, 10, 23, 15, 0), 7, 0);
    assert_eq!(friday.0, "2026-10-19");
    assert_eq!(friday.1, day(2026, 10, 12));
    assert_eq!(friday.2, day(2026, 10, 18));

    let open = week_window(at(2026, 10, 26, 7, 0), 7, 0);
    assert_eq!(open.0, "2026-10-26");
    assert_eq!(open.1, day(2026, 10, 19));
    assert_eq!(open.2, day(2026, 10, 25));

    let early = week_window(at(2026, 10, 26, 6, 59), 7, 0);
    assert_eq!(early.0, "2026-10-19");
    assert_eq!(early.1, day(2026, 10, 12));
    assert_eq!(early.2, day(2026, 10, 18));

    let sunday = week_window(at(2026, 10, 25, 23, 0), 7, 0);
    assert_eq!(sunday.0, "2026-10-19");

    let later_open = week_window(at(2026, 10, 26, 7, 15), 7, 30);
    assert_eq!(later_open.0, "2026-10-19");
    assert_eq!(later_open.1, day(2026, 10, 12));
    assert_eq!(later_open.2, day(2026, 10, 18));
}

#[test]
fn cents_round_to_the_nearest_dollar_half_away_from_zero() {
    assert_eq!(round_dollars(44_665), 447);
    assert_eq!(round_dollars(50), 1);
    assert_eq!(round_dollars(49), 0);
    assert_eq!(round_dollars(-250), -3);
}

#[tokio::test]
async fn the_fixture_counts_shipping_and_excludes_rent() {
    let rows = vec![
        spend("Groceries", -21_200),
        spend("Dining Out", -13_500),
        spend("Shipping", -800),
        spend("Rent", -100_000),
        spend("Uncategorized", 0),
    ];
    let outcome = run_weekly(
        &WeeklySpendingConfig::default(),
        at(2026, 10, 23, 15, 0),
        7,
        0,
        Arc::new(FixedSource::rows(rows)),
        Arc::new(PushHub::inert()),
        RunMode::DryRun,
    )
    .await
    .expect("dry run");
    assert_eq!(
        outcome.message,
        "Last week: groceries $212 of $240 ($28 left), everything else $143 of $110 (over by $33). This week starts fresh: $240 groceries, $110 everything else."
    );
    assert_eq!(outcome.unmapped, vec!["Shipping $8.00".to_string()]);
    assert_eq!(outcome.week_id, "2026-10-19");
    assert_eq!(outcome.start, "2026-10-12");
    assert_eq!(outcome.end, "2026-10-18");
    assert_eq!(outcome.sent, 0);
}

#[tokio::test]
async fn two_half_dollars_in_one_bucket_round_once() {
    let message = dry_message(vec![spend("Dining Out", -50), spend("Vending", -50)]).await;
    assert!(
        message.contains("everything else $1 of $110 ($109 left)"),
        "{message}"
    );
}

#[tokio::test]
async fn a_net_credit_shows_zero_spent_and_the_full_budget_left() {
    let message = dry_message(vec![spend("Groceries", 250)]).await;
    assert!(
        message.contains("groceries $0 of $240 ($240 left)"),
        "{message}"
    );
}

#[tokio::test]
async fn api_cents_round_after_they_are_flipped_to_spent() {
    let groceries = dry_message(vec![spend("Groceries", -44_665)]).await;
    assert!(
        groceries.contains("groceries $447 of $240 (over by $207)"),
        "{groceries}"
    );
    let one = dry_message(vec![spend("Groceries", -50)]).await;
    assert!(one.contains("groceries $1 of $240 ($239 left)"), "{one}");
    let zero = dry_message(vec![spend("Groceries", -49)]).await;
    assert!(zero.contains("groceries $0 of $240 ($240 left)"), "{zero}");
}

#[tokio::test]
async fn count_still_names_the_unmapped_category() {
    let cfg = WeeklySpendingConfig {
        unmapped: UnmappedPolicy::Count,
        unmapped_bucket: Some("Everything else".to_string()),
        ..WeeklySpendingConfig::default()
    };
    let source = Arc::new(FixedSource::rows(vec![spend("Shipping", -100)]));
    let outcome = run_weekly(
        &cfg,
        at(2026, 10, 23, 15, 0),
        7,
        0,
        source,
        Arc::new(PushHub::inert()),
        RunMode::DryRun,
    )
    .await
    .expect("count");
    assert_eq!(outcome.unmapped, vec!["Shipping $1.00".to_string()]);
    assert!(
        outcome
            .message
            .contains("everything else $1 of $110 ($109 left)"),
        "{}",
        outcome.message
    );
}

#[tokio::test]
async fn the_default_counts_shipping_into_everything_else_and_logs_dollars() {
    let outcome = run_weekly(
        &WeeklySpendingConfig::default(),
        at(2026, 10, 23, 15, 0),
        7,
        0,
        Arc::new(FixedSource::rows(vec![
            spend("Shipping", -1_200),
            spend("Postage", -400),
        ])),
        Arc::new(PushHub::inert()),
        RunMode::DryRun,
    )
    .await
    .expect("count");
    assert_eq!(
        outcome.unmapped,
        vec!["Shipping $12.00".to_string(), "Postage $4.00".to_string()]
    );
    assert_eq!(
        outcome.unmapped.join(", "),
        "Shipping $12.00, Postage $4.00"
    );
    assert!(
        outcome
            .message
            .contains("everything else $16 of $110 ($94 left)"),
        "{}",
        outcome.message
    );
}

#[tokio::test]
async fn ignore_logs_the_amount_and_leaves_the_bucket_unchanged() {
    let cfg = WeeklySpendingConfig {
        unmapped: UnmappedPolicy::Ignore,
        ..WeeklySpendingConfig::default()
    };
    let outcome = run_weekly(
        &cfg,
        at(2026, 10, 23, 15, 0),
        7,
        0,
        Arc::new(FixedSource::rows(vec![
            spend("Dining Out", -13_500),
            spend("Shipping", -800),
        ])),
        Arc::new(PushHub::inert()),
        RunMode::DryRun,
    )
    .await
    .expect("ignore");
    assert_eq!(outcome.unmapped, vec!["Shipping $8.00".to_string()]);
    assert!(
        outcome
            .message
            .contains("everything else $135 of $110 (over by $25)"),
        "{}",
        outcome.message
    );
    assert!(!outcome.message.contains("everything else $143"));
}

#[tokio::test]
async fn a_dry_run_fetches_while_the_switch_is_off_and_does_not_send() {
    let source = Arc::new(FixedSource::rows(vec![spend("Groceries", -100)]));
    let outcome = run_weekly(
        &WeeklySpendingConfig::default(),
        at(2026, 10, 23, 15, 0),
        7,
        0,
        Arc::clone(&source) as Arc<dyn SpendingSource>,
        Arc::new(PushHub::inert()),
        RunMode::DryRun,
    )
    .await
    .expect("dry");
    assert_eq!(source.calls(), 1);
    assert_eq!(outcome.sent, 0);
    assert!(!outcome.message.is_empty());
}

#[tokio::test]
async fn a_send_while_the_switch_is_off_does_not_fetch() {
    let source = Arc::new(FixedSource::rows(vec![spend("Groceries", -100)]));
    let err = run_weekly(
        &WeeklySpendingConfig::default(),
        at(2026, 10, 23, 15, 0),
        7,
        0,
        Arc::clone(&source) as Arc<dyn SpendingSource>,
        Arc::new(PushHub::inert()),
        RunMode::Send,
    )
    .await
    .expect_err("off");
    assert_eq!(err, "weekly spending is off");
    assert_eq!(source.calls(), 0);
}

#[tokio::test]
async fn a_send_with_nobody_due_does_not_fetch() {
    let cfg = WeeklySpendingConfig {
        enabled: true,
        ..WeeklySpendingConfig::default()
    };
    let source = Arc::new(FixedSource::rows(vec![spend("Groceries", -100)]));
    let outcome = run_weekly(
        &cfg,
        at(2026, 10, 23, 15, 0),
        7,
        0,
        Arc::clone(&source) as Arc<dyn SpendingSource>,
        Arc::new(PushHub::inert()),
        RunMode::Send,
    )
    .await
    .expect("quiet");
    assert_eq!(source.calls(), 0);
    assert!(outcome.message.is_empty());
}

#[test]
fn an_empty_budget_client_names_the_gap_and_hides_the_key() {
    let err = budget_config_error(HttpBudget::from_parts("", "example-budget-key"));
    assert_eq!(err, "budget API is not configured");
    assert!(!err.contains("example-budget-key"));
    let err = budget_config_error(HttpBudget::from_parts("http://budget.example.test", "  "));
    assert_eq!(err, "budget API is not configured");
}

fn budget_config_error(result: Result<HttpBudget, String>) -> String {
    match result {
        Err(err) => err,
        Ok(_) => panic!("budget client must stay unconfigured"),
    }
}

#[tokio::test]
async fn the_budget_get_sends_the_key_and_reads_negative_cents() {
    let server = wiremock::MockServer::start().await;
    wiremock::Mock::given(wiremock::matchers::method("GET"))
        .and(wiremock::matchers::path("/api/v1/spending/by-category"))
        .and(wiremock::matchers::query_param("start_date", "2026-10-12"))
        .and(wiremock::matchers::query_param("end_date", "2026-10-18"))
        .and(wiremock::matchers::header(
            "X-API-Key",
            "example-budget-key",
        ))
        .respond_with(
            wiremock::ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "categories": [{
                    "category_name": "Groceries",
                    "group_name": "Food & Dining",
                    "total_cents": -44665,
                    "total_display": "-$446.65",
                    "transaction_count": 13
                }]
            })),
        )
        .mount(&server)
        .await;
    mount_json(
        &server,
        "/api/v1/categories",
        serde_json::json!({"category_groups": []}),
    )
    .await;
    mount_json(
        &server,
        "/api/v1/transactions",
        serde_json::json!({"transactions": []}),
    )
    .await;
    let client = HttpBudget::from_parts(&server.uri(), "example-budget-key").expect("client");
    let rows = client
        .spending(day(2026, 10, 12), day(2026, 10, 18), &[])
        .await
        .expect("rows");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].name, "Groceries");
    assert_eq!(rows[0].total_cents, -44_665);
}

#[tokio::test]
async fn a_budget_error_is_the_status_and_not_the_body() {
    let server = wiremock::MockServer::start().await;
    wiremock::Mock::given(wiremock::matchers::method("GET"))
        .respond_with(wiremock::ResponseTemplate::new(500).set_body_string("do-not-echo-this-body"))
        .mount(&server)
        .await;
    let client = HttpBudget::from_parts(&server.uri(), "example-budget-key").expect("client");
    let err = client
        .spending(day(2026, 10, 12), day(2026, 10, 18), &[])
        .await
        .expect_err("status");
    assert_eq!(err, "budget API status 500");
    assert!(!err.contains("do-not-echo-this-body"));
    assert!(!err.contains("example-budget-key"));
}

async fn mount_json(server: &wiremock::MockServer, path: &str, body: serde_json::Value) {
    wiremock::Mock::given(wiremock::matchers::method("GET"))
        .and(wiremock::matchers::path(path))
        .and(wiremock::matchers::header(
            "X-API-Key",
            "example-budget-key",
        ))
        .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(body))
        .mount(server)
        .await;
}

async fn dry_message(rows: Vec<CategorySpend>) -> String {
    let outcome = run_weekly(
        &WeeklySpendingConfig::default(),
        at(2026, 10, 23, 15, 0),
        7,
        0,
        Arc::new(FixedSource::rows(rows)),
        Arc::new(PushHub::inert()),
        RunMode::DryRun,
    )
    .await
    .expect("dry run");
    outcome.message
}

fn enabled_config() -> WeeklySpendingConfig {
    WeeklySpendingConfig {
        enabled: true,
        ..WeeklySpendingConfig::default()
    }
}

fn phone(endpoint: &str) -> NewSubscription {
    NewSubscription {
        endpoint: endpoint.to_string(),
        p256dh: UA_PUBLIC.to_string(),
        auth: AUTH.to_string(),
        label: "phone".to_string(),
    }
}

fn hub_path(name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("liberado-weekly-{name}-{}", std::process::id()))
}

/// Clears the budget env for this test and writes the previous values back on drop.
/// The job reads them only when the caller passes no source. The values are never logged.
struct ClearedBudgetEnv {
    url: Option<String>,
    key: Option<String>,
}

impl ClearedBudgetEnv {
    fn clear() -> Self {
        let url = std::env::var("LIBERADO_BUDGET_API_URL").ok();
        let key = std::env::var("LIBERADO_BUDGET_API_KEY").ok();
        // SAFETY: env mutation is process-global. This test is the only daemon
        // reader of these two names, and Drop writes the previous values back.
        unsafe {
            std::env::remove_var("LIBERADO_BUDGET_API_URL");
            std::env::remove_var("LIBERADO_BUDGET_API_KEY");
        }
        Self { url, key }
    }
}

impl Drop for ClearedBudgetEnv {
    fn drop(&mut self) {
        restore_env("LIBERADO_BUDGET_API_URL", self.url.take());
        restore_env("LIBERADO_BUDGET_API_KEY", self.key.take());
    }
}

fn restore_env(name: &str, value: Option<String>) {
    // SAFETY: pairs with `ClearedBudgetEnv::clear`. The previous value is put back
    // and is not logged.
    unsafe {
        match value {
            Some(value) => std::env::set_var(name, value),
            None => std::env::remove_var(name),
        }
    }
}

#[tokio::test]
async fn a_job_sends_once_and_the_next_send_does_not_fetch() {
    let server = wiremock::MockServer::start().await;
    wiremock::Mock::given(wiremock::matchers::method("POST"))
        .respond_with(wiremock::ResponseTemplate::new(201))
        .expect(1)
        .mount(&server)
        .await;
    let path = hub_path("send");
    let hub = Arc::new(
        PushHub::with_keys(
            path.clone(),
            AS_PUBLIC,
            AS_PRIVATE,
            "mailto:push@example.com",
        )
        .expect("keys"),
    );
    let endpoint = format!("{}/fake-subscription", server.uri());
    hub.upsert(phone(&endpoint)).expect("subscribe");
    let source = Arc::new(FixedSource::rows(vec![spend("Groceries", -100)]));
    let now = at(2026, 10, 23, 15, 0);
    let injected = Arc::clone(&source) as Arc<dyn SpendingSource>;
    run_weekly_job(
        enabled_config(),
        now,
        7,
        0,
        Some(Arc::clone(&injected)),
        Arc::clone(&hub),
        RunMode::Send,
    )
    .await;
    assert_eq!(source.calls(), 1);
    assert!(!hub.needs_week("2026-10-19"));
    run_weekly_job(
        enabled_config(),
        now,
        7,
        0,
        Some(injected),
        hub,
        RunMode::Send,
    )
    .await;
    assert_eq!(source.calls(), 1);
    let _ = std::fs::remove_file(path);
}

#[tokio::test]
async fn a_job_keeps_the_week_due_when_the_source_errors() {
    let path = hub_path("source-err");
    let hub = Arc::new(PushHub::at_path(path.clone()));
    hub.upsert(phone("https://push.example.test/fake-subscription"))
        .expect("subscribe");
    let source = Arc::new(FixedSource::fail("budget down"));
    run_weekly_job(
        enabled_config(),
        at(2026, 10, 23, 15, 0),
        7,
        0,
        Some(Arc::clone(&source) as Arc<dyn SpendingSource>),
        Arc::clone(&hub),
        RunMode::Send,
    )
    .await;
    assert_eq!(source.calls(), 1);
    assert!(hub.needs_week("2026-10-19"));
    let _ = std::fs::remove_file(path);
}

#[tokio::test]
async fn a_job_stops_when_the_budget_env_is_missing() {
    let _guard = ClearedBudgetEnv::clear();
    let err = budget_config_error(HttpBudget::from_env());
    assert_eq!(err, "budget API is not configured");
    let path = hub_path("no-env");
    let hub = Arc::new(PushHub::at_path(path.clone()));
    hub.upsert(phone("https://push.example.test/fake-subscription"))
        .expect("subscribe");
    run_weekly_job(
        enabled_config(),
        at(2026, 10, 23, 15, 0),
        7,
        0,
        None,
        Arc::clone(&hub),
        RunMode::Send,
    )
    .await;
    assert!(hub.needs_week("2026-10-19"));
    let _ = std::fs::remove_file(path);
}
