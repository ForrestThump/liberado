//! Automations view — cron jobs. Currently a calm empty state; will list scheduled jobs from
//! the daemon once the endpoints exist.

use dioxus::prelude::*;

#[component]
pub fn Automations(api_base: String) -> Element {
    rsx! {
        div {
            class: "grok-page automations-page",
            div {
                class: "grok-page-empty",
                div {
                    class: "grok-page-eyebrow",
                    "Automations"
                }
                h1 { class: "grok-page-title", "Schedules and automations" }
                p {
                    class: "grok-page-subtitle",
                    "Cron jobs, recurring prompts, and any other timed work the agent can run for you. Nothing scheduled right now."
                }
                div {
                    class: "grok-page-meta",
                    "API: {api_base}"
                }
            }
        }
    }
}
