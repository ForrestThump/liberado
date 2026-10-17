//! Automations view — cron schedules from `GET /api/schedules`.
//!
//! The list is read-only. Edit and New each open a fresh chat.

use dioxus::prelude::*;
use serde::Deserialize;

use chat_client_contract::SurfaceMode;

use super::sidebar::Route;

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
    #[serde(default)]
    next_fire: Option<String>,
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

/// Body for a new automation chat. No target seed.
pub(crate) fn new_automation_body() -> serde_json::Value {
    serde_json::json!({ "automation_editor": true })
}

/// Body for an Edit click. The target is hidden prompt context.
pub(crate) fn edit_automation_body(row: &ScheduleRow) -> serde_json::Value {
    serde_json::json!({
        "automation_editor": true,
        "target": {
            "name": row.name,
            "enabled": row.enabled,
            "schedule_text": row.schedule_text,
            "cron_expr": row.cron_expr,
            "timezone": row.timezone,
            "description": row.description,
        }
    })
}

pub(crate) fn created_conversation_id(body: &str) -> Result<String, String> {
    let value: serde_json::Value =
        serde_json::from_str(body).map_err(|e| format!("Bad response: {e}"))?;
    value
        .get("id")
        .and_then(|id| id.as_str())
        .filter(|id| !id.is_empty())
        .map(str::to_string)
        .ok_or_else(|| "The new chat did not return an id.".to_string())
}

fn chat_error(status: u16, body: &str) -> String {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|value| {
            value
                .get("error")
                .and_then(|error| error.as_str())
                .map(str::to_string)
        })
        .unwrap_or_else(|| format!("Could not open the chat (HTTP {status})"))
}

async fn post_automation_chat(api_base: &str, body: &serde_json::Value) -> Result<String, String> {
    let url = format!("{api_base}/api/conversations");
    let resp = reqwest::Client::new()
        .post(&url)
        .json(body)
        .send()
        .await
        .map_err(|e| format!("Failed to reach daemon: {e}"))?;
    let status = resp.status().as_u16();
    let text = resp
        .text()
        .await
        .map_err(|e| format!("Bad response: {e}"))?;
    if !(200..300).contains(&status) {
        return Err(chat_error(status, &text));
    }
    created_conversation_id(&text)
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

/// `Next: Mon Oct 19, 8:00 PM CDT` when the API sent a fire time.
pub(crate) fn next_fire_line(row: &ScheduleRow) -> Option<String> {
    let text = row.next_fire.as_deref().unwrap_or("").trim();
    if text.is_empty() {
        None
    } else {
        Some(format!("Next: {text}"))
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

#[derive(Clone, PartialEq)]
struct OpenChat {
    api_base: String,
    active_conv_id: Signal<Option<String>>,
    active_surface: Signal<Option<SurfaceMode>>,
    list_epoch: Signal<u64>,
    route: Signal<Route>,
    collapsed: Signal<bool>,
    error: Signal<Option<String>>,
    opening: Signal<bool>,
}

fn start_automation_chat(mut open: OpenChat, body: serde_json::Value) {
    if (open.opening)() {
        return;
    }
    open.opening.set(true);
    #[cfg(target_arch = "wasm32")]
    {
        wasm_bindgen_futures::spawn_local(async move {
            match post_automation_chat(&open.api_base, &body).await {
                Ok(id) => land_in_chat(open, id),
                Err(message) => {
                    open.error.set(Some(message));
                    open.opening.set(false);
                }
            }
        });
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = body;
        open.opening.set(false);
    }
}

#[cfg(target_arch = "wasm32")]
fn land_in_chat(mut open: OpenChat, id: String) {
    open.active_conv_id.set(Some(id));
    open.active_surface.set(Some(SurfaceMode::Chat));
    open.list_epoch.set((open.list_epoch)() + 1);
    open.route.set(Route::ChatSession);
    if crate::is_narrow_viewport() {
        open.collapsed.set(true);
    }
    open.error.set(None);
    open.opening.set(false);
}

#[component]
pub fn Automations(
    api_base: String,
    active_conv_id: Signal<Option<String>>,
    active_surface: Signal<Option<SurfaceMode>>,
    list_epoch: Signal<u64>,
    route: Signal<Route>,
    collapsed: Signal<bool>,
) -> Element {
    let error = use_signal(|| None::<String>);
    let opening = use_signal(|| false);
    let schedules = use_resource({
        let base = api_base.clone();
        move || {
            let base = base.clone();
            async move { fetch_schedules(&base).await }
        }
    });
    let open = OpenChat {
        api_base: api_base.clone(),
        active_conv_id,
        active_surface,
        list_epoch,
        route,
        collapsed,
        error,
        opening,
    };

    rsx! {
        div {
            class: "grok-page automations-page",
            div {
                class: "grok-page-body",
                div {
                    class: "automation-toolbar",
                    button {
                        class: "approval-option",
                        r#type: "button",
                        disabled: opening(),
                        onclick: {
                            let open = open.clone();
                            move |_| start_automation_chat(open.clone(), new_automation_body())
                        },
                        "New automation"
                    }
                }
                if let Some(message) = error() {
                    p { class: "project-status", "{message}" }
                }
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
                                ScheduleRowView { row: row.clone(), open: open.clone() }
                            }
                        }
                    },
                }
            }
        }
    }
}

