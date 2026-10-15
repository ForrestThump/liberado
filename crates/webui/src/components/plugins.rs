//! Plugins view — MCP servers from `GET /api/catalog`.
//!
//! The catalog reports whether chat can call each server. It does not accept a change.
//! `POST /api/mcp/reload` re-reads topology from disk; it does not flip one server.
//! The switches are disabled and say so, including on a phone where a tooltip is easy to miss.

use dioxus::prelude::*;

use chat_client_contract::{CatalogResponse, McpInfo};

/// Visible without a hover. The switch tooltip repeats it.
pub(crate) const PLUGIN_TOGGLE_GAP: &str = "The daemon has no plugin toggle. This switch shows whether chat can call the server. \
     Change access in topology and policy, then reload MCP peers.";

/// There is no write API for one server, so the control cannot be turned on.
pub(crate) const fn plugin_switch_disabled() -> bool {
    true
}

pub(crate) fn plugin_meta(mcp: &McpInfo) -> String {
    let access = if mcp.visible_to_main_agent {
        "chat can call this server"
    } else {
        "chat cannot call this server"
    };
    if mcp.consequence.is_empty() {
        access.to_string()
    } else {
        format!("{access} · {}", mcp.consequence)
    }
}

pub(crate) fn tool_line(names: &[String]) -> String {
    names.join(", ")
}

async fn fetch_catalog(api_base: String) -> Result<CatalogResponse, String> {
    let url = format!("{api_base}/api/catalog");
    let resp = reqwest::get(&url)
        .await
        .map_err(|e| format!("Failed to reach daemon: {e}"))?;
    if !resp.status().is_success() {
        let status = resp.status();
        return Err(format!("Catalog unavailable ({status})"));
    }
    resp.json().await.map_err(|e| format!("Bad response: {e}"))
}

#[component]
pub fn Plugins(api_base: String) -> Element {
    let catalog = use_resource({
        let base = api_base.clone();
        move || fetch_catalog(base.clone())
    });

    rsx! {
        div {
            class: "grok-page plugins-page",
            div {
                class: "grok-page-header",
                div { class: "grok-page-eyebrow", "Plugins" }
                h1 { class: "grok-page-title", "Connected MCP servers" }
                p { class: "grok-page-subtitle", "{PLUGIN_TOGGLE_GAP}" }
            }
            div {
                class: "grok-page-body",
                match catalog.read().as_ref() {
                    None => rsx! { p { class: "plugin-status", "Loading..." } },
                    Some(Err(error)) => rsx! { p { class: "plugin-status", "{error}" } },
                    Some(Ok(body)) if body.mcps.is_empty() => rsx! {
                        p { class: "plugin-status", "No MCP servers registered." }
                    },
                    Some(Ok(body)) => rsx! {
                        ul {
                            class: "plugin-list",
                            for mcp in body.mcps.iter() {
                                PluginRow { mcp: mcp.clone() }
                            }
                        }
                    },
                }
            }
        }
    }
}

#[component]
fn PluginRow(mcp: McpInfo) -> Element {
    let on = mcp.visible_to_main_agent;
    let class = if on {
        "plugin-switch on"
    } else {
        "plugin-switch"
    };
    let checked = if on { "true" } else { "false" };
    let meta = plugin_meta(&mcp);
    let tools = tool_line(&mcp.tool_names);
    let label = format!("Chat access for {}", mcp.name);

    rsx! {
        li {
            class: "plugin-row",
            div {
                class: "plugin-row-main",
                span { class: "plugin-name", "{mcp.name}" }
                button {
                    class: "{class}",
                    r#type: "button",
                    role: "switch",
                    disabled: plugin_switch_disabled(),
                    title: PLUGIN_TOGGLE_GAP,
                    "aria-label": "{label}",
                    "aria-checked": "{checked}",
                }
            }
            p { class: "plugin-meta", "{meta}" }
            if !tools.is_empty() {
                p { class: "plugin-tools", "{tools}" }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(visible: bool) -> McpInfo {
        McpInfo {
            name: "tasks".into(),
            description: String::new(),
            consequence: "read_only".into(),
            tool_count: 1,
            tool_names: vec!["add".into()],
            provenance: None,
            visible_to_main_agent: visible,
            visible_to_dispatcher: false,
        }
    }

    #[test]
    fn the_switch_cannot_be_toggled_and_says_why() {
        assert!(plugin_switch_disabled());
        assert!(PLUGIN_TOGGLE_GAP.contains("no plugin toggle"));
        assert!(PLUGIN_TOGGLE_GAP.contains("topology"));
        let src = include_str!("plugins.rs");
        let page = src.split("mod tests").next().unwrap_or(src);
        assert!(
            page.contains("disabled: plugin_switch_disabled()"),
            "the control must use the disabled flag, not a click handler that pretends to toggle"
        );
        assert!(
            !page.contains("onclick"),
            "a click handler would look like a working toggle"
        );
    }

    #[test]
    fn meta_reports_chat_access_and_lists_tools() {
        assert_eq!(
            plugin_meta(&sample(true)),
            "chat can call this server · read_only"
        );
        let hidden = McpInfo {
            visible_to_main_agent: false,
            consequence: String::new(),
            ..sample(false)
        };
        assert_eq!(plugin_meta(&hidden), "chat cannot call this server");
        assert_eq!(tool_line(&["add".into(), "list".into()]), "add, list");
        assert_eq!(tool_line(&[]), "");
    }
}
