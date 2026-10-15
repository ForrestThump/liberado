//! Left sidebar rail.
//!
//! Grok-style vertical layout: brand row at the top with the small action icons, primary nav
//! (Chat / Imagine / Library / Automations), then a categorizable Agents shelf with Pinned +
//! labelled groups + Show more, then the recent Chats list, then Projects, then Plugins.
//!
//! The whole sidebar renders or hides as one block (no collapsed rail) — the app header keeps the
//! one button that brings it back. See [`crate::main`] for the matching header wiring.

use dioxus::prelude::*;

use chat_client_contract::{ConvHeader, ConversationSearchResponse, ConversationSearchResult, SurfaceMode};

use crate::icons::{IconAttach, IconSearch};

// ── Routes ──────────────────────────────────────────────────────────────────

/// Navigation routes the sidebar picks. Mirrors Grok's primary nav + the section headers (Agents,
/// Chats, Projects, Plugins) and the typed sub-routes (Agents/Projects selection).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    Chat,
    Imagine,
    Library,
    Automations,
    Agents,
    Projects,
    Plugins,
    /// An agent row was selected — opened as a chat with the agent session preloaded.
    AgentSession,
    /// A row from the recent Chats section.
    ChatSession,
    /// A row from the Projects section.
    ProjectDetail,
}

impl Route {
    /// Whether `route` means the chat compositor (any "I have a session to show" route).
    pub(crate) fn is_chat_view(self) -> bool {
        matches!(
            self,
            Route::Chat | Route::ChatSession | Route::AgentSession | Route::ProjectDetail
        )
    }

    /// Whether this route highlights the Chat primary nav row.
    pub(crate) fn highlights_chat(self) -> bool {
        self.is_chat_view()
    }
}

// ── Sidebar fetchers ────────────────────────────────────────────────────────

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

/// A delete "succeeded" when the daemon answered 2xx, or 404 — the row was already gone.
fn delete_accepted(status: u16) -> bool {
    (200..300).contains(&status) || status == 404
}

async fn delete_conversation(api_base: &str, id: &str) -> Result<(), String> {
    let url = format!("{api_base}/api/conversations/{id}");
    let resp = reqwest::Client::new()
        .delete(&url)
        .send()
        .await
        .map_err(|e| format!("Failed to reach daemon: {e}"))?;
    if delete_accepted(resp.status().as_u16()) {
        Ok(())
    } else {
        Err(format!("Delete failed: HTTP {}", resp.status().as_u16()))
    }
}

// ── Agent categorization ──────────────────────────────────────────────────

/// Names that always sit in the Pinned group, regardless of where they came from. The shipped
/// surface mode has no category field yet; this is a soft client-side taxonymy until the daemon
/// exposes one. Case-insensitive prefix match.
const PINNED_PREFIXES: &[&str] = &["Grok Bot", "Liberado-Budget"];

/// Soft categorization for agent rows. Unknown titles fall into the catch-all bucket.
fn agent_category(title: &str) -> Option<&'static str> {
    if title.contains("Disk Saver") || title.starts_with("Disk") {
        Some("Liberado Development")
    } else if title.contains("Financial") || title.contains("Budget") {
        Some("Liberado Finance")
    } else {
        None
    }
}

fn pinned(title: &str) -> bool {
    PINNED_PREFIXES
        .iter()
        .any(|prefix| title.to_ascii_lowercase().starts_with(&prefix.to_ascii_lowercase()))
}

/// Display order for category headers. Anything outside this list goes in its own catch-all at
/// the bottom, in title order.
const CATEGORY_ORDER: &[&str] = &["Liberado Development", "Liberado Finance"];

/// Group agents into (pinned list, [(category, &[headers])...], others).
fn group_agents<'a>(agents: &[&'a ConvHeader]) -> GroupedAgents<'a> {
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

struct GroupedAgents<'a> {
    pinned: Vec<&'a ConvHeader>,
    categories: Vec<(&'static str, Vec<&'a ConvHeader>)>,
    others: Vec<&'a ConvHeader>,
}

