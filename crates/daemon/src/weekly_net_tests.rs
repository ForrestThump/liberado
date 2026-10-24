//! Refund netting and the Monday message. No live network.

use std::sync::Arc;

use async_trait::async_trait;
use chrono::NaiveDate;
use liberado_config::WeeklySpendingConfig;
use liberado_notify::PushHub;

use super::{
    BudgetHttp, CategorySpend, PositiveRow, apply_refunds, collect_income, fold_refunds,
    log_skipped, parse_categories, parse_transactions,
};
use crate::weekly_spending::{HttpBudget, RunMode, RunOutcome, SpendingSource, run_weekly};

const KEY: &str = "example-budget-key";
const LIVE_WEEK: &str = "Last week: groceries $447 of $240 (over by $207), everything else $219 of $110 (over by $109). This week starts fresh: $240 groceries, $110 everything else.";
const WEEK: &[(&str, &str)] = &[("start_date", "2026-10-12"), ("end_date", "2026-10-18")];
const PAGE: &[(&str, &str)] = &[
    ("start_date", "2026-10-12"),
    ("end_date", "2026-10-18"),
    ("min_amount_cents", "1"),
    ("limit", "2000"),
    ("offset", "0"),
];

struct Rows(Vec<CategorySpend>);

#[async_trait]
impl SpendingSource for Rows {
    async fn spending(
        &self,
        _start: NaiveDate,
        _end: NaiveDate,
        _exclude: &[String],
    ) -> Result<Vec<CategorySpend>, String> {
        Ok(self.0.clone())
    }
}

fn at() -> chrono::NaiveDateTime {
    NaiveDate::from_ymd_opt(2026, 10, 23)
        .expect("date")
        .and_hms_opt(15, 0, 0)
        .expect("time")
}

fn day(day_of_month: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 10, day_of_month).expect("date")
}

fn tx(category: &str, amount_cents: i64) -> serde_json::Value {
    serde_json::json!({
        "category": category,
        "amount_cents": amount_cents,
        "date": "2026-10-15",
        "payee": "payee-should-stay-out",
        "account_name": "account-should-stay-out"
    })
}

fn live_spend() -> serde_json::Value {
    serde_json::json!({
        "categories": [
            {"category_name": "Loans", "total_cents": -61258},
            {"category_name": "Groceries", "total_cents": -44665},
            {"category_name": "Tithing", "total_cents": -28514},
            {"category_name": "Credit Card", "total_cents": -25000},
            {"category_name": "Utilities", "total_cents": -24127},
            {"category_name": "Insurance", "total_cents": -19797},
            {"category_name": "Dining Out", "total_cents": -10009},
            {"category_name": "Uncategorized", "total_cents": -7494},
            {"category_name": "Vending", "total_cents": -2890},
            {"category_name": "Subscriptions", "total_cents": -1469},
            {"category_name": "Housewares", "total_cents": -973},
            {"category_name": "Shipping", "total_cents": -555}
        ]
    })
}

fn live_groups() -> serde_json::Value {
    serde_json::json!({
        "category_groups": [
            {
                "name": "Income",
                "is_income": true,
                "categories": [{"name": "Payroll", "is_income": true}]
            },
            {
                "name": "Rewards",
                "is_income": false,
                "categories": [{"name": "Cashback", "is_income": true}]
            }
        ]
    })
}

fn live_transactions(extra: Option<serde_json::Value>) -> serde_json::Value {
    let mut rows = vec![
        tx("Payroll", 285_140),
        tx("Cashback", 273),
        tx("Transfers", 25_000),
    ];
    if let Some(extra) = extra {
        rows.push(extra);
    }
    serde_json::json!({ "transactions": rows })
}

async fn mount(
    server: &wiremock::MockServer,
    path: &str,
    body: serde_json::Value,
    queries: &[(&str, &str)],
) {
    let mut builder = wiremock::Mock::given(wiremock::matchers::method("GET"))
        .and(wiremock::matchers::path(path))
        .and(wiremock::matchers::header("X-API-Key", KEY));
    for (name, value) in queries {
        builder = builder.and(wiremock::matchers::query_param(*name, *value));
    }
    builder
        .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(body))
        .expect(1)
        .mount(server)
        .await;
}

async fn mount_week(server: &wiremock::MockServer, transactions: serde_json::Value) {
    mount(server, "/api/v1/spending/by-category", live_spend(), WEEK).await;
    mount(server, "/api/v1/categories", live_groups(), &[]).await;
    mount(server, "/api/v1/transactions", transactions, PAGE).await;
}

async fn week_from(server: &wiremock::MockServer) -> RunOutcome {
    let client = HttpBudget::from_parts(&server.uri(), KEY).expect("client");
    let outcome = run_weekly(
        &WeeklySpendingConfig::default(),
        at(),
        7,
        0,
        Arc::new(client),
        Arc::new(PushHub::inert()),
        RunMode::DryRun,
    )
    .await
    .expect("week");
    server.verify().await;
    outcome
}

