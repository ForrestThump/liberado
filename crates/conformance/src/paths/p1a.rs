//! P1a — cron liveness (read-only).

use std::path::Path;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use cron::Schedule;
use std::str::FromStr;

use crate::client::DaemonClient;
use crate::config::ConformanceConfig;
use crate::paths::elapsed_ms;
use crate::result::{PathId, PathResult};

pub async fn run(client: &DaemonClient, cfg: &ConformanceConfig, _timeout: Duration) -> PathResult {
    let start = Instant::now();
    let status = match client.status().await {
        Ok(s) => s,
        Err(e) => {
            return PathResult::fail(
                PathId::P1a,
                "GET /api/status",
                elapsed_ms(start),
                serde_json::json!({"error": e}),
            );
        }
    };

    let topology_path = match &cfg.topology_path {
        Some(p) => p.clone(),
        None => {
            return PathResult::skipped(
                PathId::P1a,
                "topology_path not set in conformance.toml — cannot list schedules",
            );
        }
    };

    let schedules = match load_enabled_user_schedules(&topology_path) {
        Ok(s) => s,
        Err(e) => {
            return PathResult::fail(
                PathId::P1a,
                "load topology schedules",
                elapsed_ms(start),
                serde_json::json!({"error": e}),
            );
        }
    };

    if schedules.is_empty() {
        return PathResult::skipped(PathId::P1a, "no enabled non-suite schedules in topology");
    }

    let reactions = match client.reactions().await {
        Ok(r) => r,
        Err(e) => {
            return PathResult::fail(
                PathId::P1a,
                "GET /api/reactions",
                elapsed_ms(start),
                serde_json::json!({"error": e}),
            );
        }
    };

    let mut failures = Vec::new();
    let mut checked = Vec::new();
    let now = Utc::now();

    for sched in &schedules {
        let period = match estimate_period_secs(&sched.cron_expr) {
            Some(p) => p,
            None => {
                failures.push(serde_json::json!({
                    "schedule": sched.name,
                    "error": "could not estimate period from cron_expr",
                    "cron_expr": sched.cron_expr,
                }));
                continue;
            }
        };
        let threshold = (period as f64 * 1.5) as u64;
        // Restart gate: ring is empty after deploy.
        if status.uptime_seconds < threshold {
            checked.push(serde_json::json!({
                "schedule": sched.name,
                "status": "skipped_uptime",
                "uptime_seconds": status.uptime_seconds,
                "threshold_secs": threshold,
            }));
            continue;
        }

        let source = format!("cron:{}", sched.name);
        let newest = reactions
            .iter()
            .filter(|r| r.source == source)
            .filter_map(|r| DateTime::parse_from_rfc3339(&r.timestamp).ok())
            .map(|dt| dt.with_timezone(&Utc))
            .max();

        match newest {
            Some(ts) if (now - ts).num_seconds() as u64 <= threshold => {
                checked.push(serde_json::json!({
                    "schedule": sched.name,
                    "status": "ok",
                    "last_reaction": ts.to_rfc3339(),
                    "threshold_secs": threshold,
                }));
            }
            Some(ts) => {
                failures.push(serde_json::json!({
                    "schedule": sched.name,
                    "status": "stale",
                    "last_reaction": ts.to_rfc3339(),
                    "threshold_secs": threshold,
                }));
            }
            None => {
                failures.push(serde_json::json!({
                    "schedule": sched.name,
                    "status": "missing",
                    "threshold_secs": threshold,
                }));
            }
        }
    }

    // If everything was uptime-skipped, report skip rather than a hollow pass.
    if failures.is_empty()
        && checked
            .iter()
            .all(|c| c.get("status").and_then(|s| s.as_str()) == Some("skipped_uptime"))
    {
        return PathResult::skipped(
            PathId::P1a,
            format!(
                "daemon uptime_seconds={} shorter than every schedule's 1.5× period — restart gate",
                status.uptime_seconds
            ),
        );
    }

    if failures.is_empty() {
        PathResult::pass(
            PathId::P1a,
            "enabled user schedules have recent reactions (uptime-gated)",
            elapsed_ms(start),
            serde_json::json!({
                "uptime_seconds": status.uptime_seconds,
                "checked": checked,
            }),
        )
    } else {
        PathResult::fail(
            PathId::P1a,
            "one or more schedules missing or stale in /api/reactions",
            elapsed_ms(start),
            serde_json::json!({
                "uptime_seconds": status.uptime_seconds,
                "checked": checked,
                "failures": failures,
            }),
        )
    }
}

#[derive(Debug)]
struct ScheduleRow {
    name: String,
    cron_expr: String,
}

fn load_enabled_user_schedules(path: &Path) -> Result<Vec<ScheduleRow>, String> {
    let raw = std::fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let value: toml::Value = raw
        .parse()
        .map_err(|e| format!("parse {}: {e}", path.display()))?;
    let mut out = Vec::new();
    let Some(arr) = value.get("schedules").and_then(|v| v.as_array()) else {
        return Ok(out);
    };
    for item in arr {
        let name = item
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if name.is_empty() || ConformanceConfig::is_suite_owned(&name) {
            continue;
        }
        let enabled = item
            .get("enabled")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);
        if !enabled {
            continue;
        }
        let cron_expr = item
            .get("cron_expr")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        out.push(ScheduleRow { name, cron_expr });
    }
    Ok(out)
}

/// Estimate schedule period in seconds from two successive firings after now.
fn estimate_period_secs(cron_expr: &str) -> Option<u64> {
    let schedule = Schedule::from_str(cron_expr).ok()?;
    let mut upcoming = schedule.after(&Utc::now()).take(2);
    let a = upcoming.next()?;
    let b = upcoming.next()?;
    let secs = (b - a).num_seconds();
    if secs <= 0 { None } else { Some(secs as u64) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn estimate_daily_period() {
        // 11:55 UTC daily
        let p = estimate_period_secs("0 55 11 * * * *").expect("parse");
        assert!((86_000..90_000).contains(&p), "got {p}");
    }

    #[test]
    fn suite_schedules_excluded_from_loader_logic() {
        assert!(ConformanceConfig::is_suite_owned("conformance"));
    }
}