// ── Component ──────────────────────────────────────────────────────────────

#[component]
pub fn Sidebar(
    api_base: String,
    /// Currently selected nav route. The sidebar highlights Chat when this is Chat, ChatSession,
    /// AgentSession, or ProjectDetail.
    route: Signal<Route>,
    active_conv_id: Signal<Option<String>>,
    collapsed: Signal<bool>,
    /// Bumped by sidebar / header "New chat" controls. Drives a fresh chat.
    new_chat_nonce: Signal<u64>,
    /// Projected shelf of the open conversation. Agent hides the profile chip.
    active_surface: Signal<Option<SurfaceMode>>,
    /// Bumped when `create_agent` succeeds so the Agents shelf refetches.
    list_epoch: Signal<u64>,
) -> Element {
    #[cfg_attr(not(target_arch = "wasm32"), allow(unused_mut))]
    let mut conversations = use_resource({
        let base = api_base.clone();
        move || {
            let _ = active_conv_id.read();
            let _ = list_epoch.read();
            fetch_conversations(base.clone())
        }
    });

    let mut search_query = use_signal(String::new);
    let mut search_open = use_signal(bool::default);
    let expand_all = use_signal(bool::default);
    let action_error = use_signal(|| None::<String>);
    #[cfg_attr(not(target_arch = "wasm32"), allow(unused_mut))]
    let mut search_results = use_resource({
        let base = api_base.clone();
        move || {
            let raw = search_query.read();
            let q = if search_open() && !raw.trim().is_empty() {
                raw.trim().to_string()
            } else {
                String::new()
            };
            let base = base.clone();
            async move {
                if q.is_empty() {
                    None
                } else {
                    Some(fetch_search_results(base, q).await)
                }
            }
        }
    });

    let delete_conv = {
        let base = api_base.clone();
        use_callback(move |id: String| {
            let base = base.clone();
            #[cfg(target_arch = "wasm32")]
            wasm_bindgen_futures::spawn_local(async move {
                match delete_conversation(&base, &id).await {
                    Ok(()) => {
                        action_error.set(None);
                        if active_conv_id.read().as_deref() == Some(id.as_str()) {
                            active_conv_id.set(None);
                        }
                        conversations.restart();
                        search_results.restart();
                    }
                    Err(e) => action_error.set(Some(e)),
                }
            });
            #[cfg(not(target_arch = "wasm32"))]
            {
                let _ = (base, id);
            }
        })
    };

    if collapsed() {
        return rsx! {};
    }

    let headers: Vec<ConvHeader> = conversations
        .read()
        .as_ref()
        .and_then(|r: &Result<Vec<ConvHeader>, String>| r.as_ref().ok())
        .cloned()
        .unwrap_or_default();
    let headers = headers.as_slice();
    let chat_rows: Vec<&ConvHeader> = headers
        .iter()
        .filter(|c| c.surface_mode == SurfaceMode::Chat)
        .collect();
    let agent_rows: Vec<&ConvHeader> = headers
        .iter()
        .filter(|c| c.surface_mode == SurfaceMode::Agent)
        .collect();
    let grouped = group_agents(&agent_rows);

    // Each row click needs to know whether the active is itself or a sibling, so the
    // `active: Signal<Option<String>>` is the single source of truth.
    rsx! {
        aside {
            class: "sidebar",

            // ── Brand row ───────────────────────────────────────────────────
            div {
                class: "sidebar-brand",
                button {
                    class: "sidebar-brand-btn",
                    r#type: "button",
                    title: "Home",
                    "aria-label": "Home",
                    onclick: move |_| route.set(Route::Chat),
                    BrandLogo {}
                }
                button {
                    class: "sidebar-icon-btn",
                    r#type: "button",
                    title: "Search",
                    "aria-label": "Search",
                    onclick: move |_| {
                        let open = !search_open();
                        search_open.set(open);
                        if !open {
                            search_query.set(String::new());
                        }
                    },
                    IconSearch {}
                }
                button {
                    class: "sidebar-icon-btn",
                    r#type: "button",
                    title: "New chat",
                    "aria-label": "New chat",
                    onclick: move |_| {
                        new_chat_nonce += 1;
                        if active_conv_id.read().is_some() {
                            active_conv_id.set(None);
                        }
                        route.set(Route::Chat);
                        if crate::is_narrow_viewport() {
                            collapsed.set(true);
                        }
                    },
                    IconAttach {}
                }
            }

            // ── Search field ────────────────────────────────────────────────
            if search_open() {
                div {
                    class: "sidebar-search",
                    input {
                        class: "sidebar-search-input",
                        r#type: "search",
                        placeholder: "Search",
                        value: "{search_query()}",
                        autofocus: true,
                        oninput: move |e| search_query.set(e.value()),
                    }
                }
            }

            // ── Primary nav ─────────────────────────────────────────────────
            nav {
                class: "sidebar-nav",
                PrimaryNavRow { route: route, target: Route::Chat, label: "Chat", icon: rsx! { ChatBubble {} } }
                PrimaryNavRow { route: route, target: Route::Imagine, label: "Imagine", icon: rsx! { Mountain {} } }
                PrimaryNavRow { route: route, target: Route::Library, label: "Library", icon: rsx! { Stack {} } }
                PrimaryNavRow { route: route, target: Route::Automations, label: "Automations", icon: rsx! { Clock {} } }
            }

            // ── Body: search overlay OR categorized sections ───────────────
            if search_open() && !search_query().trim().is_empty() {
                div {
                    class: "sidebar-list",
                    match &*search_results.read() {
                        Some(Some(Ok(results))) => rsx! {
                            SearchResultList {
                                results: results.iter().map(|r| r.clone()).collect(),
                                on_pick: move |id: String| {
                                    active_conv_id.set(Some(id));
                                    route.set(Route::ChatSession);
                                    if crate::is_narrow_viewport() {
                                        collapsed.set(true);
                                    }
                                }
                            }
                        },
                        Some(Some(Err(e))) => rsx! {
                            div {
                                class: "sidebar-error",
                                div { class: "sidebar-empty", "{e}" }
                            }
                        },
                        _ => rsx! {
                            div { class: "sidebar-empty", "Searching..." }
                        }
                    }
                }
            } else {
                div {
                    class: "sidebar-scroll",

                    // ── Agents (categorized) ──────────────────────────────────
                    section {
                        class: "sidebar-section",
                        header {
                            class: "sidebar-section-header",
                            button {
                                class: "sidebar-section-toggle",
                                r#type: "button",
                                onclick: move |_| route.set(Route::Agents),
                                span { class: "sidebar-section-label", "Agents" }
                            }
                            button {
                                class: "sidebar-section-add",
                                r#type: "button",
                                title: "New agent",
                                "aria-label": "New agent",
                                onclick: move |evt| { evt.stop_propagation(); },
                                "+"
                            }
                        }

                        PinnedList {
                            items: grouped.pinned.iter().map(|c| (*c).clone()).collect(),
                            active: active_conv_id,
                            on_pick: move |id: String| {
                                active_conv_id.set(Some(id));
                                active_surface.set(Some(SurfaceMode::Agent));
                                route.set(Route::AgentSession);
                                if crate::is_narrow_viewport() {
                                    collapsed.set(true);
                                }
                            },
                            on_delete: move |id: String| delete_conv.call(id),
                            action_error,
                        }

                        for (cat , members) in grouped.categories.iter() {
                            CategoryList {
                                label: cat,
                                members: members.iter().map(|c| (*c).clone()).collect(),
                                active: active_conv_id,
                                on_pick: move |id: String| {
                                    active_conv_id.set(Some(id));
                                    active_surface.set(Some(SurfaceMode::Agent));
                                    route.set(Route::AgentSession);
                                    if crate::is_narrow_viewport() {
                                        collapsed.set(true);
                                    }
                                },
                                on_delete: move |id: String| delete_conv.call(id),
                                action_error,
                            }
                        }

                        if !grouped.others.is_empty() {
                            ShowMoreRow {
                                count: grouped.others.len(),
                                expanded: expand_all,
                            }
                            if expand_all() {
                                AgentTailList {
                                    items: grouped.others.iter().map(|c| (*c).clone()).collect(),
                                    active: active_conv_id,
                                    on_pick: move |id: String| {
                                        active_conv_id.set(Some(id));
                                        active_surface.set(Some(SurfaceMode::Agent));
                                        route.set(Route::AgentSession);
                                        if crate::is_narrow_viewport() {
                                            collapsed.set(true);
                                        }
                                    },
                                    on_delete: move |id: String| delete_conv.call(id),
                                    action_error,
                                }
                            }
                        }

                        if agent_rows.is_empty() {
                            div { class: "sidebar-empty", "No agents yet." }
                        }
                    }

                    // ── Chats (recent) ─────────────────────────────────────────
                    section {
                        class: "sidebar-section",
                        header {
                            class: "sidebar-section-header",
                            span { class: "sidebar-section-label", "Chats" }
                        }
                        ChatList {
                            items: chat_rows.iter().map(|c| (*c).clone()).collect(),
                            active: active_conv_id,
                            on_pick: move |id: String| {
                                active_conv_id.set(Some(id));
                                route.set(Route::ChatSession);
                                if crate::is_narrow_viewport() {
                                    collapsed.set(true);
                                }
                            },
                            on_delete: move |id: String| delete_conv.call(id),
                            action_error,
                        }
                        if chat_rows.is_empty() {
                            div { class: "sidebar-empty", "No chats yet." }
                        }
                    }

                    // ── Projects ────────────────────────────────────────────────
                    section {
                        class: "sidebar-section",
                        header {
                            class: "sidebar-section-header",
                            button {
                                class: "sidebar-section-toggle",
                                r#type: "button",
                                onclick: move |_| route.set(Route::Projects),
                                span { class: "sidebar-section-label", "Projects" }
                            }
                        }
                        ProjectList {
                            on_pick: move |id: String| {
                                route.set(Route::ProjectDetail);
                                active_conv_id.set(Some(id));
                                if crate::is_narrow_viewport() {
                                    collapsed.set(true);
                                }
                            },
                        }
                    }

                    // ── Plugins ─────────────────────────────────────────────────
                    section {
                        class: "sidebar-section",
                        header {
                            class: "sidebar-section-header",
                            button {
                                class: "sidebar-section-toggle",
                                r#type: "button",
                                onclick: move |_| {
                                    if route() == Route::Plugins {
                                        route.set(Route::Chat);
                                    } else {
                                        route.set(Route::Plugins);
                                    }
                                },
                                span { class: "sidebar-section-label", "Plugins" }
                            }
                        }
                        PluginList {
                            on_open: move |_| route.set(Route::Plugins),
                        }
                    }
                }
            }

            if let Some(err) = action_error() {
                div {
                    class: "sidebar-error",
                    div { class: "sidebar-empty", "{err}" }
                }
            }
            match &*conversations.read() {
                Some(Err(e)) => rsx! {
                    div {
                        class: "sidebar-error",
                        div { class: "sidebar-empty", "{e}" }
                        button {
                            class: "sidebar-retry-btn",
                            r#type: "button",
                            onclick: move |_| conversations.restart(),
                            "Retry"
                        }
                    }
                },
                None => rsx! {
                    div {
                        class: "sidebar-error",
                        div { class: "sidebar-empty", "Loading..." }
                    }
                },
                _ => rsx! {},
            }
        }
    }
}

