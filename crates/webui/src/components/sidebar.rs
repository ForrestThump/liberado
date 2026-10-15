//! Left sidebar rail.
//!
//! Grok-style vertical layout with a permanent icon rail when collapsed and a full-width
//! panel when expanded. The rail exposes the same brand/logo as the full sidebar so the user
//! always has something to click to bring it back.
//!
//! Sections rendered when expanded: Chat / Library / Automations primary nav, Agents (Pinned +
//! labelled categories + Show more), Chats (recent), Projects (with `+` opening a modal), and
//! Plugins. Imagine is intentionally absent — the file `components/imagine.rs` and `Route::Imagine`
//! survive so the surface can be re-introduced later without re-shaping the rail.

use dioxus::prelude::*;

use chat_client_contract::{ConvHeader, ConversationSearchResponse, ConversationSearchResult, SurfaceMode};

use crate::components::projects_dialog::ProjectsDialog;
use crate::icons::{IconChevronDown, IconChevronRight, IconSearch};

// ── Routes ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    Chat,
    Library,
    Automations,
    Agents,
    Projects,
    Plugins,
    AgentSession,
    ChatSession,
    ProjectDetail,
    /// Reserved for a future Imagine surface — kept so the route enum doesn't drift, but the
    /// sidebar never renders a primary-nav row that targets it.
    #[allow(dead_code)]
    Imagine,
}

impl Route {
    pub(crate) fn is_chat_view(self) -> bool {
        matches!(
            self,
            Route::Chat | Route::ChatSession | Route::AgentSession | Route::ProjectDetail
        )
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

fn pinned(title: &str) -> bool {
    PINNED_PREFIXES
        .iter()
        .any(|prefix| title.to_ascii_lowercase().starts_with(&prefix.to_ascii_lowercase()))
}

const CATEGORY_ORDER: &[&str] = &["Liberado Development", "Liberado Finance"];

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

// ── Sidebar component ──────────────────────────────────────────────────────

#[component]
pub fn Sidebar(
    api_base: String,
    route: Signal<Route>,
    active_conv_id: Signal<Option<String>>,
    collapsed: Signal<bool>,
    new_chat_nonce: Signal<u64>,
    active_surface: Signal<Option<SurfaceMode>>,
    list_epoch: Signal<u64>,
    /// Bumped by `App` whenever the user picks a different theme, so this component can keep
    /// the rail's palette swatch in sync with the active paint.
    theme_epoch: Signal<u64>,
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
    #[cfg_attr(not(target_arch = "wasm32"), allow(unused_mut))]
    let mut action_error = use_signal(|| None::<String>);
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

    let headers: Vec<ConvHeader> = conversations
        .read()
        .as_ref()
        .and_then(|r: &Result<Vec<ConvHeader>, String>| r.as_ref().ok())
        .cloned()
        .unwrap_or_default();
    let chat_rows: Vec<&ConvHeader> = headers
        .iter()
        .filter(|c| c.surface_mode == SurfaceMode::Chat)
        .collect();
    let agent_rows: Vec<&ConvHeader> = headers
        .iter()
        .filter(|c| c.surface_mode == SurfaceMode::Agent)
        .collect();
    let grouped = group_agents(&agent_rows);

    // Projects modal. Lives in App-level state would be tidier, but the dialog has no other
    // call sites and shortcutting the state is cheaper than threading through another prop.
    let mut projects_dialog_open = use_signal(bool::default);

    if collapsed() {
        // Icon rail — vertical column of buttons that bring the rail back / navigate without
        // expanding the panel. The brand at top expands the sidebar; the rest mirror the primary
        // nav so the user can hop routes without needing 260px of canvas.
        return rsx! {
            aside { class: "sidebar sidebar-collapsed",
                button {
                    class: "sidebar-rail-brand",
                    r#type: "button",
                    title: "Expand sidebar",
                    "aria-label": "Expand sidebar",
                    onclick: move |_| collapsed.set(false),
                    BrandLogo {}
                }
                button {
                    class: if route() == Route::Chat || route().is_chat_view() {
                        "sidebar-rail-btn active"
                    } else {
                        "sidebar-rail-btn"
                    },
                    r#type: "button",
                    title: "Chat",
                    "aria-label": "Chat",
                    onclick: move |_| route.set(Route::Chat),
                    ChatBubble {}
                }
                button {
                    class: "sidebar-rail-btn",
                    r#type: "button",
                    title: "Search",
                    "aria-label": "Search",
                    onclick: move |_| {
                        collapsed.set(false);
                        search_open.set(true);
                    },
                    IconSearch {}
                }
                button {
                    class: if route() == Route::Library { "sidebar-rail-btn active" } else { "sidebar-rail-btn" },
                    r#type: "button",
                    title: "Library",
                    "aria-label": "Library",
                    onclick: move |_| route.set(Route::Library),
                    LibraryGlyph {}
                }
                button {
                    class: if route() == Route::Automations { "sidebar-rail-btn active" } else { "sidebar-rail-btn" },
                    r#type: "button",
                    title: "Automations",
                    "aria-label": "Automations",
                    onclick: move |_| route.set(Route::Automations),
                    ClockGlyph {}
                }
                button {
                    class: if route() == Route::Agents || route() == Route::AgentSession {
                        "sidebar-rail-btn active"
                    } else {
                        "sidebar-rail-btn"
                    },
                    r#type: "button",
                    title: "Agents",
                    "aria-label": "Agents",
                    onclick: move |_| route.set(Route::Agents),
                    AgentGlyph {}
                }
                button {
                    class: if route() == Route::Plugins { "sidebar-rail-btn active" } else { "sidebar-rail-btn" },
                    r#type: "button",
                    title: "Plugins",
                    "aria-label": "Plugins",
                    onclick: move |_| route.set(Route::Plugins),
                    PluginGlyph {}
                }
                div { class: "sidebar-rail-spacer" }
                ThemeSwatch { theme_epoch: theme_epoch }
            }
        };
    }

    rsx! {
        aside { class: "sidebar",
            div {
                class: "sidebar-brand",
                button {
                    class: "sidebar-brand-btn",
                    r#type: "button",
                    title: "Liberado home",
                    "aria-label": "Liberado home",
                    onclick: move |_| route.set(Route::Chat),
                    BrandLogo {}
                }
                div { class: "sidebar-brand-spacer" }
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
                    class: "sidebar-icon-btn collapse-btn",
                    r#type: "button",
                    title: "Collapse sidebar",
                    "aria-label": "Collapse sidebar",
                    onclick: move |_| collapsed.set(true),
                    CollapseGlyph {}
                }
            }

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

            nav {
                class: "sidebar-nav",
                PrimaryNavRow { route: route, target: Route::Chat, label: "Chat", icon: rsx! { ChatBubble {} } }
                PrimaryNavRow { route: route, target: Route::Library, label: "Library", icon: rsx! { LibraryGlyph {} } }
                PrimaryNavRow { route: route, target: Route::Automations, label: "Automations", icon: rsx! { ClockGlyph {} } }
            }

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
                                onclick: move |_: Event<MouseData>| { let _ = (); },
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
                            // The + opens the Create Project dialog rather than routing into a
                            // placeholder page. The route toggles just so the header row is the
                            // active piece of chrome even when the modal is open.
                            button {
                                class: "sidebar-section-add",
                                r#type: "button",
                                title: "Create project",
                                "aria-label": "Create project",
                                onclick: move |_| {
                                    projects_dialog_open.set(true);
                                },
                                "+"
                            }
                        }
                        ProjectList { route: route }
                    }

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

                    ThemeSwatch { theme_epoch: theme_epoch }
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

        if projects_dialog_open() {
            ProjectsDialog {
                on_cancel: move |_| projects_dialog_open.set(false),
                on_create: move |_name: String| projects_dialog_open.set(false),
            }
        }
    }
}

