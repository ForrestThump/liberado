use dioxus::prelude::*;

use chat_client_contract::{ConvHeader, ConversationSearchResponse, ConversationSearchResult};

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

async fn fetch_search_results(
    api_base: String,
    query: String,
) -> Result<Vec<ConversationSearchResult>, String> {
    let url = format!("{api_base}/api/conversations/search");
    let resp = reqwest::Client::new()
        .get(&url)
        .query(&[("q", query.as_str())])
        .send()
        .await
        .map_err(|e| format!("Search failed: {e}"))?;
    let body: ConversationSearchResponse = resp
        .json()
        .await
        .map_err(|e| format!("Bad search response: {e}"))?;
    Ok(body.results)
}

/// Close the sidebar after the user picks something in it — but only where it is an overlay
/// sitting on top of the chat. On a phone the sidebar covers the whole content area, so leaving it
/// open after a selection hides the very conversation you just chose and forces a second tap. On a
/// wide screen it is a persistent side panel, and closing it would take the list away for no reason.
fn collapse_after_pick(mut collapsed: Signal<bool>) {
    if crate::is_narrow_viewport() {
        collapsed.set(true);
    }
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

    let mut search_query = use_signal(String::new);

    let search_results = use_resource({
        let base = api_base.clone();
        move || {
            let q = search_query.read().trim().to_string();
            let base = base.clone(); // fresh per-call clone; the async block below consumes it
            async move {
                if q.is_empty() {
                    None
                } else {
                    Some(fetch_search_results(base, q).await)
                }
            }
        }
    });

    let toggle = move |_| collapsed.set(!collapsed());

    // Collapsed renders *nothing* — not a slim rail. A rail spent a vertical strip of every screen
    // on one button, which on a phone is a chunk of the conversation's width. The button that brings
    // this back lives in the app header (see main.rs), where it costs no layout width at all.
    if collapsed() {
        return rsx! {};
    }

    rsx! {
        div {
            class: "sidebar",
            div {
                class: "sidebar-header",
                button {
                    class: "sidebar-new-chat-btn",
                    onclick: move |_| {
                        // Only reset when the focused chat has actually been used. `active_conv_id`
                        // is set from the SSE `session` event on the first send, so `None` already
                        // means "fresh and empty" — resetting again would clear nothing and just
                        // re-render. (No session is minted here either way: this is client state,
                        // and the server creates one when the first message is sent.)
                        if active_conv_id.read().is_some() {
                            active_conv_id.set(None);
                        }
                        collapse_after_pick(collapsed);
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
                class: "sidebar-search",
                input {
                    class: "sidebar-search-input",
                    r#type: "search",
                    placeholder: "Search conversations...",
                    value: "{search_query}",
                    oninput: move |evt| search_query.set(evt.value()),
                }
            }
            div {
                class: "sidebar-list",
                if !search_query.read().trim().is_empty() {
                    match &*search_results.read() {
                        Some(Some(Ok(list))) => {
                            if list.is_empty() {
                                rsx! {
                                    p { class: "sidebar-empty", "No results." }
                                }
                            } else {
                                rsx! {
                                    for result in list {
                                        SearchResultItem {
                                            key: "{result.conversation_id}",
                                            result: result.clone(),
                                            is_active: active_conv_id.read().as_deref() == Some(&result.conversation_id),
                                            on_select: {
                                                let mut active = active_conv_id;
                                                let id = result.conversation_id.clone();
                                                move |_| {
                                                    active.set(Some(id.clone()));
                                                    collapse_after_pick(collapsed);
                                                }
                                            },
                                        }
                                    }
                                }
                            }
                        }
                        Some(Some(Err(e))) => rsx! {
                            p { class: "sidebar-empty", "Error: {e}" }
                        },
                        Some(None) | None => rsx! {
                            p { class: "sidebar-empty", "Searching..." }
                        },
                    }
                } else {
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
                                                move |_| {
                                                    active.set(Some(id.clone()));
                                                    collapse_after_pick(collapsed);
                                                }
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

#[component]
fn SearchResultItem(
    result: ConversationSearchResult,
    is_active: bool,
    on_select: EventHandler<MouseEvent>,
) -> Element {
    let cls = if is_active {
        "conv-item conv-item-active"
    } else {
        "conv-item"
    };
    let title = match &result.title {
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
                "{relative_time(&result.created_at)}"
            }
            for m in result.matches.iter() {
                div {
                    class: "search-result-snippet",
                    "{m.content_snippet}"
                }
            }
        }
    }
}