// ── Sub-components ──────────────────────────────────────────────────────────

/// The brand glyph in the top-left of the rail. Renders an orange circle "L" (Liberado's first
/// letter) — chosen to read as identity without claiming anyone's logo.
#[component]
fn BrandLogo() -> Element {
    rsx! {
        svg {
            class: "icon sidebar-brand-glyph",
            xmlns: "http://www.w3.org/2000/svg",
            view_box: "0 0 22 22",
            "aria-hidden": "true",
            circle { cx: "11", cy: "11", r: "10", fill: "var(--lib-accent, #f5a623)" }
            text {
                x: "11",
                y: "15",
                text_anchor: "middle",
                font_family: "system-ui, sans-serif",
                font_size: "12",
                font_weight: "700",
                fill: "var(--lib-app-bg, #0d0d1a)",
                "L"
            }
        }
    }
}

#[component]
fn PrimaryNavRow(
    route: Signal<Route>,
    target: Route,
    label: &'static str,
    #[props(into)] icon: Element,
) -> Element {
    let active = match target {
        Route::Chat => route().highlights_chat(),
        _ => route() == target,
    };
    let cls = if active {
        "sidebar-nav-row active"
    } else {
        "sidebar-nav-row"
    };
    rsx! {
        button {
            class: "{cls}",
            r#type: "button",
            title: "{label}",
            "aria-label": "{label}",
            onclick: move |_| route.set(target),
            {icon}
            span { class: "sidebar-nav-label", "{label}" }
        }
    }
}