#[component]
fn ScheduleRowView(row: ScheduleRow, open: OpenChat) -> Element {
    let (badge_class, badge_label) = enabled_badge(row.enabled);
    let primary = schedule_primary(&row);
    let secondary = schedule_secondary(&row);
    let body = schedule_body(&row);
    let next_line = next_fire_line(&row);
    let show_profile = row.description.trim().is_empty();
    let profile = row
        .profile
        .as_deref()
        .filter(|p| show_profile && !p.is_empty())
        .map(|p| format!("profile: {p}"))
        .unwrap_or_default();
    let edit_body = edit_automation_body(&row);

    rsx! {
        li {
            class: "automation-row",
            div {
                class: "automation-row-main",
                span { class: "automation-name", "{row.name}" }
                span { class: "{badge_class}", "{badge_label}" }
                div {
                    class: "automation-row-actions",
                    button {
                        class: "approval-option",
                        r#type: "button",
                        disabled: (open.opening)(),
                        onclick: {
                            let open = open.clone();
                            let edit_body = edit_body.clone();
                            move |_| start_automation_chat(open.clone(), edit_body.clone())
                        },
                        "Edit"
                    }
                }
            }
            p { class: "automation-when", "{primary}" }
            if let Some(next_line) = next_line {
                p { class: "automation-next", "{next_line}" }
            }
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

    fn row() -> ScheduleRow {
        ScheduleRow {
            name: "morning".into(),
            enabled: true,
            cron_expr: "0 30 12 * * * *".into(),
            goal: String::new(),
            job: Some("event-ping".into()),
            profile: None,
            description: "Sends a reminder listing calendar events.".into(),
            schedule_text: "Every day at 6:30 AM".into(),
            timezone: "America/Chicago".into(),
            next_fire: Some("Mon Oct 19, 8:00 PM CDT".into()),
        }
    }

    #[test]
    fn schedules_json_maps_goal_job_and_enabled() {
        let body = r#"{"schedules":[{"name":"tick","enabled":false,"cron_expr":"0 * * * * * *","goal":"ping","job":null,"profile":"ops","next_fire":null}]}"#;
        let rows = parse_schedules(body).unwrap();
        assert_eq!(rows.len(), 1);
        assert!(!rows[0].enabled);
        assert_eq!(rows[0].cron_expr, "0 * * * * * *");
        assert_eq!(schedule_snippet(&rows[0]), "ping");
        assert!(next_fire_line(&rows[0]).is_none());
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
            next_fire: None,
        };
        assert_eq!(schedule_snippet(&row), "habit-ping");
        assert_eq!(schedule_body(&row), "habit-ping");
    }

    #[test]
    fn readable_schedule_text_is_primary_and_cron_plus_zone_is_secondary() {
        let row = row();
        assert_eq!(schedule_primary(&row), "Every day at 6:30 AM");
        assert_eq!(
            schedule_secondary(&row),
            "0 30 12 * * * * · America/Chicago"
        );
        assert_eq!(
            schedule_body(&row),
            "Sends a reminder listing calendar events."
        );
        assert_eq!(
            next_fire_line(&row).as_deref(),
            Some("Next: Mon Oct 19, 8:00 PM CDT")
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
            next_fire: None,
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

    #[test]
    fn new_and_edit_bodies_open_a_fresh_editor_chat() {
        assert_eq!(
            new_automation_body(),
            serde_json::json!({ "automation_editor": true })
        );
        let body = edit_automation_body(&row());
        assert_eq!(body["automation_editor"], true);
        assert_eq!(body["target"]["name"], "morning");
        assert_eq!(body["target"]["schedule_text"], "Every day at 6:30 AM");
        assert_eq!(body["target"]["cron_expr"], "0 30 12 * * * *");
        assert_eq!(body["target"]["timezone"], "America/Chicago");
        assert!(body.get("profile").is_none());
    }

    #[test]
    fn the_created_chat_id_comes_from_the_header() {
        assert_eq!(
            created_conversation_id(r#"{"id":"01CHAT"}"#).unwrap(),
            "01CHAT"
        );
        assert!(created_conversation_id("{}").is_err());
        assert!(created_conversation_id("not json").is_err());
    }

    #[test]
    fn the_page_is_a_list_with_edit_and_new_and_no_dock() {
        let page = include_str!("automations.rs");
        let page = page.split("#[cfg(test)]").next().unwrap();
        let css = include_str!("../styles/main.css");
        assert!(page.contains("New automation"));
        assert!(page.contains("\"Edit\""));
        assert!(page.contains("automation-list"));
        assert!(!page.contains("automation-editor"), "{page}");
        assert!(!page.contains("topology.toml"));
        assert!(!page.contains("restart"));
        assert!(!css.contains("automation-editor-dock"));
        assert!(!css.contains("automation-editor-split"));
        assert!(!css.contains("[automation-scroll]"));
    }
}
