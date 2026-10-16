//! Agent grouping, conversation rows, and search results.
//!
//! Split from `sidebar.rs` so that file stays inside the module-health boundaries.

use std::collections::HashMap;

use dioxus::prelude::*;

use chat_client_contract::{ConvHeader, ConversationSearchResult};

use crate::icons::{IconChevronDown, IconChevronRight};

use super::row_menu::{RowOptions, conversation_row_dom_id, delete_block_for};

fn profile_seed(profiles: &HashMap<String, String>, id: &str) -> String {
    profiles.get(id).cloned().unwrap_or_default()
}

// ── Agent categorization ──────────────────────────────────────────────────

const PINNED_PREFIXES: &[&str] = &["Grok Bot", "Liberado-Budget"];

fn agent_category(title: &str) -> Option<&'static str> {
    if title.contains("Disk Saver") || title.starts_with("Disk") {
        Some("Liberado Development")
    } else if title.contains("Financial") || title.contains("Budget") {
        Some("Liberado Finance")
    } else {
        None
    }
}

pub(super) fn pinned(title: &str) -> bool {
    PINNED_PREFIXES.iter().any(|prefix| {
        title
            .to_ascii_lowercase()
            .starts_with(&prefix.to_ascii_lowercase())
    })
}

const CATEGORY_ORDER: &[&str] = &["Liberado Development", "Liberado Finance"];

pub(super) fn group_agents<'a>(agents: &[&'a ConvHeader]) -> GroupedAgents<'a> {
    let mut pinned_v = Vec::new();
    let mut groups: std::collections::BTreeMap<&'static str, Vec<&'a ConvHeader>> =
        std::collections::BTreeMap::new();
    let mut others = Vec::new();
    for conv in agents {
        let title = conv.title.as_deref().unwrap_or("(untitled)");
        if pinned(title) {
            pinned_v.push(*conv);
        } else if let Some(cat) = agent_category(title) {
            groups.entry(cat).or_default().push(*conv);
        } else {
            others.push(*conv);
        }
    }
    let mut grouped: Vec<(&'static str, Vec<&'a ConvHeader>)> = groups.into_iter().collect();
    grouped.sort_by_key(|(cat, _)| {
        CATEGORY_ORDER
            .iter()
            .position(|c| *c == *cat)
            .unwrap_or(usize::MAX)
    });
    GroupedAgents {
        pinned: pinned_v,
        categories: grouped,
        others,
    }
}

pub(super) struct GroupedAgents<'a> {
    pub(super) pinned: Vec<&'a ConvHeader>,
    pub(super) categories: Vec<(&'static str, Vec<&'a ConvHeader>)>,
    pub(super) others: Vec<&'a ConvHeader>,
}

// ── Sidebar rows ───────────────────────────────────────────────────────────

pub(super) fn row_dot_color(title: &str) -> &'static str {
    let lower = title.to_ascii_lowercase();
    if lower.starts_with("grok") {
        "var(--dot-orange)"
    } else if lower.starts_with("liberado-budget") {
        "var(--dot-green)"
    } else if lower.starts_with("disk") {
        "var(--dot-orange)"
    } else if lower.starts_with("self help") {
        "var(--dot-green)"
    } else if lower.starts_with("life") || lower.starts_with("homelab") {
        "var(--dot-cyan)"
    } else if pinned(title) {
        "var(--dot-accent)"
    } else {
        "var(--dot-neutral)"
    }
}

pub(super) fn truncate_title(title: &str, max: usize) -> String {
    if title.chars().count() <= max {
        return title.to_string();
    }
    let truncated: String = title.chars().take(max.saturating_sub(1)).collect();
    format!("{truncated}…")
}

fn conv_display_title(conv: &ConvHeader) -> String {
    let raw = conv.title.as_deref().unwrap_or("(untitled)");
    let title = truncate_title(raw, 30);
    match conv.project.as_deref().filter(|name| !name.is_empty()) {
        Some(project) => format!("{title} · {project}"),
        None => title,
    }
}

