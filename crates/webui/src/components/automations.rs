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
            match schedules.read().as_ref() {
                None => rsx! {
                    div {
                        class: "grok-page-header",
                        div { class: "grok-page-eyebrow", "Automations" }
                        h1 { class: "grok-page-title", "Schedules and automations" }
                        p { class: "project-status", "Loading..." }
                    }
                },
                Some(Err(error)) => rsx! {
                    div {
                        class: "grok-page-header",
                        div { class: "grok-page-eyebrow", "Automations" }
                        h1 { class: "grok-page-title", "Schedules and automations" }
                        p { class: "project-status", "{error}" }
                    }
                },
                Some(Ok(rows)) if rows.is_empty() => rsx! {
                    div {
                        class: "grok-page-empty",
                        div { class: "grok-page-eyebrow", "Automations" }
                        h1 { class: "grok-page-title", "Schedules and automations" }
                        p {
                            class: "grok-page-subtitle",
                            "Cron jobs, recurring prompts, and other timed work from topology.toml. Nothing is configured right now."
                        }
                    }
                },
                Some(Ok(rows)) => rsx! {
                    div {
                        class: "grok-page-header",
                        div { class: "grok-page-eyebrow", "Automations" }
                        h1 { class: "grok-page-title", "Schedules and automations" }
                        p {
                            class: "grok-page-subtitle",
                            "Read-only view of [[schedules]] in topology.toml. Enable, disable, and run-now are not available here yet."
                        }
                    }
                    div {
                        class: "grok-page-body",
                        ul {
                            class: "automation-list",
                            for row in rows.iter() {
                                ScheduleRowView { row: row.clone() }
                            }
                        }
                    }
                },
            }
        }
    }
}

#[component]
fn ScheduleRowView(row: ScheduleRow) -> Element {
    let (badge_class, badge_label) = enabled_badge(row.enabled);
    let snippet = schedule_snippet(&row);
    let profile = row
        .profile
        .as_deref()
        .filter(|p| !p.is_empty())
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
            div {
                class: "automation-expr",
                code { "{row.cron_expr}" }
            }
            p {
                class: "automation-snippet",
                "{snippet}"
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
        };
        assert_eq!(schedule_snippet(&row), "habit-ping");
    }

    #[test]
    fn enabled_badge_labels_match_state() {
        assert_eq!(enabled_badge(true).1, "Enabled");
        assert_eq!(enabled_badge(false).1, "Disabled");
    }
}
