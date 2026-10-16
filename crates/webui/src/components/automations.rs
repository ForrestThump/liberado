//! Automations view — cron schedules from `GET /api/schedules`.

use dioxus::prelude::*;
use serde::Deserialize;

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub(crate) struct ScheduleRow {
    name: String,
    enabled: bool,
    cron_expr: String,
    #[serde(default)]
    goal: String,
    job: Option<String>,
    profile: Option<String>,
    #[serde(default)]
    description: String,
    #[serde(default)]
    schedule_text: String,
    #[serde(default)]
    timezone: String,
}

#[derive(Debug, Deserialize)]
struct SchedulesResponse {
    #[serde(default)]
    schedules: Vec<ScheduleRow>,
}

fn parse_schedules(body: &str) -> Result<Vec<ScheduleRow>, String> {
    let parsed: SchedulesResponse =
        serde_json::from_str(body).map_err(|e| format!("Bad response: {e}"))?;
    Ok(parsed.schedules)
}

async fn fetch_schedules(api_base: &str) -> Result<Vec<ScheduleRow>, String> {
    let url = format!("{api_base}/api/schedules");
    let resp = reqwest::get(&url)
        .await
        .map_err(|e| format!("Failed to reach daemon: {e}"))?;
    if !resp.status().is_success() {
        let status = resp.status();
        return Err(format!("Schedules unavailable ({status})"));
    }
    let text = resp
        .text()
        .await
        .map_err(|e| format!("Bad response: {e}"))?;
    parse_schedules(&text)
}

pub(crate) fn schedule_snippet(row: &ScheduleRow) -> String {
    let goal = row.goal.trim();
    if !goal.is_empty() {
        return goal.to_string();
    }
    row.job
        .as_deref()
        .filter(|j| !j.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| "—".to_string())
}

pub(crate) fn schedule_primary(row: &ScheduleRow) -> String {
    let text = row.schedule_text.trim();
    if text.is_empty() {
        row.cron_expr.clone()
    } else {
        text.to_string()
    }
}

pub(crate) fn schedule_secondary(row: &ScheduleRow) -> String {
    let timezone = row.timezone.trim();
    if timezone.is_empty() {
        row.cron_expr.clone()
    } else {
        format!("{} · {}", row.cron_expr, timezone)
    }
}

pub(crate) fn schedule_body(row: &ScheduleRow) -> String {
    let description = row.description.trim();
    if description.is_empty() {
        schedule_snippet(row)
    } else {
        description.to_string()
    }
}

fn enabled_badge(enabled: bool) -> (&'static str, &'static str) {
    if enabled {
        ("automation-badge on", "Enabled")
    } else {
        ("automation-badge off", "Disabled")
    }
}

#[component]
pub fn Automations(api_base: String) -> Element {
    let schedules = use_resource({
        let base = api_base.clone();
        move || {
            let base = base.clone();
            async move { fetch_schedules(&base).await }
        }
    });

    rsx! {
        div {
            class: "grok-page automations-page",
            div {
                class: "grok-page-body",
                match schedules.read().as_ref() {
                    None => rsx! { p { class: "project-status", "Loading..." } },
                    Some(Err(error)) => rsx! { p { class: "project-status", "{error}" } },
                    Some(Ok(rows)) if rows.is_empty() => rsx! {
                        p { class: "project-status", "No automations are configured." }
                    },
                    Some(Ok(rows)) => rsx! {
                        ul {
                            class: "automation-list",
                            for row in rows.iter() {
                                ScheduleRowView { row: row.clone() }
                            }
                        }
                    },
                }
            }
        }
    }
}

#[component]
fn ScheduleRowView(row: ScheduleRow) -> Element {
    let (badge_class, badge_label) = enabled_badge(row.enabled);
    let primary = schedule_primary(&row);
    let secondary = schedule_secondary(&row);
    let body = schedule_body(&row);
    let show_profile = row.description.trim().is_empty();
    let profile = row
        .profile
        .as_deref()
        .filter(|p| show_profile && !p.is_empty())
        .map(|p| format!("profile: {p}"))
        .unwrap_or_default();

    rsx! {
        li {
            class: "automation-row",
            div {
                class: "automation-row-main",
                span { class: "automation-name", "{row.name}" }
                span { class: "{badge_class}", "{badge_label}" }
            }
            p { class: "automation-when", "{primary}" }
            if secondary != primary {
                p { class: "automation-when-raw", "{secondary}" }
            }
            p {
                class: "automation-description",
                "{body}"
            }
            if !profile.is_empty() {
                p { class: "automation-meta", "{profile}" }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schedules_json_maps_goal_job_and_enabled() {
        let body = r#"{"schedules":[{"name":"tick","enabled":false,"cron_expr":"0 * * * * * *","goal":"ping","job":null,"profile":"ops","next_fire":null}]}"#;
        let rows = parse_schedules(body).unwrap();
        assert_eq!(rows.len(), 1);
        assert!(!rows[0].enabled);
        assert_eq!(rows[0].cron_expr, "0 * * * * * *");
        assert_eq!(schedule_snippet(&rows[0]), "ping");
    }

    #[test]
    fn snippet_falls_back_to_job_when_goal_is_empty() {
        let row = ScheduleRow {
            name: "habit".into(),
            enabled: true,
            cron_expr: "0 0 8 * * * *".into(),
            goal: String::new(),
            job: Some("habit-ping".into()),
            profile: None,
            description: String::new(),
            schedule_text: String::new(),
            timezone: String::new(),
        };
        assert_eq!(schedule_snippet(&row), "habit-ping");
        assert_eq!(schedule_body(&row), "habit-ping");
    }

    #[test]
    fn readable_schedule_text_is_primary_and_cron_plus_zone_is_secondary() {
        let row = ScheduleRow {
            name: "morning".into(),
            enabled: true,
            cron_expr: "0 30 12 * * * *".into(),
            goal: String::new(),
            job: Some("event-ping".into()),
            profile: None,
            description: "Sends a reminder listing calendar events.".into(),
            schedule_text: "Every day at 6:30 AM".into(),
            timezone: "America/Chicago".into(),
        };
        assert_eq!(schedule_primary(&row), "Every day at 6:30 AM");
        assert_eq!(
            schedule_secondary(&row),
            "0 30 12 * * * * · America/Chicago"
        );
        assert_eq!(
            schedule_body(&row),
            "Sends a reminder listing calendar events."
        );
    }

    #[test]
    fn a_missing_reading_falls_back_to_the_raw_cron() {
        let row = ScheduleRow {
            name: "tick".into(),
            enabled: true,
            cron_expr: "0 * * * * *".into(),
            goal: "ping".into(),
            job: None,
            profile: Some("ops".into()),
            description: String::new(),
            schedule_text: String::new(),
            timezone: String::new(),
        };
        assert_eq!(schedule_primary(&row), "0 * * * * *");
        assert_eq!(schedule_secondary(&row), "0 * * * * *");
        assert_eq!(schedule_body(&row), "ping");
    }

    #[test]
    fn enabled_badge_labels_match_state() {
        assert_eq!(enabled_badge(true).1, "Enabled");
        assert_eq!(enabled_badge(false).1, "Disabled");
    }
}