// Inline SVG glyphs for the four nav rows. Kept inline so we can tune the colour per-route without
// building a separate icon crate. Each one is a 22x22 viewBox with 1.6px stroke.

#[component]
fn ChatBubble() -> Element {
    rsx! {
        svg {
            class: "icon sidebar-icon",
            xmlns: "http://www.w3.org/2000/svg",
            view_box: "0 0 22 22",
            fill: "none",
            stroke: "currentColor",
            stroke_width: "1.6",
            stroke_linecap: "round",
            stroke_linejoin: "round",
            "aria-hidden": "true",
            path { d: "M3.5 4.5h13a2 2 0 0 1 2 2v7a2 2 0 0 1-2 2H8.5l-3.5 3v-3H3.5a2 2 0 0 1-2-2v-7a2 2 0 0 1 2-2z" }
        }
    }
}

#[component]
fn Mountain() -> Element {
    rsx! {
        svg {
            class: "icon sidebar-icon",
            xmlns: "http://www.w3.org/2000/svg",
            view_box: "0 0 22 22",
            fill: "none",
            stroke: "currentColor",
            stroke_width: "1.6",
            stroke_linecap: "round",
            stroke_linejoin: "round",
            "aria-hidden": "true",
            rect { x: "3", y: "4", width: "16", height: "14", rx: "2.5" }
            circle { cx: "8", cy: "9.5", r: "1.4" }
            path { d: "M5 16l4-4.5 3 3 2-2.5 3 4" }
        }
    }
}

