//! Library view — artifacts. Currently a calm empty state; will list any artifacts the daemon
//! exposes once an endpoint exists.

use dioxus::prelude::*;

#[component]
pub fn Library(api_base: String) -> Element {
    rsx! {
        div {
            class: "grok-page library-page",
            div {
                class: "grok-page-empty",
                div {
                    class: "grok-page-eyebrow",
                    "Library"
                }
                h1 { class: "grok-page-title", "Your artifacts" }
                p {
                    class: "grok-page-subtitle",
                    "Generated images, files the agent has saved, and any other artifacts produced across your conversations. Right now there are none."
                }
                div {
                    class: "grok-page-meta",
                    "API: {api_base}"
                }
            }
        }
    }
}
