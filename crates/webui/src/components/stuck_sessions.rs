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

/// Whether a cancel response counts as accepted. 202 Accepted is the documented success path;
/// some stacks return plain 200, so any 2xx counts.
fn cancel_accepted(status: u16) -> bool {
    status == 202 || (200..300).contains(&status)
}

async fn cancel_goal(api_base: String, id: String) -> Result<(), String> {
    let url = format!("{api_base}/api/goals/{id}/cancel");
    let client = reqwest::Client::new();
    let resp = client
        .post(&url)
        .send()
        .await
        .map_err(|e| format!("cancel request failed: {e}"))?;
    if cancel_accepted(resp.status().as_u16()) {
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

#[cfg(test)]
mod tests {
    use super::{age_label, cancel_accepted, short_id};

    /// A missing timestamp reads as "age unknown" — never an empty label or a crash.
    #[test]
    fn missing_timestamp_is_age_unknown() {
        assert_eq!(age_label(None), "age unknown");
    }

    /// An unparseable timestamp is shown raw: hiding it would claim the row is newer than it is,
    /// and inventing a number would claim something false.
    #[test]
    fn unparseable_timestamp_is_shown_raw() {
        assert_eq!(age_label(Some("not-a-date")), "not-a-date");
    }

    /// Each band has its own unit: seconds under a minute, minutes under an hour, hours under a
    /// day, days after that. The unit is the point of the label.
    #[test]
    fn age_bands_choose_the_unit() {
        // 45 seconds ago → "45s"
        let raw = (chrono::Utc::now() - chrono::Duration::seconds(45)).to_rfc3339();
        assert_eq!(age_label(Some(&raw)), "45s");

        // 5 minutes ago → "5m"
        let raw = (chrono::Utc::now() - chrono::Duration::minutes(5)).to_rfc3339();
        assert_eq!(age_label(Some(&raw)), "5m");

        // 3 hours ago → "3h"
        let raw = (chrono::Utc::now() - chrono::Duration::hours(3)).to_rfc3339();
        assert_eq!(age_label(Some(&raw)), "3h");

        // 2 days ago → "2d"
        let raw = (chrono::Utc::now() - chrono::Duration::days(2)).to_rfc3339();
        assert_eq!(age_label(Some(&raw)), "2d");
    }

    /// A timestamp from the future (clock skew) must not produce a negative age label.
    #[test]
    fn future_timestamp_clamps_to_zero() {
        let raw = (chrono::Utc::now() + chrono::Duration::minutes(1)).to_rfc3339();
        assert_eq!(age_label(Some(&raw)), "0s");
    }

    /// The band boundaries are exact: 60s is a minute, 3600s an hour, 86400s a day — the `<`
    /// comparisons must be strict or the unit drifts by exactly one.
    #[test]
    fn band_boundaries_fall_up() {
        let at = |secs: i64| (chrono::Utc::now() - chrono::Duration::seconds(secs)).to_rfc3339();
        assert_eq!(age_label(Some(&at(60))), "1m");
        assert_eq!(age_label(Some(&at(3600))), "1h");
        assert_eq!(age_label(Some(&at(86_400))), "1d");
    }

    /// `cancel_accepted` treats 202 and any other 2xx as success; everything else is a refusal
    /// worth showing.
    #[test]
    fn cancel_accepts_2xx_including_202() {
        for ok in [200, 202, 204] {
            assert!(cancel_accepted(ok), "{ok} should cancel cleanly");
        }
        for no in [300, 400, 500] {
            assert!(
                !cancel_accepted(no),
                "{no} should surface as a cancel failure"
            );
        }
    }

    /// Short ids pass through whole; long ones lose only the tail.
    #[test]
    fn short_id_truncates_to_twelve_chars() {
        assert_eq!(short_id("abc"), "abc");
        assert_eq!(short_id("0123456789ab"), "0123456789ab");
        assert_eq!(short_id("0123456789abcdef"), "0123456789ab");
    }

    /// Multi-byte ids must be cut on char boundaries — the regression the char-based version
    /// exists for. The first 12 *chars* survive even when that ends mid-codepoint in byte terms.
    #[test]
    fn short_id_is_char_boundary_safe() {
        let id = "中中中中中中中中中中中中";
        let short = short_id(id);
        assert_eq!(short.chars().count(), 12);
        assert!(id.starts_with(&short));
    }
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
