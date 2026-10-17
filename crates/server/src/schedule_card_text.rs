//! Plain-English before/after text for a schedule approval card.
//!
//! The stored rationale stays the fallback. This module replaces the card
//! summary only when the proposed row and, for a change or a delete, the
//! current row can both be read.

use liberado_common::{Proposal, ProposedAction};
use liberado_config::{CronSchedule, schedule_topology_path};

use crate::api::schedule_description::schedule_description;

/// Schedules on disk, plus the file's timezone.
pub struct ScheduleBook {
    pub timezone: String,
    pub schedules: Vec<CronSchedule>,
}

/// Read `topology.toml` when the config dir and the file are both present.
pub fn load_schedule_book() -> Option<ScheduleBook> {
    let path = schedule_topology_path().ok()?;
    let text = std::fs::read_to_string(path).ok()?;
    parse_schedule_book(&text)
}

pub fn parse_schedule_book(text: &str) -> Option<ScheduleBook> {
    let file: ScheduleFile = toml::from_str(text).ok()?;
    Some(ScheduleBook {
        timezone: file.timezone,
        schedules: file.schedules,
    })
}

/// Card summary for one schedule edit. `None` means use the stored rationale.
pub fn schedule_edit_summary(
    tool: &str,
    args: &serde_json::Value,
    book: &ScheduleBook,
) -> Option<String> {
    let name = args.get("name").and_then(|value| value.as_str())?.trim();
    if name.is_empty() {
        return None;
    }
    let current = book.schedules.iter().find(|row| row.name == name);
    match tool {
        "create_schedule" => create_summary(args, current, book),
        "update_schedule" => update_summary(args, current?, book),
        "delete_schedule" => Some(delete_summary(current?, book)),
        _ => None,
    }
}

/// Summary for a proposal that is a schedule edit. Other proposals return `None`.
pub fn proposal_schedule_summary(proposal: &Proposal, book: &ScheduleBook) -> Option<String> {
    let (tool, args) = tool_call(proposal)?;
    schedule_edit_summary(tool, args, book)
}

fn create_summary(
    args: &serde_json::Value,
    current: Option<&CronSchedule>,
    book: &ScheduleBook,
) -> Option<String> {
    let after = proposed_schedule(args)?;
    Some(pair(
        &before_line(current, book),
        &english_line(&after, book),
    ))
}

fn update_summary(
    args: &serde_json::Value,
    current: &CronSchedule,
    book: &ScheduleBook,
) -> Option<String> {
    let after = proposed_schedule(args)?;
    Some(pair(
        &english_line(current, book),
        &english_line(&after, book),
    ))
}

fn delete_summary(current: &CronSchedule, book: &ScheduleBook) -> String {
    pair(
        &english_line(current, book),
        "This automation will be removed.",
    )
}

fn pair(before: &str, after: &str) -> String {
    format!("Before: {before}\nAfter: {after}")
}

fn before_line(current: Option<&CronSchedule>, book: &ScheduleBook) -> String {
    match current {
        Some(row) => english_line(row, book),
        None => "This automation does not exist.".to_string(),
    }
}

fn english_line(schedule: &CronSchedule, book: &ScheduleBook) -> String {
    let state = if schedule.enabled {
        "Enabled"
    } else {
        "Disabled"
    };
    let when = liberado_cron::cron_to_english(&schedule.cron_expr, book.timezone.trim());
    let what = schedule_description(schedule);
    format!("{} — {state}. {when}. {what}", schedule.name)
}

fn proposed_schedule(args: &serde_json::Value) -> Option<CronSchedule> {
    let mut schedule: CronSchedule = serde_json::from_value(args.clone()).ok()?;
    if let Some(new_name) = args.get("new_name").and_then(|value| value.as_str()) {
        let trimmed = new_name.trim();
        if !trimmed.is_empty() {
            schedule.name = trimmed.to_string();
        }
    }
    if schedule.name.trim().is_empty() {
        return None;
    }
    Some(schedule)
}

