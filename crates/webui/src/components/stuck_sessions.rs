//! Stuck / parked goal sessions panel (F4).
//!
//! Lists `parked` goal sessions from `GET /api/goals` with age from durable `created_at`, and a
//! cancel control that hits `POST /api/goals/{id}/cancel` — the same path the hub now uses to
//! finish parked store records that have no live cancel token.

use dioxus::prelude::*;

use chrono::{DateTime, Utc};
use serde::Deserialize;

/// Minimal wire shape of a goal session row (fields the panel needs).
#[derive(Debug, Clone, Deserialize, PartialEq)]
struct GoalSessionRow {
    id: String,
    status: String,
    #[serde(default)]
    created_at: Option<String>,
    #[serde(default)]
    awaiting_input: bool,
    #[serde(default)]
    goal: Option<GoalSpecLite>,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
struct GoalSpecLite {
    #[serde(default)]
    description: String,
    #[serde(default)]
    domain: String,
}

async fn fetch_goals(api_base: String) -> Result<Vec<GoalSessionRow>, String> {
    let url = format!("{api_base}/api/goals");
    let resp = reqwest::get(&url)
        .await
        .map_err(|e| format!("Failed to list goals: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("list goals HTTP {}", resp.status()));
    }
    resp.json()
        .await
        .map_err(|e| format!("Bad goals response: {e}"))
}

async fn cancel_goal(api_base: String, id: String) -> Result<(), String> {
    let url = format!("{api_base}/api/goals/{id}/cancel");
    let client = reqwest::Client::new();
    let resp = client
        .post(&url)
        .send()
        .await
        .map_err(|e| format!("cancel request failed: {e}"))?;
    // 202 Accepted is the success path; some stacks may return 200.
    if resp.status().as_u16() == 202 || resp.status().is_success() {
        Ok(())
    } else {
        let body = resp.text().await.unwrap_or_default();
        Err(if body.is_empty() {
            "cancel failed".into()
        } else {
            body
        })
    }
}

fn age_label(created_at: Option<&str>) -> String {
    let Some(raw) = created_at else {
        return "age unknown".into();
    };
    let Ok(dt) = DateTime::parse_from_rfc3339(raw) else {
        return raw.to_string();
    };
    let created: DateTime<Utc> = dt.with_timezone(&Utc);
    let secs = (Utc::now() - created).num_seconds().max(0);
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else if secs < 86400 {
        format!("{}h", secs / 3600)
    } else {
        format!("{}d", secs / 86400)
    }
}

/// Truncate on char boundaries: ids are ULIDs today, but `GoalSpec.id` is caller-supplied, and
/// byte-slicing a multi-byte id would panic the whole panel rather than shorten one label.
fn short_id(id: &str) -> String {
    id.chars().take(12).collect()
}

#[component]
pub fn StuckSessionsPanel(api_base: String) -> Element {
    let refresh = use_signal(|| 0u32);
    let cancel_error = use_signal(|| None::<String>);
    let cancelling = use_signal(|| None::<String>);

    let goals = use_resource({
        let base = api_base.clone();
        move || {
            let _ = *refresh.read();
            let base = base.clone();
            async move { fetch_goals(base).await }
        }
    });

    rsx! {
        div {
            class: "card stuck-sessions-panel",
            div {
                class: "card-header",
                h2 { "Stuck sessions" }
            }
            div {
                class: "card-body",
                p {
                    class: "stuck-sessions-hint",
                    "Parked goal sessions (waiting on a human, or left behind after a restart). "
                    "Cancel clears the durable record so it no longer blocks capacity."
                }
                if let Some(err) = cancel_error.read().as_ref() {
                    p { class: "stuck-sessions-error", "{err}" }
                }
                match &*goals.read() {
                    Some(Ok(list)) => {
                        let parked: Vec<_> = list
                            .iter()
                            .filter(|r| r.status == "parked")
                            .cloned()
                            .collect();
                        if parked.is_empty() {
                            rsx! {
                                p { class: "empty-panel", "No parked sessions." }
                            }
                        } else {
                            rsx! {
                                div {
                                    class: "stuck-sessions-list",
                                    for row in parked {
                                        StuckSessionRow {
                                            api_base: api_base.clone(),
                                            row: row.clone(),
                                            cancelling: cancelling,
                                            cancel_error: cancel_error,
                                            refresh: refresh,
                                        }
                                    }
                                }
                            }
                        }
                    }
                    Some(Err(e)) => rsx! {
                        p { class: "empty-panel", "Error: {e}" }
                    },
                    None => rsx! {
                        p { class: "empty-panel", "Loading..." }
                    },
                }
            }
        }
    }
}

#[component]
fn StuckSessionRow(
    api_base: String,
    row: GoalSessionRow,
    mut cancelling: Signal<Option<String>>,
    mut cancel_error: Signal<Option<String>>,
    mut refresh: Signal<u32>,
) -> Element {
    let id = row.id.clone();
    let id_btn = id.clone();
    let age = age_label(row.created_at.as_deref());
    let desc = row
        .goal
        .as_ref()
        .map(|g| g.description.as_str())
        .unwrap_or("")
        .chars()
        .take(80)
        .collect::<String>();
    let domain = row
        .goal
        .as_ref()
        .map(|g| {
            if g.domain.is_empty() {
                "—".to_string()
            } else {
                g.domain.clone()
            }
        })
        .unwrap_or_else(|| "—".into());
    let awaiting = if row.awaiting_input {
        "awaiting input"
    } else {
        "parked"
    };
    let is_cancelling = cancelling.read().as_deref() == Some(id.as_str());
    let short = short_id(&id);

    rsx! {
        div {
            class: "stuck-session-row",
            div {
                class: "stuck-session-meta",
                span { class: "stuck-session-id", title: "{id}", "{short}" }
                span { class: "stuck-session-age", "age {age}" }
                span { class: "stuck-session-status", "{awaiting}" }
                span { class: "stuck-session-domain", "{domain}" }
            }
            p { class: "stuck-session-desc", "{desc}" }
            button {
                class: "stuck-session-cancel",
                disabled: is_cancelling,
                onclick: move |_| {
                    let base = api_base.clone();
                    let sid = id_btn.clone();
                    cancelling.set(Some(sid.clone()));
                    cancel_error.set(None);
                    spawn(async move {
                        match cancel_goal(base, sid.clone()).await {
                            Ok(()) => {
                                cancelling.set(None);
                                refresh.set(refresh() + 1);
                            }
                            Err(e) => {
                                cancelling.set(None);
                                cancel_error.set(Some(e));
                            }
                        }
                    });
                },
                if is_cancelling { "Cancelling…" } else { "Cancel" }
            }
        }
    }
}
