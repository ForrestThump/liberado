//! Code-computed fire times shared by schedule tools and approval cards.
//!
//! Weekday and clock come from [`liberado_cron::next_fires`]. Callers do not
//! read names, comments, or labels to decide the day.

use liberado_cron::FireInstant;
use serde_json::{Value, json};

/// `America/Chicago` when the file has no timezone.
pub fn zone_name(timezone: &str) -> &str {
    let name = timezone.trim();
    if name.is_empty() {
        "America/Chicago"
    } else {
        name
    }
}

/// The next `n` fires as JSON objects: weekday, local, utc, display.
pub fn next_fire_rows(expr: &str, timezone: &str, n: usize) -> Vec<Value> {
    match liberado_cron::next_fires(expr, zone_name(timezone), n) {
        Ok(fires) => fires.iter().map(fire_row).collect(),
        Err(_) => Vec::new(),
    }
}

/// Five upcoming local fires, one per line, with the UTC instant beside each.
pub fn fire_lines(expr: &str, timezone: &str) -> String {
    let Ok(fires) = liberado_cron::next_fires(expr, zone_name(timezone), 5) else {
        return "Next: unavailable\n".to_string();
    };
    if fires.is_empty() {
        return "Next: unavailable\n".to_string();
    }
    let mut text = String::from("Next:\n");
    for fire in &fires {
        text.push_str(&format!(
            "- {} (UTC {})\n",
            fire.display(),
            fire.utc.to_rfc3339()
        ));
    }
    text
}

fn fire_row(fire: &FireInstant) -> Value {
    json!({
        "weekday": fire.weekday_name(),
        "local": fire.local.to_rfc3339(),
        "utc": fire.utc.to_rfc3339(),
        "display": fire.display(),
    })
}
