use dioxus::prelude::*;

use chat_client_contract::{ReactionEvent, ReactionOutcome};

async fn fetch_reactions(api_base: String) -> Result<Vec<ReactionEvent>, String> {
    let url = format!("{api_base}/api/reactions?limit=20");
    let resp = reqwest::get(&url).await.map_err(|e| format!("{e}"))?;
    let events: Vec<ReactionEvent> = resp.json().await.map_err(|e| format!("{e}"))?;
    Ok(events)
}

/// The CSS class for a reaction's outcome — how the row's badge is tinted.
fn outcome_class(outcome: &ReactionOutcome) -> &'static str {
    match outcome {
        ReactionOutcome::Acted => "reaction-outcome acted",
        ReactionOutcome::Decided => "reaction-outcome decided",
        ReactionOutcome::Observed => "reaction-outcome observed",
        // A dispatch is the strongest form of acting — the agent already sent work out.
        ReactionOutcome::Dispatched { .. } => "reaction-outcome acted",
    }
}

/// The human text for a reaction's outcome.
fn outcome_label(outcome: &ReactionOutcome) -> String {
    match outcome {
        ReactionOutcome::Acted => "acted".to_string(),
        ReactionOutcome::Decided => "decided".to_string(),
        ReactionOutcome::Observed => "observed".to_string(),
        ReactionOutcome::Dispatched { session_id } => {
            format!("session {}", short_session_label(session_id))
        }
    }
}

/// The first 8 characters of a session id, on char boundaries. Session ids are ULIDs (ASCII)
/// today, but the wire type is a plain string — byte-slicing an id that is not ASCII would panic
/// the whole panel.
fn short_session_label(id: &str) -> String {
    id.chars().take(8).collect()
}

/// The row's clock, or the raw timestamp when it cannot be parsed — the event is still shown,
/// just without a formatted time.
fn reaction_time_label(iso: &str) -> String {
    chrono::DateTime::parse_from_rfc3339(iso)
        .map(|dt| dt.format("%H:%M:%S").to_string())
        .unwrap_or_else(|_| iso.to_string())
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
    let outcome_cls = outcome_class(&event.outcome);
    let outcome_label = outcome_label(&event.outcome);
    let path_str = event.path.clone().unwrap_or_else(|| "-".to_string());
    let time_str = reaction_time_label(&event.timestamp);

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

#[cfg(test)]
mod tests {
    use super::{outcome_class, outcome_label, reaction_time_label, short_session_label};
    use chat_client_contract::ReactionOutcome;

    /// Every outcome gets a distinct readable label, and a dispatched reaction names the session
    /// it was sent from.
    #[test]
    fn each_outcome_has_a_label() {
        assert_eq!(outcome_label(&ReactionOutcome::Acted), "acted");
        assert_eq!(outcome_label(&ReactionOutcome::Decided), "decided");
        assert_eq!(outcome_label(&ReactionOutcome::Observed), "observed");
        assert_eq!(
            outcome_label(&ReactionOutcome::Dispatched {
                session_id: "01HZABC".into()
            }),
            "session 01HZABC"
        );
    }

    /// A dispatch reads as acted — the strongest badge — not as its own shade that means nothing.
    #[test]
    fn dispatch_badges_as_acted() {
        assert_eq!(
            outcome_class(&ReactionOutcome::Dispatched {
                session_id: "x".into()
            }),
            "reaction-outcome acted"
        );
        assert_eq!(
            outcome_class(&ReactionOutcome::Acted),
            "reaction-outcome acted"
        );
        assert_eq!(
            outcome_class(&ReactionOutcome::Observed),
            "reaction-outcome observed"
        );
    }

    /// The session id on the badge is truncated to 8 characters; multi-byte ids must survive the
    /// cut rather than panicking the row.
    #[test]
    fn session_label_truncates_on_char_boundaries() {
        assert_eq!(short_session_label("0123456789ab"), "01234567");
        let wide = "中中中中中中中中中"; // 9 chars, truncated to 8
        let label = short_session_label(wide);
        assert_eq!(label.chars().count(), 8);
        assert!(wide.starts_with(&label));
    }

    /// A parseable timestamp becomes a clock; anything else is shown raw so the event is never
    /// left without a time column.
    #[test]
    fn time_label_formats_or_passes_through() {
        assert_eq!(reaction_time_label("2026-01-15T10:30:45Z"), "10:30:45");
        assert_eq!(reaction_time_label("not-a-date"), "not-a-date");
    }
}
