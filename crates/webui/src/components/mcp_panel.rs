use dioxus::prelude::*;

use chat_client_contract::{CatalogResponse, McpInfo};

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

#[cfg(test)]
mod tests {
    use super::consequence_badge_class;

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
                    if expanded() { "\u{25BC}" } else { "\u{25B8}" }
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

    let tool_count = if mcp.tool_count > 0 {
        format!(
            "{} tool{}",
            mcp.tool_count,
            if mcp.tool_count == 1 { "" } else { "s" }
        )
    } else if !mcp.tool_names.is_empty() {
        format!(
            "{} tool{}",
            mcp.tool_names.len(),
            if mcp.tool_names.len() == 1 { "" } else { "s" }
        )
    } else {
        String::new()
    };

    rsx! {
        div {
            class: "mcp-server",
            button {
                class: "mcp-server-header",
                onclick: move |_| server_expanded.set(!server_expanded()),
                span { class: "mcp-server-arrow",
                    if server_expanded() { "\u{25BC}" } else { "\u{25B8}" }
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