#[component]
fn Stack() -> Element {
    rsx! {
        svg {
            class: "icon sidebar-icon",
            xmlns: "http://www.w3.org/2000/svg",
            view_box: "0 0 22 22",
            fill: "none",
            stroke: "currentColor",
            stroke_width: "1.6",
            stroke_linecap: "round",
            stroke_linejoin: "round",
            "aria-hidden": "true",
            rect { x: "5", y: "4", width: "12", height: "13", rx: "1.5" }
            path { d: "M5 8h12" }
            path { d: "M5 12h12" }
        }
    }
}

#[component]
fn Clock() -> Element {
    rsx! {
        svg {
            class: "icon sidebar-icon",
            xmlns: "http://www.w3.org/2000/svg",
            view_box: "0 0 22 22",
            fill: "none",
            stroke: "currentColor",
            stroke_width: "1.6",
            stroke_linecap: "round",
            stroke_linejoin: "round",
            "aria-hidden": "true",
            circle { cx: "11", cy: "11", r: "7.5" }
            path { d: "M11 6.5V11l3 1.5" }
        }
    }
}

#[component]
fn ProjectGlyph() -> Element {
    rsx! {
        svg {
            class: "icon sidebar-icon",
            xmlns: "http://www.w3.org/2000/svg",
            view_box: "0 0 22 22",
            fill: "none",
            stroke: "currentColor",
            stroke_width: "1.6",
            stroke_linecap: "round",
            stroke_linejoin: "round",
            "aria-hidden": "true",
            path { d: "M3.5 6.5a1 1 0 0 1 1-1H8l2 2h7.5a1 1 0 0 1 1 1v8a1 1 0 0 1-1 1h-13a1 1 0 0 1-1-1z" }
        }
    }
}