#[component]
fn SidebarRowCompact(
    conv: ConvHeader,
    profile_name: String,
    active: Signal<Option<String>>,
    menu_for: Signal<Option<String>>,
    action_error: Signal<Option<String>>,
    delete_block: Option<&'static str>,
    on_pick: EventHandler<String>,
    on_delete: EventHandler<String>,
    on_rename: EventHandler<(String, String)>,
) -> Element {
    let id = conv.id.clone();
    let is_active = active().as_deref() == Some(id.as_str());
    let cls = if is_active {
        "grok-row grok-row-active"
    } else {
        "grok-row"
    };
    let display = conv_display_title(&conv);
    let seed = profile_name;
    let dot = row_dot_color(display.as_str());
    let id_for_pick = id.clone();
    let row_dom_id = conversation_row_dom_id(&id);
    rsx! {
        li {
            id: "{row_dom_id}",
            class: "{cls}",
            button {
                class: "grok-row-btn",
                r#type: "button",
                title: "{display}",
                onclick: move |_| on_pick.call(id_for_pick.clone()),
                span {
                    class: "grok-row-dot",
                    style: "background: {dot};",
                    "aria-hidden": "true"
                }
                span { class: "grok-row-title", "{display}" }
            }
            RowOptions {
                id: id.clone(),
                seed,
                display: display.clone(),
                noun: "agent",
                menu_for,
                action_error,
                delete_block,
                on_delete,
                on_rename,
            }
        }
    }
}

#[component]
fn SidebarRowChat(
    conv: ConvHeader,
    active: Signal<Option<String>>,
    menu_for: Signal<Option<String>>,
    action_error: Signal<Option<String>>,
    editor_ids: Vec<String>,
    on_pick: EventHandler<String>,
    on_delete: EventHandler<String>,
    on_rename: EventHandler<(String, String)>,
) -> Element {
    let id = conv.id.clone();
    let is_active = active().as_deref() == Some(id.as_str());
    let cls = if is_active {
        "grok-row grok-row-active"
    } else {
        "grok-row"
    };
    let display = conv_display_title(&conv);
    let seed = conv.title.clone().unwrap_or_default();
    let time = relative_time(&conv.created_at);
    let id_for_pick = id.clone();
    let row_dom_id = conversation_row_dom_id(&id);
    let delete_block = delete_block_for(&id, &[], &editor_ids);
    rsx! {
        li {
            id: "{row_dom_id}",
            class: "{cls}",
            button {
                class: "grok-row-btn",
                r#type: "button",
                title: "{display}",
                onclick: move |_| on_pick.call(id_for_pick.clone()),
                span { class: "grok-row-title", "{display}" }
                if !time.is_empty() {
                    span { class: "grok-row-time", "{time}" }
                }
            }
            RowOptions {
                id: id.clone(),
                seed,
                display: display.clone(),
                noun: "chat",
                menu_for,
                action_error,
                delete_block,
                on_delete,
                on_rename,
            }
        }
    }
}

#[component]
pub(super) fn PinnedList(
    items: Vec<ConvHeader>,
    profiles: HashMap<String, String>,
    active: Signal<Option<String>>,
    menu_for: Signal<Option<String>>,
    protected_ids: Vec<String>,
    editor_ids: Vec<String>,
    on_pick: EventHandler<String>,
    on_delete: EventHandler<String>,
    on_rename: EventHandler<(String, String)>,
    action_error: Signal<Option<String>>,
) -> Element {
    if items.is_empty() {
        return rsx! {};
    }
    rsx! {
        div {
            class: "sidebar-subheader",
            "Pinned"
        }
        ul {
            class: "grok-list",
            for conv in items.iter() {
                SidebarRowCompact {
                    conv: conv.clone(),
                    profile_name: profile_seed(&profiles, &conv.id),
                    active: active,
                    menu_for: menu_for,
                    action_error: action_error,
                    delete_block: delete_block_for(&conv.id, &protected_ids, &editor_ids),
                    on_pick: on_pick,
                    on_delete: on_delete,
                    on_rename: on_rename,
                }
            }
        }
    }
}

#[component]
pub(super) fn CategoryList(
    label: &'static str,
    members: Vec<ConvHeader>,
    profiles: HashMap<String, String>,
    active: Signal<Option<String>>,
    menu_for: Signal<Option<String>>,
    protected_ids: Vec<String>,
    editor_ids: Vec<String>,
    on_pick: EventHandler<String>,
    on_delete: EventHandler<String>,
    on_rename: EventHandler<(String, String)>,
    action_error: Signal<Option<String>>,
) -> Element {
    if members.is_empty() {
        return rsx! {};
    }
    rsx! {
        div {
            class: "sidebar-subheader",
            "{label}"
        }
        ul {
            class: "grok-list",
            for conv in members.iter() {
                SidebarRowCompact {
                    conv: conv.clone(),
                    profile_name: profile_seed(&profiles, &conv.id),
                    active: active,
                    menu_for: menu_for,
                    action_error: action_error,
                    delete_block: delete_block_for(&conv.id, &protected_ids, &editor_ids),
                    on_pick: on_pick,
                    on_delete: on_delete,
                    on_rename: on_rename,
                }
            }
        }
    }
}