fn hides_private_text(outcome: &RunOutcome) {
    let unmapped = outcome.unmapped.join(", ");
    assert!(!outcome.message.contains("payee-should-stay-out"));
    assert!(!outcome.message.contains("account-should-stay-out"));
    assert!(!outcome.message.contains(KEY));
    assert!(!unmapped.contains("payee-should-stay-out"));
    assert!(!unmapped.contains("account-should-stay-out"));
}

#[tokio::test]
async fn the_live_week_counts_shipping_and_drops_income() {
    let server = wiremock::MockServer::start().await;
    mount_week(&server, live_transactions(None)).await;
    let outcome = week_from(&server).await;
    assert_eq!(outcome.message, LIVE_WEEK);
    assert_eq!(outcome.unmapped, vec!["Shipping $5.55".to_string()]);
    assert_eq!(outcome.start, "2026-10-12");
    assert_eq!(outcome.end, "2026-10-18");
    hides_private_text(&outcome);
}

#[tokio::test]
async fn a_groceries_refund_reduces_that_bucket() {
    let server = wiremock::MockServer::start().await;
    mount_week(&server, live_transactions(Some(tx("Groceries", 2_000)))).await;
    let outcome = week_from(&server).await;
    assert_eq!(
        outcome.message,
        "Last week: groceries $427 of $240 (over by $187), everything else $219 of $110 (over by $109). This week starts fresh: $240 groceries, $110 everything else."
    );
    assert_eq!(outcome.unmapped, vec!["Shipping $5.55".to_string()]);
    hides_private_text(&outcome);
}

#[test]
fn income_exclude_and_blank_rows_do_not_reduce_spend() {
    let spent = vec![
        CategorySpend {
            name: "Groceries".to_string(),
            total_cents: -44_665,
        },
        CategorySpend {
            name: "Inference".to_string(),
            total_cents: -9_000,
        },
    ];
    let groups = serde_json::json!({
        "category_groups": [
            {
                "name": "Income",
                "is_income": true,
                "categories": [{"name": "Payroll", "is_income": false}]
            },
            {
                "name": "Rewards",
                "is_income": false,
                "categories": [{"name": "Cashback", "is_income": true}]
            }
        ]
    });
    let income = collect_income(&groups).expect("groups");
    let positives = vec![
        PositiveRow {
            category: "Payroll".to_string(),
            amount_cents: 285_140,
        },
        PositiveRow {
            category: "Cashback".to_string(),
            amount_cents: 273,
        },
        PositiveRow {
            category: "Transfers".to_string(),
            amount_cents: 25_000,
        },
        PositiveRow {
            category: "Inference".to_string(),
            amount_cents: 100,
        },
        PositiveRow {
            category: " Groceries ".to_string(),
            amount_cents: 2_000,
        },
        PositiveRow {
            category: String::new(),
            amount_cents: 100,
        },
        PositiveRow {
            category: "Uncategorized".to_string(),
            amount_cents: 50,
        },
    ];
    let exclude = WeeklySpendingConfig::default().exclude;
    let (rows, skipped) = fold_refunds(spent, &positives, &income, &exclude);
    log_skipped(skipped);
    assert_eq!(skipped, 2);
    assert_eq!(rows[0].total_cents, -42_665);
    assert_eq!(rows[1].total_cents, -9_000);
    assert_eq!(rows.len(), 2);
    let settled = apply_refunds(
        vec![CategorySpend {
            name: "Groceries".to_string(),
            total_cents: -44_665,
        }],
        &positives,
        &income,
        &exclude,
    );
    assert_eq!(settled[0].total_cents, -42_665);
}

#[tokio::test]
async fn a_refund_only_category_shows_zero_and_logs_the_credit() {
    let rows = apply_refunds(
        Vec::new(),
        &[PositiveRow {
            category: "Shipping".to_string(),
            amount_cents: 250,
        }],
        &[],
        &WeeklySpendingConfig::default().exclude,
    );
    assert_eq!(rows[0].total_cents, 250);
    let outcome = run_weekly(
        &WeeklySpendingConfig::default(),
        at(),
        7,
        0,
        Arc::new(Rows(rows)),
        Arc::new(PushHub::inert()),
        RunMode::DryRun,
    )
    .await
    .expect("credit");
    assert_eq!(outcome.unmapped, vec!["Shipping -$2.50".to_string()]);
    assert!(
        outcome
            .message
            .contains("everything else $0 of $110 ($110 left)"),
        "{}",
        outcome.message
    );
    assert!(
        outcome.message.contains("groceries $0 of $240 ($240 left)"),
        "{}",
        outcome.message
    );
}

