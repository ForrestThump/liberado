use dioxus::prelude::*;

use chat_client_contract::ConvHeader;

use crate::components::mcp_panel::McpPanel;

async fn fetch_conversations(api_base: String) -> Result<Vec<ConvHeader>, String> {
    let url = format!("{api_base}/api/conversations");
    let resp = reqwest::get(&url)
        .await
        .map_err(|e| format!("Failed to reach daemon: {e}"))?;
    let headers: Vec<ConvHeader> = resp
        .json()
        .await
        .map_err(|e| format!("Bad response: {e}"))?;
    Ok(headers)
}

fn relative_time(iso: &str) -> String {
    let parsed = chrono::DateTime::parse_from_rfc3339(iso)
        .or_else(|_| chrono::DateTime::parse_from_rfc3339(&format!("{iso}Z")))
        .or_else(|_| {
            chrono::NaiveDateTime::parse_from_str(iso, "%Y-%m-%dT%H:%M:%S%.f")
                .map(|naive| naive.and_utc().into())
        });
    let dt = match parsed {
        Ok(dt) => dt,
        Err(_) => return String::new(),
    };
    let now = chrono::Utc::now();
    let dur = now.signed_duration_since(dt);
    if dur.num_seconds() < 0 {
        return "just now".into();
    }
    let secs = dur.num_seconds() as u64;
    if secs < 60 {
        return format!("{}s ago", secs);
    }
    let mins = secs / 60;
    if mins < 60 {
        return format!("{}m ago", mins);
    }
    let hours = mins / 60;
    if hours < 24 {
        return format!("{}h ago", hours);
    }
    let days = hours / 24;
    if days < 30 {
        return format!("{}d ago", days);
    }
    dt.format("%b %d").to_string()
}

#[component]
pub fn Sidebar(
    api_base: String,
    active_conv_id: Signal<Option<String>>,
    collapsed: Signal<bool>,
) -> Element {
    let conversations = use_resource({
        let base = api_base.clone();
        move || {
            let _ = active_conv_id.read();
            fetch_conversations(base.clone())
        }
    });

    let toggle = move |_| collapsed.set(!collapsed());

    if collapsed() {
        return rsx! {
            div {
                class: "sidebar sidebar-collapsed",
                button {
                    class: "sidebar-toggle-btn",
                    onclick: toggle,
                    title: "Expand sidebar",
                    "☰"
                }
            }
        };
    }

    rsx! {
        div {
            class: "sidebar",
            div {
                class: "sidebar-header",
                button {
                    class: "sidebar-new-chat-btn",
                    onclick: move |_| {
                        active_conv_id.set(None);
                    },
                    "+ New Chat"
                }
                button {
                    class: "sidebar-collapse-btn",
                    onclick: toggle,
                    title: "Collapse sidebar",
                    "◄"
                }
            }
            div {
                class: "sidebar-list",
                match &*conversations.read() {
                    Some(Ok(list)) => {
                        if list.is_empty() {
                            rsx! {
                                p {
                                    class: "sidebar-empty",
                                    "No conversations yet."
                                }
                            }
                        } else {
                            rsx! {
                                for conv in list {
                                    ConvItem {
                                        key: "{conv.id}",
                                        conv: conv.clone(),
                                        is_active: active_conv_id.read().as_deref() == Some(&conv.id),
                                        on_select: {
                                            let mut active = active_conv_id;
                                            let id = conv.id.clone();
                                            move |_| active.set(Some(id.clone()))
                                        },
                                    }
                                }
                            }
                        }
                    }
                    Some(Err(e)) => rsx! {
                        p {
                            class: "sidebar-empty",
                            "Error: {e}"
                        }
                    },
                    None => rsx! {
                        p {
                            class: "sidebar-empty",
                            "Loading..."
                        }
                    },
                }
            }
            div {
                class: "sidebar-footer",
                McpPanel { api_base: api_base.clone() }
            }
        }
    }
}

#[component]
fn ConvItem(conv: ConvHeader, is_active: bool, on_select: EventHandler<MouseEvent>) -> Element {
    let cls = if is_active {
        "conv-item conv-item-active"
    } else {
        "conv-item"
    };
    let title = match &conv.title {
        Some(t) if !t.is_empty() => t.as_str(),
        _ => "Untitled",
    };

    rsx! {
        button {
            class: "{cls}",
            onclick: move |evt| on_select.call(evt),
            div {
                class: "conv-item-title",
                "{title}"
            }
            div {
                class: "conv-item-time",
                "{relative_time(&conv.created_at)}"
            }
        }
    }
}
