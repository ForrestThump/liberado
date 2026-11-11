//! Plugins view — MCP servers from `GET /api/catalog`.
//!
//! The switch calls `PUT /api/plugins/{id}` with `{"enabled": bool}`. It can turn
//! chat access off, and back on only when policy already grants the server. The
//! error text is on the row, so a phone does not need a tooltip.

use dioxus::prelude::*;

use chat_client_contract::{CatalogResponse, McpInfo};

/// Shown under the page title. The switch itself does not depend on a hover.
pub(crate) const PLUGIN_TOGGLE_NOTE: &str = "This switch turns chat access to a server on or off. It cannot grant a server that policy does not already grant.";

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

pub(crate) fn plugin_error(status: u16, body: &str) -> String {
    let detail = serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|value| {
            value
                .get("error")
                .and_then(|error| error.as_str())
                .map(str::to_string)
        });
    match detail {
        Some(text) => text,
        None => format!("Plugin update failed: HTTP {status}"),
    }
}

async fn fetch_catalog(api_base: String) -> Result<CatalogResponse, String> {
    let url = format!("{api_base}/api/catalog");
    let resp = crate::http::get(&url)
        .await
        .map_err(|e| format!("Failed to reach daemon: {e}"))?;
    if !resp.status().is_success() {
        let status = resp.status();
        return Err(format!("Catalog unavailable ({status})"));
    }
    resp.json().await.map_err(|e| format!("Bad response: {e}"))
}

async fn put_plugin_enabled(api_base: &str, name: &str, enabled: bool) -> Result<(), String> {
    let url = format!("{api_base}/api/plugins/{name}");
    let resp = crate::http::put_json(&url, &serde_json::json!({ "enabled": enabled }))
        .await
        .map_err(|e| format!("Failed to reach daemon: {e}"))?;
    let status = resp.status().as_u16();
    if (200..300).contains(&status) {
        Ok(())
    } else {
        let body = resp.text().await.unwrap_or_default();
        Err(plugin_error(status, &body))
    }
}

#[cfg_attr(not(target_arch = "wasm32"), allow(unused_mut))]
fn start_plugin_toggle(
    api_base: String,
    name: String,
    enabled: bool,
    mut error: Signal<Option<String>>,
    on_saved: EventHandler<()>,
) {
    #[cfg(target_arch = "wasm32")]
    wasm_bindgen_futures::spawn_local(async move {
        match put_plugin_enabled(&api_base, &name, enabled).await {
            Ok(()) => {
                error.set(None);
                on_saved.call(());
            }
            Err(message) => error.set(Some(message)),
        }
    });
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = (api_base, name, enabled, error, on_saved);
    }
}

#[component]
pub fn Plugins(api_base: String) -> Element {
    let mut catalog = use_resource({
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
                p { class: "grok-page-subtitle", "{PLUGIN_TOGGLE_NOTE}" }
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
                                PluginRow {
                                    mcp: mcp.clone(),
                                    api_base: api_base.clone(),
                                    on_saved: move |_| catalog.restart(),
                                }
                            }
                        }
                    },
                }
            }
        }
    }
}

#[component]
fn PluginRow(mcp: McpInfo, api_base: String, on_saved: EventHandler<()>) -> Element {
    let error = use_signal(|| None::<String>);
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
    let name = mcp.name.clone();
    let next = !on;

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
                    title: PLUGIN_TOGGLE_NOTE,
                    "aria-label": "{label}",
                    "aria-checked": "{checked}",
                    onclick: move |_| {
                        start_plugin_toggle(
                            api_base.clone(),
                            name.clone(),
                            next,
                            error,
                            on_saved,
                        );
                    },
                }
            }
            p { class: "plugin-meta", "{meta}" }
            if let Some(message) = error() {
                p { class: "plugin-status", "{message}" }
            }
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
    fn the_switch_calls_put_and_shows_a_refusal_without_a_hover() {
        assert!(PLUGIN_TOGGLE_NOTE.contains("cannot grant"));
        let src = include_str!("plugins.rs");
        let page = src.split("mod tests").next().unwrap_or(src);
        assert!(page.contains("/api/plugins/{name}"));
        assert!(page.contains("\"enabled\": enabled"));
        assert!(page.contains("onclick"));
        assert!(!page.contains("disabled:"));
        assert!(page.contains("plugin-status"));
        let css = include_str!("../styles/main.css");
        let switch = css_block(css, ".plugin-switch");
        assert!(switch.contains("cursor: pointer"), "{switch}");
        let refusal = plugin_error(
            409,
            r#"{"error":"policy does not grant 'tasks' to chat; this switch cannot widen access"}"#,
        );
        assert!(refusal.contains("cannot widen"), "{refusal}");
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

    /// The list used to sit in a short nested scroller. It now fills the page under
    /// the header and scrolls only when the servers do not fit.
    #[test]
    fn the_server_list_fills_the_page_and_scrolls_only_on_overflow() {
        let css = include_str!("../styles/main.css");
        let page = css_block(css, ".plugins-page");
        assert!(page.contains("flex: 1"), "{page}");
        assert!(page.contains("min-height: 0"), "{page}");
        assert!(page.contains("overflow: hidden"), "{page}");
        let body = css_block(css, ".plugins-page .grok-page-body");
        assert!(body.contains("min-height: 0"), "{body}");
        assert!(body.contains("overflow: hidden"), "{body}");
        let list = css_block(css, ".plugins-page .plugin-list");
        assert!(list.contains("flex: 1"), "{list}");
        assert!(list.contains("min-height: 0"), "{list}");
        assert!(list.contains("overflow-y: auto"), "{list}");
        assert!(
            !list.contains("max-height"),
            "a max-height brings back the short scroll box: {list}"
        );
    }

    fn css_block<'a>(css: &'a str, selector: &str) -> &'a str {
        let needle = format!("{selector} {{");
        let start = css
            .find(&needle)
            .unwrap_or_else(|| panic!("missing CSS rule {selector}"))
            + needle.len();
        let rest = &css[start..];
        let end = rest
            .find('}')
            .unwrap_or_else(|| panic!("unterminated CSS rule {selector}"));
        &rest[..end]
    }
}
