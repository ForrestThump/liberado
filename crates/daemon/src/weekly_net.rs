//! Net refunds into category spend, and format the weekly lines.
//!
//! By-category is the spend side. It already drops transfers, split parents,
//! split rows, and positive amounts. A positive transaction reduces its category
//! when the name is set, is not Uncategorized, is not income, and is not excluded.
//! The log records a count of skipped positive rows. It does not record payee,
//! account, key, or URL.

use chrono::NaiveDate;

use crate::weekly_spending::{CategorySpend, round_dollars};

const TRANSACTION_LIMIT: u32 = 2_000;
const CATEGORIES_PATH: &str = "/api/v1/categories";

/// One positive transaction. Only the category name and the cents are kept.
struct PositiveRow {
    category: String,
    amount_cents: i64,
}

/// Budget HTTP calls for one weekly run. The key is a header and is not logged.
pub(crate) struct BudgetHttp<'a> {
    client: &'a reqwest::Client,
    base: &'a str,
    key: &'a str,
}

impl<'a> BudgetHttp<'a> {
    pub(crate) fn new(client: &'a reqwest::Client, base: &'a str, key: &'a str) -> Self {
        Self { client, base, key }
    }

    pub(crate) async fn fetch(
        &self,
        start: NaiveDate,
        end: NaiveDate,
        exclude: &[String],
    ) -> Result<Vec<CategorySpend>, String> {
        let spent = self.load_spent(start, end).await?;
        self.with_refunds(spent, start, end, exclude).await
    }

    async fn with_refunds(
        &self,
        spent: Vec<CategorySpend>,
        start: NaiveDate,
        end: NaiveDate,
        exclude: &[String],
    ) -> Result<Vec<CategorySpend>, String> {
        let income = self.load_income().await?;
        let positives = self.load_positives(start, end).await?;
        Ok(apply_refunds(spent, &positives, &income, exclude))
    }

    async fn load_spent(
        &self,
        start: NaiveDate,
        end: NaiveDate,
    ) -> Result<Vec<CategorySpend>, String> {
        let body = self.get_json(&spent_path(start, end)).await?;
        Ok(parse_categories(&body))
    }

    async fn load_income(&self) -> Result<Vec<String>, String> {
        let body = self.get_json(CATEGORIES_PATH).await?;
        collect_income(&body)
    }

    async fn load_positives(
        &self,
        start: NaiveDate,
        end: NaiveDate,
    ) -> Result<Vec<PositiveRow>, String> {
        self.collect_pages(start, end, TRANSACTION_LIMIT).await
    }

    async fn collect_pages(
        &self,
        start: NaiveDate,
        end: NaiveDate,
        limit: u32,
    ) -> Result<Vec<PositiveRow>, String> {
        let mut cursor = PageCursor::start();
        while !cursor.done {
            self.pull_page(&mut cursor, start, end, limit).await?;
        }
        Ok(cursor.rows)
    }

    async fn pull_page(
        &self,
        cursor: &mut PageCursor,
        start: NaiveDate,
        end: NaiveDate,
        limit: u32,
    ) -> Result<(), String> {
        let page = self.one_page(start, end, limit, cursor.offset).await?;
        cursor.done = page_is_short(page.len(), limit);
        cursor.offset = next_offset(cursor.offset, limit);
        cursor.rows.extend(page);
        Ok(())
    }

    async fn one_page(
        &self,
        start: NaiveDate,
        end: NaiveDate,
        limit: u32,
        offset: u32,
    ) -> Result<Vec<PositiveRow>, String> {
        let body = self
            .get_json(&transaction_path(start, end, limit, offset))
            .await?;
        parse_transactions(&body)
    }

    async fn get_json(&self, path: &str) -> Result<serde_json::Value, String> {
        let response = self.send(path).await?;
        read_json(response).await
    }

    async fn send(&self, path: &str) -> Result<reqwest::Response, String> {
        let url = format!("{}{path}", self.base);
        self.client
            .get(url)
            .header("X-API-Key", self.key)
            .send()
            .await
            .map_err(transport_error)
    }
}

struct PageCursor {
    offset: u32,
    rows: Vec<PositiveRow>,
    done: bool,
}

impl PageCursor {
    fn start() -> Self {
        Self {
            offset: 0,
            rows: Vec::new(),
            done: false,
        }
    }
}

fn page_is_short(len: usize, limit: u32) -> bool {
    limit == 0 || len < limit as usize
}