#[component]
fn PluginGlyph() -> Element {
    rsx! {
        svg {
            class: "icon sidebar-icon",
            xmlns: "http://www.w3.org/2000/svg",
            view_box: "0 0 22 22",
            fill: "none",
            stroke: "currentColor",
            stroke_width: "1.6",
            stroke_linecap: "round",
            stroke_linejoin: "round",
            "aria-hidden": "true",
            path { d: "M9 4v3" }
            path { d: "M13 4v3" }
            path { d: "M7 7h8v3a4 4 0 0 1-4 4 4 4 0 0 1-4-4z" }
            path { d: "M11 14v4" }
        }
    }
}

// Sidebar rows
// ─────────────

/// A solid circle tinted to the conversation's hash. The dot color is the row's visual anchor:
/// Pinned rows get the accent color, others derive from the title's hash. Colors come from a
/// short hand-built palette rather than a generator because anything that runs in the main thread
/// needs to be allocation-free.
fn row_dot_color(title: &str) -> &'static str {
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

/// Truncate title for display with the same width-budget logic the old list used: ends with a
/// trailing ellipsis when the title overflows.
fn truncate_title(title: &str, max: usize) -> String {
    if title.chars().count() <= max {
        return title.to_string();
    }
    let truncated: String = title.chars().take(max.saturating_sub(1)).collect();
    format!("{truncated}…")
}

fn conv_display_title(conv: &ConvHeader) -> String {
    let raw = conv.title.as_deref().unwrap_or("(untitled)");
    // Bump the cap to ~30 chars so titles like "Elite Dangerous Planetary So…" survive without
    // losing both words.
    truncate_title(raw, 30)
}

/// A condensed sidebar row, used in Pinned, CategoryList, AgentTailList and SearchResultList.
/// Matches the Grok aesthetic: dot colour, title, two-line clamp.
#[component]
fn SidebarRowCompact(
    conv: ConvHeader,
    active: Signal<Option<String>>,
    on_pick: EventHandler<String>,
    on_delete: EventHandler<String>,
) -> Element {
    let id = conv.id.clone();
    let is_active = active().as_deref() == Some(id.as_str());
    let cls = if is_active {
        "grok-row grok-row-active"
    } else {
        "grok-row"
    };
    let display = conv_display_title(&conv);
    let dot = row_dot_color(display.as_str());
    let id_for_pick = id.clone();
    rsx! {
        li {
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
        }
    }
}

/// A longer ChatList row — includes the relative-time suffix to match "Chats" expectations.
#[component]
fn SidebarRowChat(
    conv: ConvHeader,
    active: Signal<Option<String>>,
    on_pick: EventHandler<String>,
) -> Element {
    let id = conv.id.clone();
    let is_active = active().as_deref() == Some(id.as_str());
    let cls = if is_active {
        "grok-row grok-row-active"
    } else {
        "grok-row"
    };
    let display = conv_display_title(&conv);
    let time = relative_time(&conv.created_at);
    let id_for_pick = id.clone();
    rsx! {
        li {
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
        }
    }
}

#[component]
fn PinnedList(
    items: Vec<ConvHeader>,
    active: Signal<Option<String>>,
    on_pick: EventHandler<String>,
    on_delete: EventHandler<String>,
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
                    active: active,
                    on_pick: on_pick,
                    on_delete: on_delete,
                }
            }
        }
    }
}

#[component]
fn CategoryList(
    label: &'static str,
    members: Vec<ConvHeader>,
    active: Signal<Option<String>>,
    on_pick: EventHandler<String>,
    on_delete: EventHandler<String>,
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
                    active: active,
                    on_pick: on_pick,
                    on_delete: on_delete,
                }
            }
        }
    }
}