#[component]
pub(super) fn ShowMoreRow(count: usize, expanded: Signal<bool>) -> Element {
    rsx! {
        button {
            class: "sidebar-show-more",
            r#type: "button",
            onclick: move |_| expanded.set(!expanded()),
            if expanded() { IconChevronDown {} } else { IconChevronRight {} }
            span {
                if expanded() { "Show less ({count})" } else { "Show more ({count})" }
            }
        }
    }
}

#[component]
pub(super) fn AgentTailList(
    items: Vec<ConvHeader>,
    profiles: HashMap<String, String>,
    active: Signal<Option<String>>,
    menu_for: Signal<Option<String>>,
    protected_ids: Vec<String>,
    editor_ids: Vec<String>,
    on_pick: EventHandler<String>,
    on_delete: EventHandler<String>,
    on_rename: EventHandler<(String, String)>,
    action_error: Signal<Option<String>>,
) -> Element {
    rsx! {
        ul {
            class: "grok-list",
            for conv in items.iter() {
                SidebarRowCompact {
                    conv: conv.clone(),
                    profile_name: profile_seed(&profiles, &conv.id),
                    active: active,
                    menu_for: menu_for,
                    action_error: action_error,
                    delete_block: delete_block_for(&conv.id, &protected_ids, &editor_ids),
                    on_pick: on_pick,
                    on_delete: on_delete,
                    on_rename: on_rename,
                }
            }
        }
    }
}

#[component]
pub(super) fn ChatList(
    items: Vec<ConvHeader>,
    active: Signal<Option<String>>,
    menu_for: Signal<Option<String>>,
    editor_ids: Vec<String>,
    on_pick: EventHandler<String>,
    on_delete: EventHandler<String>,
    on_rename: EventHandler<(String, String)>,
    action_error: Signal<Option<String>>,
) -> Element {
    rsx! {
        ul {
            class: "grok-list",
            for conv in items.iter() {
                SidebarRowChat {
                    conv: conv.clone(),
                    active: active,
                    menu_for: menu_for,
                    action_error: action_error,
                    editor_ids: editor_ids.clone(),
                    on_pick: on_pick,
                    on_delete: on_delete,
                    on_rename: on_rename,
                }
            }
        }
    }
}

fn search_noun(agents_shelf: bool) -> &'static str {
    if agents_shelf {
        "agent"
    } else {
        "conversation"
    }
}

fn search_seed(
    agents_shelf: bool,
    profiles: &HashMap<String, String>,
    id: &str,
    title: &Option<String>,
) -> String {
    if agents_shelf {
        profile_seed(profiles, id)
    } else {
        title.clone().unwrap_or_default()
    }
}

#[component]
pub(super) fn SearchResultList(
    results: Vec<ConversationSearchResult>,
    menu_for: Signal<Option<String>>,
    action_error: Signal<Option<String>>,
    agents_shelf: bool,
    profiles: HashMap<String, String>,
    on_pick: EventHandler<String>,
    on_delete: EventHandler<String>,
    on_rename: EventHandler<(String, String)>,
    on_delete_profile: EventHandler<String>,
    on_rename_profile: EventHandler<(String, String)>,
    protected_ids: Vec<String>,
    editor_ids: Vec<String>,
) -> Element {
    if results.is_empty() {
        return rsx! {
            div { class: "sidebar-empty", "No matches." }
        };
    }
    let noun = search_noun(agents_shelf);
    let delete = if agents_shelf {
        on_delete_profile
    } else {
        on_delete
    };
    let rename = if agents_shelf {
        on_rename_profile
    } else {
        on_rename
    };
    rsx! {
        ul {
            class: "grok-list",
            for r in results.iter() {
                {
                    let id = r.conversation_id.clone();
                    let title = r.title.clone().unwrap_or_else(|| "(untitled)".into());
                    let seed = search_seed(agents_shelf, &profiles, &id, &r.title);
                    let row_dom_id = conversation_row_dom_id(&id);
                    let delete_block = delete_block_for(&id, &protected_ids, &editor_ids);
                    rsx! {
                        li {
                            id: "{row_dom_id}",
                            class: "grok-row",
                            button {
                                class: "grok-row-btn",
                                r#type: "button",
                                onclick: move |_| on_pick.call(id.clone()),
                                span { class: "grok-row-title", "{title}" }
                            }
                            RowOptions {
                                id: id.clone(),
                                seed,
                                display: title.clone(),
                                noun,
                                menu_for,
                                action_error,
                                delete_block,
                                on_delete: delete,
                                on_rename: rename,
                            }
                        }
                    }
                }
            }
        }
    }
}

// ── Time formatter ─────────────────────────────────────────────────────────

pub(in crate::components) fn relative_time(iso: &str) -> String {
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