fn next_offset(offset: u32, limit: u32) -> u32 {
    offset.saturating_add(limit.max(1))
}

fn spent_path(start: NaiveDate, end: NaiveDate) -> String {
    format!("/api/v1/spending/by-category?start_date={start}&end_date={end}")
}

fn transaction_path(start: NaiveDate, end: NaiveDate, limit: u32, offset: u32) -> String {
    format!(
        "/api/v1/transactions?start_date={start}&end_date={end}&min_amount_cents=1&limit={limit}&offset={offset}"
    )
}

async fn read_json(response: reqwest::Response) -> Result<serde_json::Value, String> {
    let code = response.status();
    if !code.is_success() {
        return Err(format!("budget API status {}", code.as_u16()));
    }
    response.json().await.map_err(json_error)
}

fn json_error(_err: reqwest::Error) -> String {
    "budget API response was not JSON".to_string()
}

fn transport_error(err: reqwest::Error) -> String {
    liberado_common::redact_secrets(&err.without_url().to_string())
}

fn parse_categories(value: &serde_json::Value) -> Vec<CategorySpend> {
    let mut out = Vec::new();
    let Some(rows) = value
        .get("categories")
        .and_then(serde_json::Value::as_array)
    else {
        return out;
    };
    for row in rows {
        push_category(&mut out, row);
    }
    out
}

fn push_category(out: &mut Vec<CategorySpend>, row: &serde_json::Value) {
    let Some(spend) = one_category(row) else {
        return;
    };
    out.push(spend);
}

fn one_category(row: &serde_json::Value) -> Option<CategorySpend> {
    Some(CategorySpend {
        name: required_text(row, "category_name")?,
        total_cents: required_i64(row, "total_cents")?,
    })
}

fn required_text(row: &serde_json::Value, key: &str) -> Option<String> {
    Some(row.get(key)?.as_str()?.to_string())
}

fn required_i64(row: &serde_json::Value, key: &str) -> Option<i64> {
    row.get(key)?.as_i64()
}

fn parse_transactions(value: &serde_json::Value) -> Result<Vec<PositiveRow>, String> {
    let mut out = Vec::new();
    for row in transaction_rows(value)? {
        push_positive(&mut out, row);
    }
    Ok(out)
}

fn transaction_rows(value: &serde_json::Value) -> Result<&Vec<serde_json::Value>, String> {
    match value
        .get("transactions")
        .and_then(serde_json::Value::as_array)
    {
        Some(rows) => Ok(rows),
        None => Err("budget API transactions have no rows".to_string()),
    }
}

fn push_positive(out: &mut Vec<PositiveRow>, row: &serde_json::Value) {
    let Some(amount_cents) = row.get("amount_cents").and_then(serde_json::Value::as_i64) else {
        return;
    };
    out.push(PositiveRow {
        category: text_field(row, "category"),
        amount_cents,
    });
}

fn text_field(row: &serde_json::Value, key: &str) -> String {
    row.get(key)
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
        .to_string()
}

fn collect_income(value: &serde_json::Value) -> Result<Vec<String>, String> {
    let mut names = Vec::new();
    for group in group_list(value)? {
        collect_group(&mut names, group);
    }
    Ok(names)
}

fn group_list(value: &serde_json::Value) -> Result<&Vec<serde_json::Value>, String> {
    match value
        .get("category_groups")
        .and_then(serde_json::Value::as_array)
    {
        Some(groups) => Ok(groups),
        None => Err("budget API categories have no groups".to_string()),
    }
}

fn collect_group(names: &mut Vec<String>, group: &serde_json::Value) {
    let group_income = bool_field(group, "is_income");
    let Some(categories) = group
        .get("categories")
        .and_then(serde_json::Value::as_array)
    else {
        return;
    };
    for category in categories {
        remember_income(names, category, group_income);
    }
}

fn remember_income(names: &mut Vec<String>, category: &serde_json::Value, group_income: bool) {
    let Some(name) = income_label(category, group_income) else {
        return;
    };
    if listed(names, name) {
        return;
    }
    names.push(name.to_string());
}

fn income_label(category: &serde_json::Value, group_income: bool) -> Option<&str> {
    if !is_income_category(category, group_income) {
        return None;
    }
    present_name(category)
}

fn is_income_category(category: &serde_json::Value, group_income: bool) -> bool {
    group_income || bool_field(category, "is_income")
}

