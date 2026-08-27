use dioxus::prelude::*;

use chat_client_contract::{CatalogResponse, McpInfo};

use crate::icons::{IconChevronDown, IconChevronRight};

async fn fetch_catalog(api_base: String) -> Result<CatalogResponse, String> {
    let url = format!("{api_base}/api/catalog");
    let resp = reqwest::get(&url)
        .await
        .map_err(|e| format!("Failed to reach daemon: {e}"))?;
    let catalog: CatalogResponse = resp
        .json()
        .await
        .map_err(|e| format!("Bad response: {e}"))?;
    Ok(catalog)
}

fn consequence_badge_class(consequence: &str) -> &'static str {
    match consequence {
        "read_only" => "consequence-badge read-only",
        "reversible" => "consequence-badge reversible",
        "irreversible" => "consequence-badge irreversible",
        "external" => "consequence-badge external",
        _ => "consequence-badge unknown",
    }
}

/// "N tools" for the MCP server row — prefers the daemon's own count, falls back to the name list,
/// and renders singular correctly. Empty when there are no tools.
fn tool_count_label(tool_count: usize, tool_names: &[String]) -> String {
    let count = if tool_count > 0 {
        tool_count
    } else {
        tool_names.len()
    };
    if count == 0 {
        String::new()
    } else {
        format!("{} tool{}", count, if count == 1 { "" } else { "s" })
    }
}

#[cfg(test)]
mod tests {
    use super::{consequence_badge_class, tool_count_label};

    fn names(n: usize) -> Vec<String> {
        (0..n).map(|i| format!("t{i}")).collect()
    }

    /// Every consequence the daemon can report has its own badge class, and anything it cannot
    /// name yet renders as `unknown` rather than panicking or colliding with a real class.
    #[test]
    fn each_consequence_has_its_own_badge() {
        assert_eq!(
            consequence_badge_class("read_only"),
            "consequence-badge read-only"
        );
        assert_eq!(
            consequence_badge_class("reversible"),
            "consequence-badge reversible"
        );
        assert_eq!(
            consequence_badge_class("irreversible"),
            "consequence-badge irreversible"
        );
        assert_eq!(
            consequence_badge_class("external"),
            "consequence-badge external"
        );
        assert_eq!(
            consequence_badge_class("something-new"),
            "consequence-badge unknown"
        );
    }

    /// The daemon's own count wins when it has one; the name list is the fallback for servers that
    /// report none.
    #[test]
    fn tool_count_prefers_the_daemons_count() {
        assert_eq!(tool_count_label(3, &names(1)), "3 tools");
        assert_eq!(tool_count_label(1, &names(5)), "1 tool");
        assert_eq!(tool_count_label(0, &names(2)), "2 tools");
        assert_eq!(tool_count_label(0, &names(1)), "1 tool");
        assert_eq!(tool_count_label(0, &names(0)), "");
        assert_eq!(tool_count_label(0, &[]), "");
    }
}

#[component]
pub fn McpPanel(api_base: String) -> Element {
    let catalog = use_resource({
        let base = api_base.clone();
        move || fetch_catalog(base.clone())
    });

    let mut expanded = use_signal(|| false);

    let count_display = match &*catalog.read() {
        Some(Ok(c)) => c.mcps.len().to_string(),
        _ => "-".to_string(),
    };

    rsx! {
        div {
            class: "mcp-panel",
            button {
                class: "mcp-panel-header",
                onclick: move |_| expanded.set(!expanded()),
                span { class: "mcp-panel-arrow",
                    if expanded() { IconChevronDown {} } else { IconChevronRight {} }
                }
                span { class: "mcp-panel-title", "MCP Servers" }
                span { class: "mcp-panel-count", "{count_display}" }
            }
            if expanded() {
                div {
                    class: "mcp-panel-body",
                    match &*catalog.read() {
                        Some(Ok(c)) => {
                            if c.mcps.is_empty() {
                                rsx! {
                                    p {
                                        class: "mcp-empty",
                                        "No MCP servers registered."
                                    }
                                }
                            } else {
                                rsx! {
                                    for mcp in c.mcps.iter() {
                                        McpServerItem { mcp: mcp.clone() }
                                    }
                                }
                            }
                        }
                        Some(Err(e)) => rsx! {
                            p { class: "mcp-empty", "Error: {e}" }
                        },
                        None => rsx! {
                            p { class: "mcp-empty", "Loading..." }
                        },
                    }
                }
            }
        }
    }
}

#[component]
fn McpServerItem(mcp: McpInfo) -> Element {
    let mut server_expanded = use_signal(|| false);
    let badge = consequence_badge_class(&mcp.consequence);

    let tool_count = tool_count_label(mcp.tool_count, &mcp.tool_names);

    rsx! {
        div {
            class: "mcp-server",
            button {
                class: "mcp-server-header",
                onclick: move |_| server_expanded.set(!server_expanded()),
                span { class: "mcp-server-arrow",
                    if server_expanded() { IconChevronDown {} } else { IconChevronRight {} }
                }
                div {
                    class: "mcp-server-info",
                    span { class: "mcp-server-name", "{mcp.name}" }
                    if !tool_count.is_empty() {
                        span { class: "mcp-server-tool-count", "{tool_count}" }
                    }
                }
                div {
                    class: "mcp-visibility",
                    span {
                        class: if mcp.visible_to_main_agent { "visibility-badge active" } else { "visibility-badge inactive" },
                        title: "Main agent (chat)",
                        "MA"
                    }
                    span {
                        class: if mcp.visible_to_dispatcher { "visibility-badge active" } else { "visibility-badge inactive" },
                        title: "Dispatcher (reactive pipeline)",
                        "DX"
                    }
                }
                span { class: "{badge}", "{mcp.consequence}" }
            }
            if server_expanded() {
                div {
                    class: "mcp-server-body",
                    if !mcp.description.is_empty() {
                        p { class: "mcp-server-desc", "{mcp.description}" }
                    }
                    div {
                        class: "mcp-server-visibility",
                        span {
                            class: if mcp.visible_to_main_agent { "visibility-pill active" } else { "visibility-pill inactive" },
                            "Main agent"
                        }
                        span {
                            class: if mcp.visible_to_dispatcher { "visibility-pill active" } else { "visibility-pill inactive" },
                            "Dispatcher"
                        }
                    }
                    if !mcp.tool_names.is_empty() {
                        div {
                            class: "mcp-tool-list",
                            for tool in mcp.tool_names.iter() {
                                div {
                                    class: "mcp-tool",
                                    span { class: "mcp-tool-bullet", "\u{2022}" }
                                    span { class: "mcp-tool-name", "{tool}" }
                                }
                            }
                        }
                    }
                    if let Some(ref prov) = mcp.provenance {
                        div {
                            class: "mcp-server-provenance",
                            "{prov}"
                        }
                    }
                }
            }
        }
    }
}