// ── Sub-components ──────────────────────────────────────────────────────────

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
                fill: "var(--lib-app-bg, #0a0a0a)",
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
        Route::Chat => route().is_chat_view(),
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

// Glyph set — kept inline so the colour swatches and weights match the rest of the rail.

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
fn LibraryGlyph() -> Element {
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
fn ClockGlyph() -> Element {
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
fn AgentGlyph() -> Element {
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
            circle { cx: "11", cy: "8", r: "3.5" }
            path { d: "M4 18a7 7 0 0 1 14 0" }
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

/// Chevron pointing left — the user-facing signal that the action will collapse the rail back
/// to the icon-only state.
#[component]
fn CollapseGlyph() -> Element {
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
            // Arrow that opens to the right when paired with the brand glyph: a single chevron at
            // the very left edge of the brand row, identical to the rotate-by-180 mirror image of
            // the icon in the collapsed-rail state.
            path { d: "M14.5 5l-6 6 6 6" }
        }
    }
}

#[component]
fn PaletteGlyph() -> Element {
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
            path { d: "M11 3a8 8 0 0 0-1 15.9c.5.1.9-.4.7-.9-.2-.5-.4-1.1-.4-1.6 0-1.1.9-1.4 1.6-1.4H13a3 3 0 0 0 3-3.3 8 8 0 0 0-5-8.7z" }
            circle { cx: "6.5", cy: "11", r: "1" }
            circle { cx: "9", cy: "6.5", r: "1" }
            circle { cx: "14.5", cy: "7.5", r: "1" }
            circle { cx: "17", cy: "11.5", r: "1" }
        }
    }
}

// ── Theme cycling ──────────────────────────────────────────────────────────