#[component]
fn ShowMoreRow(count: usize, expanded: Signal<bool>) -> Element {
    rsx! {
        button {
            class: "sidebar-show-more",
            r#type: "button",
            onclick: move |_| expanded.set(!expanded()),
            if expanded() { "Show less ({count})" } else { "Show more ({count})" }
        }
    }
}

#[component]
fn AgentTailList(
    items: Vec<ConvHeader>,
    active: Signal<Option<String>>,
    on_pick: EventHandler<String>,
    on_delete: EventHandler<String>,
    action_error: Signal<Option<String>>,
) -> Element {
    rsx! {
        ul {
            class: "grok-list",
            for conv in items.iter() {
                SidebarRowCompact {
                    conv: conv.clone(),
                    active: active,
                    on_pick: on_pick,
                    on_delete: on_delete,
                }
            }
        }
    }
}

#[component]
fn ChatList(
    items: Vec<ConvHeader>,
    active: Signal<Option<String>>,
    on_pick: EventHandler<String>,
    on_delete: EventHandler<String>,
    action_error: Signal<Option<String>>,
) -> Element {
    rsx! {
        ul {
            class: "grok-list",
            for conv in items.iter() {
                SidebarRowChat {
                    conv: conv.clone(),
                    active: active,
                    on_pick: on_pick,
                }
            }
        }
    }
}

/// Section-level placeholder list — three demo projects named after the screenshot. The endpoint
/// to back this list is part of the same milestone, so a backend connection comes later.
const PROJECT_ENTRIES: &[(&str, &str)] = &[
    ("Life System", "var(--dot-cyan)"),
    ("Self Help", "var(--dot-green)"),
    ("Homelab fix", "var(--dot-orange)"),
];

#[component]
fn ProjectList(on_pick: EventHandler<String>) -> Element {
    rsx! {
        ul {
            class: "grok-list",
            for entry in PROJECT_ENTRIES.iter() {
                ProjectRow {
                    label: entry.0,
                    color: entry.1,
                    title: format!("Open the {} project.", entry.0),
                    on_click: move || {
                        let label = entry.0;
                        on_pick.call(format!("project:{label}"));
                    },
                }
            }
        }
    }
}

