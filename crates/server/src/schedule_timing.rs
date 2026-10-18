//! Code-computed fire times shared by schedule tools and approval cards.
//!
//! Weekday and clock come from [`liberado_cron::next_fires`]. Callers do not
//! read names, comments, or labels to decide the day.

use liberado_config::CronSchedule;
use liberado_cron::FireInstant;
use serde_json::{Value, json};

/// How one row should be shown: expression, zone, and English.
pub struct ShownSchedule {
    pub expr: String,
    pub zone: String,
    pub civil: bool,
    pub english: String,
}

/// Compile `schedule` against `topology_tz` and describe it.
pub fn show(schedule: &CronSchedule, topology_tz: &str) -> Result<ShownSchedule, String> {
    let timing = schedule.timing(topology_tz)?;
    let civil = timing.civil_zone.is_some();
    let zone = timing
        .civil_zone
        .unwrap_or_else(|| zone_name(topology_tz).to_string());
    let english = english_for(&timing.expr, &zone, civil);
    Ok(ShownSchedule {
        expr: timing.expr,
        zone,
        civil,
        english,
    })
}

fn english_for(expr: &str, zone: &str, civil: bool) -> String {
    if civil {
        liberado_cron::cron_to_english_civil(expr, zone)
    } else {
        liberado_cron::cron_to_english(expr, zone)
    }
}

fn upcoming(shown: &ShownSchedule, n: usize) -> Result<Vec<FireInstant>, String> {
    if shown.civil {
        liberado_cron::next_fires_zoned(&shown.expr, &shown.zone, n)
    } else {
        liberado_cron::next_fires(&shown.expr, &shown.zone, n)
    }
}

/// The next `n` fires of a compiled row.
pub fn rows_for(shown: &ShownSchedule, n: usize) -> Vec<Value> {
    match upcoming(shown, n) {
        Ok(fires) => fires.iter().map(fire_row).collect(),
        Err(_) => Vec::new(),
    }
}

/// Five upcoming fires of a compiled row, one per line.
pub fn lines_for(shown: &ShownSchedule) -> String {
    match upcoming(shown, 5) {
        Ok(fires) if !fires.is_empty() => format_lines(&fires),
        _ => "Next: unavailable\n".to_string(),
    }
}

fn format_lines(fires: &[FireInstant]) -> String {
    let mut text = String::from("Next:\n");
    for fire in fires {
        text.push_str(&format!(
            "- {} (UTC {})\n",
            fire.display(),
            fire.utc.to_rfc3339()
        ));
    }
    text
}

/// `America/Chicago` when the file has no timezone.
pub fn zone_name(timezone: &str) -> &str {
    let name = timezone.trim();
    if name.is_empty() {
        "America/Chicago"
    } else {
        name
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
    format_lines(&fires)
}

fn fire_row(fire: &FireInstant) -> Value {
    json!({
        "weekday": fire.weekday_name(),
        "local": fire.local.to_rfc3339(),
        "utc": fire.utc.to_rfc3339(),
        "display": fire.display(),
    })
}