fn tool_call(proposal: &Proposal) -> Option<(&str, &serde_json::Value)> {
    match &proposal.proposed_action {
        ProposedAction::ToolCalls(calls) => {
            calls.first().map(|call| (call.tool.as_str(), &call.args))
        }
        _ => None,
    }
}

#[derive(serde::Deserialize)]
struct ScheduleFile {
    #[serde(default)]
    timezone: String,
    #[serde(default)]
    schedules: Vec<CronSchedule>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn book(timezone: &str, schedules: Vec<CronSchedule>) -> ScheduleBook {
        ScheduleBook {
            timezone: timezone.into(),
            schedules,
        }
    }

    fn row(name: &str, cron: &str, job: Option<&str>) -> CronSchedule {
        serde_json::from_value(json!({
            "name": name,
            "enabled": true,
            "cron_expr": cron,
            "job": job,
        }))
        .unwrap()
    }

    #[test]
    fn an_update_shows_before_and_after_in_english() {
        let current = row("morning-tasks", "0 */15 * * * *", Some("task-ping"));
        let args = json!({
            "name": "morning-tasks",
            "enabled": true,
            "cron_expr": "0 */30 * * * *",
            "job": "task-ping",
        });
        let summary = schedule_edit_summary(
            "update_schedule",
            &args,
            &book("America/Chicago", vec![current]),
        )
        .unwrap();
        assert!(summary.starts_with("Before: "), "{summary}");
        assert!(summary.contains("\nAfter: "), "{summary}");
        assert!(summary.contains("Every 15 minutes"), "{summary}");
        assert!(summary.contains("Every 30 minutes"), "{summary}");
        assert!(summary.contains("open vault tasks"), "{summary}");
    }

    #[test]
    fn a_create_with_no_current_row_says_it_does_not_exist() {
        let args = json!({
            "name": "fresh",
            "enabled": false,
            "cron_expr": "0 */15 * * * *",
            "job": "event-ping",
        });
        let summary =
            schedule_edit_summary("create_schedule", &args, &book("America/Chicago", vec![]))
                .unwrap();
        assert!(summary.contains("Before: This automation does not exist."));
        assert!(summary.contains("After: fresh — Disabled."));
        assert!(summary.contains("Every 15 minutes"));
        assert!(summary.contains("calendar events"));
    }

    #[test]
    fn a_delete_names_the_current_row_and_says_it_will_be_removed() {
        let current = row("morning-tasks", "0 */15 * * * *", Some("habit-ping"));
        let args = json!({ "name": "morning-tasks" });
        let summary = schedule_edit_summary(
            "delete_schedule",
            &args,
            &book("America/Chicago", vec![current]),
        )
        .unwrap();
        assert!(summary.contains("Before: morning-tasks"));
        assert!(summary.contains("After: This automation will be removed."));
    }

    #[test]
    fn a_partial_update_falls_back_when_the_row_or_the_proposal_cannot_be_read() {
        let current = row("morning-tasks", "0 */15 * * * *", Some("task-ping"));
        let book = book("America/Chicago", vec![current]);
        assert!(
            schedule_edit_summary("update_schedule", &json!({"name": "missing"}), &book).is_none()
        );
        assert!(
            schedule_edit_summary("update_schedule", &json!({"name": "morning-tasks"}), &book)
                .is_none()
        );
        assert!(
            schedule_edit_summary("list_schedules", &json!({"name": "morning-tasks"}), &book)
                .is_none()
        );
        assert!(
            schedule_edit_summary("delete_schedule", &json!({"name": "gone"}), &book).is_none()
        );
    }

    #[test]
    fn a_book_parses_timezone_and_rows() {
        let text = "timezone = \"America/Chicago\"\n\n[[schedules]]\nname = \"tick\"\nenabled = true\ncron_expr = \"0 */15 * * * *\"\n";
        let book = parse_schedule_book(text).unwrap();
        assert_eq!(book.timezone, "America/Chicago");
        assert_eq!(book.schedules.len(), 1);
        assert_eq!(book.schedules[0].name, "tick");
    }
}