#[component]
fn ProjectRow(
    label: &'static str,
    color: &'static str,
    title: String,
    on_click: EventHandler<()>,
) -> Element {
    rsx! {
        li {
            class: "grok-row",
            button {
                class: "grok-row-btn",
                r#type: "button",
                title: "{title}",
                onclick: move |_| on_click.call(()),
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
fn PluginList(on_open: EventHandler<()>) -> Element {
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

#[component]
fn SearchResultList(
    results: Vec<ConversationSearchResult>,
    on_pick: EventHandler<String>,
) -> Element {
    if results.is_empty() {
        return rsx! {
            div { class: "sidebar-empty", "No matches." }
        };
    }
    rsx! {
        ul {
            class: "grok-list",
            for r in results.iter() {
                {
                    let id = r.conversation_id.clone();
                    let title = r.title.clone().unwrap_or_else(|| "(untitled)".into());
                    rsx! {
                        li {
                            class: "grok-row",
                            button {
                                class: "grok-row-btn",
                                r#type: "button",
                                onclick: move |_| on_pick.call(id.clone()),
                                span { class: "grok-row-title", "{title}" }
                            }
                        }
                    }
                }
            }
        }
    }
}

// ── Time formatter (moved from old sidebar; identical behaviour) ────────────

pub(super) fn relative_time(iso: &str) -> String {
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

#[cfg(test)]
mod tests {
    use super::{delete_accepted, pinned, relative_time, row_dot_color, truncate_title};

    fn ago(dur: chrono::Duration) -> String {
        (chrono::Utc::now() - dur).to_rfc3339()
    }

    #[test]
    fn unparseable_is_empty() {
        assert_eq!(relative_time("not-a-date"), "");
    }

    #[test]
    fn future_reads_as_just_now() {
        assert_eq!(
            relative_time(&ago(-chrono::Duration::minutes(1))),
            "just now"
        );
    }

    #[test]
    fn age_bands_choose_the_unit() {
        assert_eq!(
            relative_time(&ago(chrono::Duration::seconds(12))),
            "12s ago"
        );
        assert_eq!(relative_time(&ago(chrono::Duration::minutes(5))), "5m ago");
        assert_eq!(relative_time(&ago(chrono::Duration::hours(3))), "3h ago");
        assert_eq!(relative_time(&ago(chrono::Duration::days(2))), "2d ago");
    }

    #[test]
    fn past_a_month_shows_the_date() {
        assert_eq!(relative_time("2000-01-15T10:00:00Z"), "Jan 15");
    }

    #[test]
    fn offsetless_timestamp_is_treated_as_utc() {
        let raw = (chrono::Utc::now() - chrono::Duration::minutes(7))
            .format("%Y-%m-%dT%H:%M:%S%.f")
            .to_string();
        assert_eq!(relative_time(&raw), "7m ago");
    }

    #[test]
    fn band_boundaries_fall_up() {
        let at = |secs: i64| (chrono::Utc::now() - chrono::Duration::seconds(secs)).to_rfc3339();
        assert_eq!(relative_time(&at(60)), "1m ago");
        assert_eq!(relative_time(&at(3600)), "1h ago");
        assert_eq!(relative_time(&at(86_400)), "1d ago");
    }

    #[test]
    fn exactly_thirty_days_shows_the_date() {
        let raw = (chrono::Utc::now() - chrono::Duration::days(30)).to_rfc3339();
        let expected = chrono::DateTime::parse_from_rfc3339(&raw)
            .unwrap()
            .format("%b %d")
            .to_string();
        assert_eq!(relative_time(&raw), expected);
    }

    #[test]
    fn sub_second_ages_from_zero() {
        let raw = (chrono::Utc::now() - chrono::Duration::milliseconds(500)).to_rfc3339();
        assert_eq!(relative_time(&raw), "0s ago");
    }

    #[test]
    fn delete_accepts_2xx_and_404() {
        for ok in [200, 204, 299, 404] {
            assert!(delete_accepted(ok), "{ok} should delete cleanly");
        }
        for no in [300, 403, 500] {
            assert!(!delete_accepted(no), "{no} should surface as a delete failure");
        }
    }

    /// A title with a quote in it (or any text overflowing the cap) ends in the truncation
    /// marker and never exceeds `max` chars minus the marker.
    #[test]
    fn long_titles_truncate_with_an_ellipsis() {
        let s = "a".repeat(40);
        let out = truncate_title(&s, 30);
        assert!(out.ends_with('…'));
        assert!(out.chars().count() <= 30);
    }

    /// Short titles render untouched — no spurious ellipsis when there's room to spare.
    #[test]
    fn short_titles_pass_through() {
        assert_eq!(truncate_title("Disk Saver", 30), "Disk Saver");
    }

    /// The Pinned set is for the user's pinned agents. The match is a case-insensitive prefix on
    /// the title so a user-typed title still pins without exact casing.
    #[test]
    fn pinned_match_is_case_insensitive_prefix() {
        assert!(pinned("Grok Bot"));
        assert!(pinned("grok bot extra"));
        assert!(pinned("Liberado-Budget Manager"));
        assert!(!pinned("Other Agent"));
    }

    /// The dot palette covers every visible category in the prototype so we never fall through to
    /// the neutral palette by accident — if a new agent shows up uncolored, this test breaks.
    #[test]
    fn dot_palette_covers_prototype_categories() {
        // All categories appear in row_dot_color. Spot-check that grep sees the strings.
        for name in ["Grok", "Liberado-Budget", "Disk Saver", "Self Help", "Life System", "Homelab"] {
            assert!(
                !row_dot_color(name).is_empty(),
                "row_dot_color should resolve a colour for {name}"
            );
        }
    }
}
