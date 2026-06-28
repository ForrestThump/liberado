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
            class: "rounded-xl border border-gray-800 bg-gray-900/50",
            div {
                class: "px-5 py-4 border-b border-gray-800",
                h2 { class: "text-sm font-semibold uppercase tracking-wider text-gray-400",
                    "Recent Reactions"
                }
            }
            div {
                class: "divide-y divide-gray-800 max-h-96 overflow-y-auto",
                match &*reactions.read() {
                    Some(Ok(list)) => {
                        if list.is_empty() {
                            rsx! {
                                p { class: "text-gray-600 text-sm text-center py-8",
                                    "No reactions yet."
                                }
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
                        p { class: "text-red-500 text-sm p-4", "Error: {e}" }
                    },
                    None => rsx! {
                        p { class: "text-gray-600 text-sm text-center py-8", "Loading..." }
                    },
                }
            }
        }
    }
}

#[component]
fn ReactionRow(event: ReactionEvent) -> Element {
    let outcome_color = match event.outcome {
        ReactionOutcome::Acted => "text-emerald-400",
        ReactionOutcome::Decided => "text-amber-400",
        ReactionOutcome::Observed => "text-gray-500",
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
            class: "px-5 py-3 hover:bg-gray-800/50 transition-colors",
            div {
                class: "flex items-center justify-between",
                span { class: "text-sm font-medium text-gray-200",
                    "{event.event_type}"
                }
                span { class: "text-xs font-mono {outcome_color}", "{outcome_label}" }
            }
            div {
                class: "flex items-center justify-between mt-1",
                span { class: "text-xs text-gray-500 truncate max-w-[60%]",
                    "{path_str}"
                }
                span { class: "text-xs text-gray-600",
                    "{time_str}"
                }
            }
        }
    }
}
