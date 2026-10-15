//! Project and plugin rows in the sidebar.
//!
//! Split from `sidebar.rs` so that file stays inside the module-health boundaries.

use dioxus::prelude::*;

use super::Route;
use super::rail::{PluginGlyph, ProjectGlyph};

/// Projects are placeholders for the prototype — three demo entries with the colour the user
/// saw in the original screenshot. The header `+` opens a modal in the parent; rows here are
/// status rows, not links.
const PROJECT_ENTRIES: &[(&str, &str)] = &[
    ("Life System", "var(--dot-cyan)"),
    ("Self Help", "var(--dot-green)"),
    ("Homelab fix", "var(--dot-orange)"),
];

#[component]
pub(super) fn ProjectList(route: Signal<Route>) -> Element {
    rsx! {
        ul {
            class: "grok-list",
            for entry in PROJECT_ENTRIES.iter() {
                ProjectRow {
                    label: entry.0,
                    color: entry.1,
                }
            }
        }
    }
}

#[component]
fn ProjectRow(label: &'static str, color: &'static str) -> Element {
    rsx! {
        li {
            class: "grok-row grok-row-static",
            div {
                class: "grok-row-btn grok-row-static-btn",
                span {
                    class: "grok-row-dot",
                    style: "background: {color};",
                    "aria-hidden": "true"
                }
                ProjectGlyph {}
                span { class: "grok-row-title", "{label}" }
            }
        }
    }
}

#[component]
pub(super) fn PluginList(on_open: EventHandler<()>) -> Element {
    rsx! {
        ul {
            class: "grok-list",
            li {
                class: "grok-row",
                button {
                    class: "grok-row-btn",
                    r#type: "button",
                    onclick: move |_| on_open.call(()),
                    PluginGlyph {}
                    span { class: "grok-row-title", "Manage plugins" }
                }
            }
        }
    }
}
