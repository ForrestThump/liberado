//! Placeholder for the Imagine route — image generation isn't shipped yet. The shell mirrors the
//! Chat empty state so a user who navigates here from the sidebar sees something intentional,
//! not a half-empty page.

use dioxus::prelude::*;

#[component]
pub fn Imagine(api_base: String) -> Element {
    rsx! {
        div {
            class: "grok-page imagine-page",
            div {
                class: "grok-page-empty",
                div {
                    class: "grok-page-eyebrow",
                    "Imagine"
                }
                h1 { class: "grok-page-title", "Picture what you want" }
                p {
                    class: "grok-page-subtitle",
                    "Generate images from a prompt. The surface is wired but no model is bound yet — when one is, this page becomes the canvas."
                }
                div {
                    class: "grok-page-meta",
                    "API: {api_base}"
                }
            }
        }
    }
}
