use dioxus::prelude::*;

use chat_client_contract::{ReactionEvent, ReactionOutcome};

async fn fetch_reactions(api_base: String) -> Result<Vec<ReactionEvent>, String> {
    let url = format!("{api_base}/api/reactions?limit=20");
    let resp = reqwest::get(&url).await.map_err(|e| format!("{e}"))?;
    let events: Vec<ReactionEvent> = resp.json().await.map_err(|e| format!("{e}"))?;
    Ok(events)
}

#[component]
pub fn ReactionsPanel(api_base: String) -> Element {
    let reactions = use_resource(move || fetch_reactions(api_base.clone()));

    rsx! {
        div {
            class: "card",
            div {
                class: "card-header",
                h2 { "Recent Reactions" }
            }
            div {
                class: "reactions-list",
                match &*reactions.read() {
                    Some(Ok(list)) => {
                        if list.is_empty() {
                            rsx! {
                                p { class: "empty-panel", "No reactions yet." }
                            }
                        } else {
                            rsx! {
                                for ev in list {
                                    ReactionRow { event: ev.clone() }
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
fn ReactionRow(event: ReactionEvent) -> Element {
    let outcome_cls = match event.outcome {
        ReactionOutcome::Acted => "reaction-outcome acted",
        ReactionOutcome::Decided => "reaction-outcome decided",
        ReactionOutcome::Observed => "reaction-outcome observed",
    };
    let outcome_label = match event.outcome {
        ReactionOutcome::Acted => "acted",
        ReactionOutcome::Decided => "decided",
        ReactionOutcome::Observed => "observed",
    };
    let path_str = event.path.clone().unwrap_or_else(|| "-".to_string());
    // timestamp is now an ISO-8601 String (not DateTime<Utc>); parse for display.
    let time_str = chrono::DateTime::parse_from_rfc3339(&event.timestamp)
        .map(|dt| dt.format("%H:%M:%S").to_string())
        .unwrap_or_else(|_| event.timestamp.clone());

    rsx! {
        div {
            class: "reaction-row",
            div {
                class: "reaction-top",
                span { class: "reaction-event-type", "{event.event_type}" }
                span { class: "{outcome_cls}", "{outcome_label}" }
            }
            div {
                class: "reaction-bottom",
                span { class: "reaction-path", "{path_str}" }
                span { class: "reaction-time", "{time_str}" }
            }
        }
    }
}
