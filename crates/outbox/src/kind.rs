//! Notification kind strings stored on outbox rows and push payloads.

pub const REMINDER: &str = "reminder";
pub const TASK_DUE: &str = "task_due";
pub const EVENT_START: &str = "event_start";
pub const TASK_LIST: &str = "task_list";
pub const EVENT_LIST: &str = "event_list";
pub const HABIT: &str = "habit";
pub const JOB_ERROR: &str = "job_error";
pub const TEST: &str = "test";
pub const DIGEST: &str = "digest";

/// Missing or empty DB values read as [`REMINDER`].
pub fn parse_stored(text: Option<&str>) -> &str {
    match text.filter(|value| !value.trim().is_empty()) {
        Some(value) => value,
        None => REMINDER,
    }
}