#[tokio::test]
async fn positive_pages_stop_after_a_short_page() {
    let server = wiremock::MockServer::start().await;
    mount(
        &server,
        "/api/v1/transactions",
        serde_json::json!({
            "transactions": [tx("Alpha", 10), tx("Beta", 20)]
        }),
        &[
            ("start_date", "2026-10-12"),
            ("end_date", "2026-10-18"),
            ("min_amount_cents", "1"),
            ("limit", "2"),
            ("offset", "0"),
        ],
    )
    .await;
    mount(
        &server,
        "/api/v1/transactions",
        serde_json::json!({ "transactions": [tx("Gamma", 30)] }),
        &[
            ("start_date", "2026-10-12"),
            ("end_date", "2026-10-18"),
            ("min_amount_cents", "1"),
            ("limit", "2"),
            ("offset", "2"),
        ],
    )
    .await;
    let client = reqwest::Client::new();
    let rows = BudgetHttp::new(&client, &server.uri(), KEY)
        .collect_pages(day(12), day(18), 2)
        .await
        .expect("pages");
    server.verify().await;
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].category, "Alpha");
    assert_eq!(rows[0].amount_cents, 10);
    assert_eq!(rows[1].category, "Beta");
    assert_eq!(rows[2].category, "Gamma");
    assert_eq!(rows[2].amount_cents, 30);
}

#[test]
fn a_null_category_is_an_empty_name_and_a_nameless_spend_row_is_skipped() {
    let rows = parse_transactions(&serde_json::json!({
        "transactions": [{
            "category": null,
            "amount_cents": 10,
            "payee": "payee-should-stay-out",
            "account_name": "account-should-stay-out"
        }]
    }))
    .expect("rows");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].category, "");
    assert_eq!(rows[0].amount_cents, 10);
    let spent = parse_categories(&serde_json::json!({
        "categories": [
            {"total_cents": -5},
            {"category_name": "Groceries", "total_cents": -10}
        ]
    }));
    assert_eq!(spent.len(), 1);
    assert_eq!(spent[0].total_cents, -10);
    assert!(parse_transactions(&serde_json::json!({})).is_err());
    assert!(collect_income(&serde_json::json!({})).is_err());
}

#[tokio::test]
async fn a_failed_lookup_is_the_status_and_not_the_body() {
    assert_eq!(
        failed("/api/v1/spending/by-category").await,
        "budget API status 500"
    );
    assert_eq!(failed("/api/v1/categories").await, "budget API status 500");
    assert_eq!(
        failed("/api/v1/transactions").await,
        "budget API status 500"
    );
}

async fn failed(path: &str) -> String {
    let server = wiremock::MockServer::start().await;
    mount_status(&server, "/api/v1/spending/by-category", path).await;
    mount_status(&server, "/api/v1/categories", path).await;
    mount_status(&server, "/api/v1/transactions", path).await;
    let client = HttpBudget::from_parts(&server.uri(), KEY).expect("client");
    let err = client
        .spending(day(12), day(18), &[])
        .await
        .expect_err("status");
    assert!(!err.contains("do-not-echo-this-body"), "{err}");
    assert!(!err.contains(KEY), "{err}");
    err
}

async fn mount_status(server: &wiremock::MockServer, path: &str, fail_path: &str) {
    let failing = path == fail_path;
    let status = if failing { 500 } else { 200 };
    let body = if failing {
        serde_json::json!({"error": "do-not-echo-this-body"})
    } else {
        empty_body(path)
    };
    wiremock::Mock::given(wiremock::matchers::method("GET"))
        .and(wiremock::matchers::path(path))
        .respond_with(wiremock::ResponseTemplate::new(status).set_body_json(body))
        .mount(server)
        .await;
}

fn empty_body(path: &str) -> serde_json::Value {
    if path.ends_with("categories") && !path.contains("by-category") {
        return serde_json::json!({"category_groups": []});
    }
    if path.ends_with("transactions") {
        return serde_json::json!({"transactions": []});
    }
    serde_json::json!({"categories": []})
}

#[tokio::test]
async fn a_bad_payload_names_the_gap() {
    let not_json = shape_error(
        wiremock::ResponseTemplate::new(200).set_body_string("not-json"),
        true,
    )
    .await;
    assert_eq!(not_json, "budget API response was not JSON");
    let no_groups = shape_error(
        wiremock::ResponseTemplate::new(200).set_body_json(serde_json::json!({})),
        true,
    )
    .await;
    assert_eq!(no_groups, "budget API categories have no groups");
    let no_rows = shape_error(
        wiremock::ResponseTemplate::new(200).set_body_json(live_groups()),
        false,
    )
    .await;
    assert_eq!(no_rows, "budget API transactions have no rows");
}

async fn shape_error(categories: wiremock::ResponseTemplate, categories_fail: bool) -> String {
    let server = wiremock::MockServer::start().await;
    wiremock::Mock::given(wiremock::matchers::method("GET"))
        .and(wiremock::matchers::path("/api/v1/spending/by-category"))
        .respond_with(
            wiremock::ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"categories": []})),
        )
        .mount(&server)
        .await;
    wiremock::Mock::given(wiremock::matchers::method("GET"))
        .and(wiremock::matchers::path("/api/v1/categories"))
        .respond_with(categories)
        .mount(&server)
        .await;
    if !categories_fail {
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/api/v1/transactions"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .mount(&server)
            .await;
    }
    let client = HttpBudget::from_parts(&server.uri(), KEY).expect("client");
    client
        .spending(day(12), day(18), &[])
        .await
        .expect_err("shape")
}
