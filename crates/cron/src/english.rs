//! Plain-English reading of a cron expression.
//!
//! Liberado stores 6- or 7-field expressions, seconds first, in UTC (the `cron` crate:
//! Sunday is 1). A 5-field string is Unix cron, minute first, with seconds taken as 0
//! (Sunday is 0 or 7). Clock times are converted into `timezone` at `at`. An expression
//! this module cannot describe exactly is left unchanged.

use std::str::FromStr;

use chrono::{DateTime, Utc};

mod parse;
mod phrase;

/// Read `expr` in `timezone` as of now. Odd or unsupported expressions return `expr`.
pub fn cron_to_english(expr: &str, timezone: &str) -> String {
    cron_to_english_at(expr, timezone, Utc::now())
}

/// [`cron_to_english`] at a fixed UTC instant, so tests can pin the offset.
pub fn cron_to_english_at(expr: &str, timezone: &str, at: DateTime<Utc>) -> String {
    describe(expr.trim(), timezone.trim(), at).unwrap_or_else(|| expr.to_string())
}

fn describe(raw: &str, timezone: &str, at: DateTime<Utc>) -> Option<String> {
    let (spec, five_field) = parse::parse_expression(raw)?;
    if !five_field && cron::Schedule::from_str(raw).is_err() {
        return None;
    }
    if let Some(text) = phrase::interval_text(&spec) {
        return Some(text);
    }
    phrase::clock_text(&spec, timezone, at)
}

#[cfg(test)]
use phrase::ordinal;

#[cfg(test)]
#[path = "english_tests.rs"]
mod tests;
