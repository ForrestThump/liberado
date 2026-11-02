//! Path parsing and view-model helpers for notification detail pages.

use crate::components::sidebar::Route;

const TASK_DUE: &str = "task_due";
const EVENT_START: &str = "event_start";
const TASK_LIST: &str = "task_list";
const EVENT_LIST: &str = "event_list";
const HABIT: &str = "habit";
const JOB_ERROR: &str = "job_error";
const TEST: &str = "test";
const REMINDER: &str = "reminder";

/// Parse a browser pathname into a notifications route.
pub fn route_from_path(path: &str) -> Option<Route> {
    let trimmed = path.trim();
    if trimmed == "/notifications" || trimmed == "/notifications/" {
        return Some(Route::Notifications);
    }
    let rest = trimmed.strip_prefix("/notifications/")?;
    if rest.contains('/') || rest.is_empty() {
        return None;
    }
    let id = rest.parse::<i64>().ok()?;
    if id <= 0 {
        return None;
    }
    Some(Route::NotificationDetail(id))
}

pub fn path_for_route(route: Route) -> Option<String> {
    match route {
        Route::Notifications => Some("/notifications".into()),
        Route::NotificationDetail(id) => Some(format!("/notifications/{id}")),
        _ => None,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetailActions {
    pub show_complete: bool,
    pub show_snooze: bool,
    pub show_obsidian: bool,
    pub complete_label: &'static str,
}

pub fn resolution_state_text(
    resolution: Option<&str>,
    snooze_until_local: Option<&str>,
) -> Option<String> {
    match resolution {
        Some("completed") => Some("Completed".into()),
        Some("dismissed") => Some("Dismissed".into()),
        Some("snoozed") => {
            let until = snooze_until_local.unwrap_or("later");
            Some(format!("Snoozed until {until}"))
        }
        _ => None,
    }
}

pub fn detail_actions(kind: &str, source_type: Option<&str>, resolved: bool) -> DetailActions {
    if resolved {
        return DetailActions {
            show_complete: false,
            show_snooze: false,
            show_obsidian: source_type == Some("vault_note"),
            complete_label: "Mark complete",
        };
    }
    let vault_note = source_type == Some("vault_note");
    let show_complete = kind == TASK_DUE && vault_note
        || matches!(
            kind,
            EVENT_START | HABIT | TASK_LIST | EVENT_LIST | JOB_ERROR | TEST | REMINDER
        );
    let complete_label = if kind == TASK_DUE && vault_note {
        "Mark complete"
    } else {
        "Dismiss"
    };
    DetailActions {
        show_complete,
        show_snooze: kind != TEST,
        show_obsidian: vault_note,
        complete_label,
    }
}

pub fn why_now_text(late: bool, catch_up: bool, due_at_local: &str) -> String {
    if late || catch_up {
        format!("Missed — was due at {due_at_local}")
    } else {
        format!("Due at {due_at_local}")
    }
}

pub fn what_kind_label(kind: &str) -> &'static str {
    match kind {
        TASK_DUE => "Task due",
        EVENT_START => "Event starting",
        HABIT => "Habit",
        TASK_LIST => "Task list",
        EVENT_LIST => "Event list",
        JOB_ERROR => "Schedule error",
        TEST => "Test notification",
        _ => "Reminder",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_parsing_maps_notifications_routes() {
        assert_eq!(
            route_from_path("/notifications"),
            Some(Route::Notifications)
        );
        assert_eq!(
            route_from_path("/notifications/42"),
            Some(Route::NotificationDetail(42))
        );
        assert_eq!(
            route_from_path("/notifications/"),
            Some(Route::Notifications)
        );
        assert_eq!(route_from_path("/notifications/0"), None);
        assert_eq!(route_from_path("/notifications/abc"), None);
        assert_eq!(route_from_path("/notifications/42/extra"), None);
    }

    #[test]
    fn detail_actions_per_kind_and_source() {
        let task = detail_actions(TASK_DUE, Some("vault_note"), false);
        assert!(task.show_complete);
        assert!(task.show_obsidian);
        assert_eq!(task.complete_label, "Mark complete");

        let habit = detail_actions(HABIT, Some("schedule"), false);
        assert!(habit.show_complete);
        assert!(!habit.show_obsidian);
        assert_eq!(habit.complete_label, "Dismiss");

        let test = detail_actions(TEST, Some("settings"), false);
        assert!(test.show_complete);
        assert!(!test.show_snooze);
    }

    #[test]
    fn resolution_state_text_maps_owner_outcomes() {
        assert_eq!(
            resolution_state_text(Some("completed"), None),
            Some("Completed".into())
        );
        assert_eq!(
            resolution_state_text(Some("dismissed"), None),
            Some("Dismissed".into())
        );
        assert_eq!(
            resolution_state_text(Some("snoozed"), Some("3:15 PM")),
            Some("Snoozed until 3:15 PM".into())
        );
    }

    #[test]
    fn detail_actions_hide_when_resolved() {
        let resolved = detail_actions(TASK_DUE, Some("vault_note"), true);
        assert!(!resolved.show_complete);
        assert!(!resolved.show_snooze);
        assert!(resolved.show_obsidian);
    }

    #[test]
    fn why_now_text_uses_late_wording() {
        assert!(why_now_text(true, false, "2:00 PM").starts_with("Missed"));
        assert!(why_now_text(false, false, "2:00 PM").starts_with("Due at"));
    }
}