/// A compact swatch that lists the registered themes and applies the picked one through the
/// `crate::theme` helpers. Persistence rides on `save_theme_name` — the same backend the `/theme`
/// slash command already uses, so the choice survives across server restarts.
#[component]
fn ThemeSwatch(theme_epoch: Signal<u64>) -> Element {
    let mut current = use_signal(crate::theme::saved_theme_name);
    // React to external bumps (mostly a safety net — the picker itself mutates `current` on
    // click and a parent re-render would otherwise not re-sync the dropdown).
    use_effect(move || {
        let _ = theme_epoch();
        current.set(crate::theme::saved_theme_name());
    });
    let names = use_signal(crate::theme::builtin_theme_names);

    let mut open = use_signal(bool::default);

    rsx! {
        div {
            class: "theme-swatch-shell",
            button {
                class: if open() { "theme-swatch-toggle open" } else { "theme-swatch-toggle" },
                r#type: "button",
                title: "Theme",
                "aria-label": "Theme",
                onclick: move |_| open.set(!open()),
                PaletteGlyph {}
            }
            if open() {
                div {
                    class: "theme-swatch-pop",
                    for name in names.read().iter() {
                        {
                            let name_str = name.clone();
                            let active = *name == current();
                            let cls = if active { "theme-swatch-row active" } else { "theme-swatch-row" };
                            let dot = theme_dot(name.as_str());
                            rsx! {
                                button {
                                    class: "{cls}",
                                    r#type: "button",
                                    onclick: move |_| {
                                        current.set(name_str.clone());
                                        crate::theme::save_theme_name(name_str.as_str());
                                        open.set(false);
                                        // Trigger the parent to re-render the CSS block.
                                        theme_epoch += 1;
                                    },
                                    span {
                                        class: "theme-swatch-dot",
                                        style: "background: {dot};",
                                        "aria-hidden": "true"
                                    }
                                    span { class: "theme-swatch-name", "{name}" }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

fn theme_dot(name: &str) -> &'static str {
    match name {
        "dark" => "#00ffff",
        "light" => "#c0c0c0",
        "nord" => "#5e81ac",
        "sunset" => "#f5a623",
        "forest" => "#6dba6d",
        "plum" => "#ce93d8",
        "mono" => "#d4a85a",
        _ => "#888888",
    }
}

// ── Sidebar rows ───────────────────────────────────────────────────────────

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

fn truncate_title(title: &str, max: usize) -> String {
    if title.chars().count() <= max {
        return title.to_string();
    }
    let truncated: String = title.chars().take(max.saturating_sub(1)).collect();
    format!("{truncated}…")
}

fn conv_display_title(conv: &ConvHeader) -> String {
    let raw = conv.title.as_deref().unwrap_or("(untitled)");
    truncate_title(raw, 30)
}

#[component]
fn SidebarRowCompact(
    conv: ConvHeader,
    active: Signal<Option<String>>,
    on_pick: EventHandler<String>,
    on_delete: EventHandler<String>,
) -> Element {
    let id = conv.id.clone();
    let is_active = active().as_deref() == Some(id.as_str());
    let cls = if is_active { "grok-row grok-row-active" } else { "grok-row" };
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

#[component]
fn SidebarRowChat(
    conv: ConvHeader,
    active: Signal<Option<String>>,
    on_pick: EventHandler<String>,
) -> Element {
    let id = conv.id.clone();
    let is_active = active().as_deref() == Some(id.as_str());
    let cls = if is_active { "grok-row grok-row-active" } else { "grok-row" };
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
            if expanded() { IconChevronDown {} } else { IconChevronRight {} }
            span {
                if expanded() { "Show less ({count})" } else { "Show more ({count})" }
            }
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

/// Projects are placeholders for the prototype — three demo entries with the colour the user
/// saw in the original screenshot. The header `+` opens a modal in the parent; rows here are
/// status rows, not links.
const PROJECT_ENTRIES: &[(&str, &str)] = &[
    ("Life System", "var(--dot-cyan)"),
    ("Self Help", "var(--dot-green)"),
    ("Homelab fix", "var(--dot-orange)"),
];

#[component]
fn ProjectList(route: Signal<Route>) -> Element {
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

// ── Time formatter ─────────────────────────────────────────────────────────

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
        assert_eq!(relative_time(&ago(chrono::Duration::seconds(12))), "12s ago");
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

    #[test]
    fn long_titles_truncate_with_an_ellipsis() {
        let s = "a".repeat(40);
        let out = truncate_title(&s, 30);
        assert!(out.ends_with('…'));
        assert!(out.chars().count() <= 30);
    }

    #[test]
    fn short_titles_pass_through() {
        assert_eq!(truncate_title("Disk Saver", 30), "Disk Saver");
    }

    #[test]
    fn pinned_match_is_case_insensitive_prefix() {
        assert!(pinned("Grok Bot"));
        assert!(pinned("grok bot extra"));
        assert!(pinned("Liberado-Budget Manager"));
        assert!(!pinned("Other Agent"));
    }

    #[test]
    fn dot_palette_covers_prototype_categories() {
        for name in ["Grok", "Liberado-Budget", "Disk Saver", "Self Help", "Life System", "Homelab"] {
            assert!(
                !row_dot_color(name).is_empty(),
                "row_dot_color should resolve a colour for {name}"
            );
        }
    }
}
