//! Fire times for a cron expression whose fields are civil time in an IANA zone.

use std::str::FromStr;

use chrono::{DateTime, Utc};
use chrono_tz::Tz;

use crate::fire_at::next_utc;
use crate::next_fires::{FireInstant, stamp};

/// The next `n` civil-time fires of `expr` after now, evaluated in `tz`.
pub fn next_fires_zoned(expr: &str, tz: &str, n: usize) -> Result<Vec<FireInstant>, String> {
    next_fires_zoned_after(expr, tz, n, Utc::now())
}

/// [`next_fires_zoned`] starting strictly after `after`.
pub fn next_fires_zoned_after(
    expr: &str,
    tz: &str,
    n: usize,
    after: DateTime<Utc>,
) -> Result<Vec<FireInstant>, String> {
    if n == 0 {
        return Ok(Vec::new());
    }
    let zone = require_zone(tz)?;
    let parsed = cron::Schedule::from_str(expr.trim()).map_err(|err| err.to_string())?;
    collect_fires(&parsed, zone, n, after)
}

fn require_zone(tz: &str) -> Result<Tz, String> {
    let text = tz.trim();
    if text.is_empty() {
        return Err("timezone is empty".into());
    }
    text.parse::<Tz>()
        .map_err(|_| format!("unknown timezone '{text}'"))
}

fn collect_fires(
    parsed: &cron::Schedule,
    zone: Tz,
    n: usize,
    after: DateTime<Utc>,
) -> Result<Vec<FireInstant>, String> {
    let mut cursor = after;
    let mut fires = Vec::with_capacity(n);
    while fires.len() < n {
        let Some(utc) = next_utc(parsed, Some(zone), cursor) else {
            break;
        };
        if utc <= cursor {
            break;
        }
        fires.push(stamp(utc, zone));
        cursor = utc;
    }
    Ok(fires)
}

#[cfg(test)]
#[path = "zoned_tests.rs"]
mod tests;