fn present_name(category: &serde_json::Value) -> Option<&str> {
    let name = category.get("name")?.as_str()?.trim();
    if name.is_empty() { None } else { Some(name) }
}

fn bool_field(value: &serde_json::Value, key: &str) -> bool {
    value
        .get(key)
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
}

fn apply_refunds(
    spent: Vec<CategorySpend>,
    positives: &[PositiveRow],
    income: &[String],
    exclude: &[String],
) -> Vec<CategorySpend> {
    let (rows, skipped) = fold_refunds(spent, positives, income, exclude);
    log_skipped(skipped);
    rows
}

fn log_skipped(skipped: usize) {
    if skipped == 0 {
        return;
    }
    tracing::info!(
        skipped,
        "weekly spending skipped positive rows with no spend category"
    );
}

fn fold_refunds(
    mut spent: Vec<CategorySpend>,
    positives: &[PositiveRow],
    income: &[String],
    exclude: &[String],
) -> (Vec<CategorySpend>, usize) {
    let mut skipped = 0usize;
    for row in positives {
        skipped += one_refund(&mut spent, row, income, exclude);
    }
    (spent, skipped)
}

fn one_refund(
    spent: &mut Vec<CategorySpend>,
    row: &PositiveRow,
    income: &[String],
    exclude: &[String],
) -> usize {
    let Some(name) = refund_name(row, income, exclude) else {
        return blank_skip(row);
    };
    add_refund(spent, name, row.amount_cents);
    0
}

fn refund_name<'b>(row: &'b PositiveRow, income: &[String], exclude: &[String]) -> Option<&'b str> {
    let name = charged_name(row)?;
    if listed(income, name) || listed(exclude, name) {
        return None;
    }
    Some(name)
}

fn charged_name(row: &PositiveRow) -> Option<&str> {
    if row.amount_cents <= 0 {
        return None;
    }
    let name = row.category.trim();
    if is_blank_category(name) {
        return None;
    }
    Some(name)
}

fn blank_skip(row: &PositiveRow) -> usize {
    if row.amount_cents <= 0 {
        return 0;
    }
    usize::from(is_blank_category(row.category.trim()))
}

fn is_blank_category(name: &str) -> bool {
    name.is_empty() || name == "Uncategorized"
}

fn add_refund(spent: &mut Vec<CategorySpend>, name: &str, cents: i64) {
    if let Some(index) = find_name(spent, name) {
        add_cents(spent, index, cents);
        return;
    }
    spent.push(CategorySpend {
        name: name.to_string(),
        total_cents: cents,
    });
}

fn add_cents(spent: &mut [CategorySpend], index: usize, cents: i64) {
    if let Some(row) = spent.get_mut(index) {
        row.total_cents = row.total_cents.saturating_add(cents);
    }
}

fn find_name(spent: &[CategorySpend], name: &str) -> Option<usize> {
    for (index, row) in spent.iter().enumerate() {
        if row.name.trim() == name {
            return Some(index);
        }
    }
    None
}

fn listed(names: &[String], name: &str) -> bool {
    for item in names {
        if item.trim() == name {
            return true;
        }
    }
    false
}

/// Whole dollars for a bucket. A net credit is shown as zero.
pub(crate) fn shown_dollars(spent_cents: i64) -> i64 {
    round_dollars(spent_cents.max(0))
}

/// Signed cents, for example `Shipping $5.55` or `Shipping -$2.50`.
pub(crate) fn cents_label(cents: i64) -> String {
    format!("{}{}", sign_of(cents), unsigned_cents(cents.unsigned_abs()))
}

fn sign_of(cents: i64) -> &'static str {
    if cents < 0 { "-" } else { "" }
}

fn unsigned_cents(abs: u64) -> String {
    format!("${}.{:02}", abs / 100, abs % 100)
}

pub(crate) fn bucket_clause(name: &str, spent_dollars: i64, budget_dollars: u32) -> String {
    let budget = i64::from(budget_dollars);
    format!(
        "{} {} of {} {}",
        name.to_lowercase(),
        whole_money(spent_dollars),
        whole_money(budget),
        gap_note(spent_dollars, budget)
    )
}

fn gap_note(spent: i64, budget: i64) -> String {
    if spent > budget {
        return format!("(over by {})", whole_money(spent - budget));
    }
    format!("({} left)", whole_money(budget - spent))
}

fn whole_money(dollars: i64) -> String {
    format!("${dollars}")
}

#[cfg(test)]
#[path = "weekly_net_tests.rs"]
mod weekly_net_tests;
