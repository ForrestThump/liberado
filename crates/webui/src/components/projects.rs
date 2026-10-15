//! Projects view — project list. The sidebar already exposes three named projects. This view is
//! the project landing surface; selecting a project in the sidebar will route here.

use dioxus::prelude::*;

#[component]
pub fn Projects() -> Element {
    rsx! {
        div {
            class: "grok-page projects-page",
            div {
                class: "grok-page-empty",
                div {
                    class: "grok-page-eyebrow",
                    "Projects"
                }
                h1 { class: "grok-page-title", "Pick a project to start with" }
                p {
                    class: "grok-page-subtitle",
                    "Each project groups a chat, a vault mount, and a few seeded memories. The Life System, Self Help, and Homelab projects are visible in the sidebar — open one to switch context."
                }
            }
        }
    }
}
