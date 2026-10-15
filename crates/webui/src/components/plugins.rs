//! Plugins view — full-height lift of the McpPanel. Lives in its own route so the user can browse
//! MCP servers as a real, dismissible screen.

use dioxus::prelude::*;

#[component]
pub fn Plugins(api_base: String) -> Element {
    rsx! {
        div {
            class: "grok-page plugins-page",
            div {
                class: "grok-page-header",
                div {
                    class: "grok-page-eyebrow",
                    "Plugins"
                }
                h1 { class: "grok-page-title", "Connected MCP servers" }
                p {
                    class: "grok-page-subtitle",
                    "Servers the agent can call out to for tools. Toggle on what you want to expose, and the daemon will pick up the change."
                }
            }
            div {
                class: "grok-page-body",
                crate::components::mcp_panel::McpPanel { api_base: api_base.clone() }
            }
        }
    }
}
