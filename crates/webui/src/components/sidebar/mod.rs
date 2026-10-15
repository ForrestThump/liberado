//! Left sidebar rail.
//!
//! Grok-style vertical layout with an icon rail when collapsed and a full panel when expanded.
//! On a phone the collapsed rail is hidden (the header menu opens the panel as an overlay).
//! On a desk the rail's brand mark brings the panel back. On a phone the header menu does.
//!
//! Sections rendered when expanded: Chat / Library / Automations primary nav, Agents (Pinned +
//! labelled categories + Show more), Chats (recent), Projects (with `+` opening a modal), and
//! Plugins. Imagine is intentionally absent — the file `components/imagine.rs` and `Route::Imagine`
//! survive so the surface can be re-introduced later without re-shaping the rail.

use dioxus::prelude::*;

use chat_client_contract::{
    ConvHeader, ConversationSearchResponse, ConversationSearchResult, SurfaceMode,
};

use crate::components::project_data::close_phone_drawer;
use crate::components::projects_dialog::ProjectsDialog;
use crate::components::sidebar_agent_creator::CreatorError;
#[cfg(target_arch = "wasm32")]
use crate::components::sidebar_agent_creator::open_agent_creator_session;
use crate::components::sidebar_shelf::{Shelf, search_results_on_shelf, surface_for_id};
use crate::icons::IconSearch;

mod agents;
mod projects;
mod rail;
mod row_menu;

use self::agents::{
    AgentTailList, CategoryList, ChatList, PinnedList, SearchResultList, ShowMoreRow, group_agents,
};
use self::projects::{PluginList, ProjectList};
use self::rail::{
    AgentGlyph, BrandLogo, ChatBubble, ClockGlyph, CollapseGlyph, ExpandGlyph, LibraryGlyph,
    PluginGlyph, PrimaryNavRow, ThemeSwatch,
};

pub(in crate::components) use self::agents::relative_time;

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

/// Headers from `GET /api/conversations`, plus the built-in creator ids.
///
/// `agent_creator` is on the wire and is not a field of [`ConvHeader`]. The
/// sidebar needs it so Delete can stay off for that one row.
#[derive(Clone, Debug, Default)]
pub(super) struct ConversationPage {
    pub(super) headers: Vec<ConvHeader>,
    pub(super) creator_ids: Vec<String>,
}

pub(super) fn page_from_list(value: &serde_json::Value) -> Result<ConversationPage, String> {
    let items = value
        .as_array()
        .ok_or_else(|| "Bad response: expected a list".to_string())?;
    let mut headers = Vec::with_capacity(items.len());
    let mut creator_ids = Vec::new();
    for item in items {
        let header: ConvHeader =
            serde_json::from_value(item.clone()).map_err(|e| format!("Bad response: {e}"))?;
        if item
            .get("agent_creator")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
        {
            creator_ids.push(header.id.clone());
        }
        headers.push(header);
    }
    Ok(ConversationPage {
        headers,
        creator_ids,
    })
}

fn loaded_page(loaded: Option<&Result<ConversationPage, String>>) -> ConversationPage {
    loaded
        .and_then(|result| result.as_ref().ok())
        .cloned()
        .unwrap_or_default()
}

async fn fetch_conversations(api_base: String) -> Result<ConversationPage, String> {
    let url = format!("{api_base}/api/conversations");
    let resp = reqwest::get(&url)
        .await
        .map_err(|e| format!("Failed to reach daemon: {e}"))?;
    let body: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("Bad response: {e}"))?;
    page_from_list(&body)
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

fn rename_accepted(status: u16) -> bool {
    (200..300).contains(&status)
}

/// Body of `PATCH /api/conversations/{id}`.
pub(super) fn rename_body(title: &str) -> serde_json::Value {
    serde_json::json!({ "title": title })
}

/// The open conversation was the one removed, so the canvas must become a fresh chat.
pub(super) fn deleted_the_open_conversation(open_id: Option<&str>, deleted_id: &str) -> bool {
    open_id == Some(deleted_id)
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

/// `PATCH /api/conversations/{id}` — update only the conversation's display title.
async fn rename_conversation(api_base: &str, id: &str, title: &str) -> Result<(), String> {
    let url = format!("{api_base}/api/conversations/{id}");
    let resp = reqwest::Client::new()
        .patch(&url)
        .json(&rename_body(title))
        .send()
        .await
        .map_err(|e| format!("Failed to reach daemon: {e}"))?;
    if rename_accepted(resp.status().as_u16()) {
        Ok(())
    } else {
        Err(format!("Rename failed: HTTP {}", resp.status().as_u16()))
    }
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
    /// Coding project selected for this window. Rows come from `GET /api/projects`.
    active_project: Signal<Option<String>>,
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
    // Chats | Agents. Switching shelves only refilters the list. The categorised
    // agent rows and the chat rows stay the new rail's content for each side.
    let mut shelf = use_signal(Shelf::default);
    let mut new_chat_nonce = new_chat_nonce;
    // Agents + failure. Separate from row `action_error` so a delete error stays on the row.
    #[cfg_attr(not(target_arch = "wasm32"), allow(unused_mut))]
    let mut create_error = use_signal(Option::<String>::default);
    // Distinguishes each Agents + click. The daemon mints a new conversation per request;
    // the stamp keeps the shelf titles from colliding.
    #[cfg_attr(not(target_arch = "wasm32"), allow(unused_mut))]
    let mut creator_seq = use_signal(|| 0u64);
    let expand_all = use_signal(bool::default);
    #[cfg_attr(not(target_arch = "wasm32"), allow(unused_mut))]
    let mut action_error = use_signal(|| None::<String>);
    // One open action sheet. Opening a row closes the sheet on any other row.
    // `.set` from the delete and rename callbacks is wasm-only.
    #[cfg_attr(not(target_arch = "wasm32"), allow(unused_mut))]
    let mut menu_for = use_signal(|| None::<String>);
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
                        menu_for.set(None);
                        action_error.set(None);
                        if deleted_the_open_conversation(active_conv_id.read().as_deref(), &id) {
                            active_conv_id.set(None);
                            active_surface.set(Some(SurfaceMode::Chat));
                            route.set(Route::Chat);
                            new_chat_nonce += 1;
                            close_phone_drawer(collapsed);
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

    let rename_conv = {
        let base = api_base.clone();
        use_callback(move |(id, title): (String, String)| {
            let base = base.clone();
            #[cfg(target_arch = "wasm32")]
            wasm_bindgen_futures::spawn_local(async move {
                match rename_conversation(&base, &id, &title).await {
                    Ok(()) => {
                        menu_for.set(None);
                        action_error.set(None);
                        conversations.restart();
                        search_results.restart();
                    }
                    Err(e) => action_error.set(Some(e)),
                }
            });
            #[cfg(not(target_arch = "wasm32"))]
            {
                let _ = (base, id, title);
            }
        })
    };

    let page = loaded_page(conversations.read().as_ref());
    let headers = page.headers;
    let creator_ids = page.creator_ids;
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
    // Cloned before the move closures below, which take `api_base` for their own requests.
    let projects_api = api_base.clone();

    if collapsed() {
        // Icon rail — vertical column of buttons that bring the rail back / navigate without
        // expanding the panel. The top button is an expand-sidebar chevron. The rest mirror the
        // primary nav so the user can hop routes without needing 260px of canvas.
        return rsx! {
            aside { id: "sidebar", class: "sidebar sidebar-collapsed",
                button {
                    class: "sidebar-rail-brand",
                    r#type: "button",
                    title: "Expand sidebar",
                    "aria-label": "Expand sidebar",
                    onclick: move |_| collapsed.set(false),
                    ExpandGlyph {}
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
        aside { id: "sidebar", class: "sidebar",
            div {
                class: "sidebar-brand",
                button {
                    class: "sidebar-brand-btn",
                    r#type: "button",
                    title: "Liberado home",
                    "aria-label": "Liberado home",
                    onclick: move |_| {
                        route.set(Route::Chat);
                        close_phone_drawer(collapsed);
                    },
                    BrandLogo {}
                }
                div { class: "sidebar-brand-spacer" }
                button {
                    class: if search_open() { "sidebar-icon-btn active" } else { "sidebar-icon-btn" },
                    r#type: "button",
                    title: if search_open() { "Close search" } else { shelf().search_button_label() },
                    "aria-label": if search_open() { "Close search" } else { shelf().search_button_label() },
                    "aria-expanded": "{search_open()}",
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
                        placeholder: "{shelf().search_placeholder()}",
                        "aria-label": "{shelf().search_field_label()}",
                        value: "{search_query()}",
                        autofocus: true,
                        oninput: move |e| search_query.set(e.value()),
                    }
                }
            }

            nav {
                class: "sidebar-nav",
                PrimaryNavRow { route: route, collapsed: collapsed, target: Route::Chat, label: "Chat", icon: rsx! { ChatBubble {} } }
                PrimaryNavRow { route: route, collapsed: collapsed, target: Route::Library, label: "Library", icon: rsx! { LibraryGlyph {} } }
                PrimaryNavRow { route: route, collapsed: collapsed, target: Route::Automations, label: "Automations", icon: rsx! { ClockGlyph {} } }
            }

            div {
                class: "sidebar-shelves",
                role: "tablist",
                "aria-label": "Conversation shelves",
                button {
                    class: if shelf() == Shelf::Chats { "sidebar-shelf-btn active" } else { "sidebar-shelf-btn" },
                    role: "tab",
                    r#type: "button",
                    "aria-selected": "{shelf() == Shelf::Chats}",
                    onclick: move |_| shelf.set(Shelf::Chats),
                    "Chats"
                }
                button {
                    class: if shelf() == Shelf::Agents { "sidebar-shelf-btn active" } else { "sidebar-shelf-btn" },
                    role: "tab",
                    r#type: "button",
                    "aria-selected": "{shelf() == Shelf::Agents}",
                    onclick: move |_| shelf.set(Shelf::Agents),
                    "Agents"
                }
            }

            if search_open() && !search_query().trim().is_empty() {
                div {
                    class: "sidebar-list",
                    match &*search_results.read() {
                        Some(Some(Ok(results))) => rsx! {
                            SearchResultList {
                                results: search_results_on_shelf(results, &headers, shelf())
                                    .into_iter()
                                    .cloned()
                                    .collect(),
                                menu_for,
                                action_error,
                                on_delete: move |id: String| delete_conv.call(id),
                                on_rename: move |pair: (String, String)| rename_conv.call(pair),
                                protected_ids: creator_ids.clone(),
                                on_pick: move |id: String| {
                                    let mode = conversations
                                        .read()
                                        .as_ref()
                                        .and_then(|r| r.as_ref().ok())
                                        .and_then(|page| surface_for_id(&page.headers, &id));
                                    active_conv_id.set(Some(id));
                                    match mode {
                                        Some(SurfaceMode::Agent) => {
                                            active_surface.set(Some(SurfaceMode::Agent));
                                            route.set(Route::AgentSession);
                                        }
                                        Some(mode) => {
                                            active_surface.set(Some(mode));
                                            route.set(Route::ChatSession);
                                        }
                                        None => route.set(Route::ChatSession),
                                    }
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

                    if shelf() == Shelf::Agents {
                    section {
                        class: "sidebar-section",
                        header {
                            class: "sidebar-section-header",
                            button {
                                class: "sidebar-section-toggle",
                                r#type: "button",
                                onclick: move |_| {
                                    route.set(Route::Agents);
                                    close_phone_drawer(collapsed);
                                },
                                span { class: "sidebar-section-label", "Agents" }
                            }
                            button {
                                class: "sidebar-section-add",
                                r#type: "button",
                                title: "New agent",
                                "aria-label": "New agent",
                                onclick: move |_: Event<MouseData>| {
                                    creator_seq += 1;
                                    let stamp = creator_seq().to_string();
                                    let base = api_base.clone();
                                    #[cfg(target_arch = "wasm32")]
                                    wasm_bindgen_futures::spawn_local(async move {
                                        match open_agent_creator_session(&base, &stamp).await {
                                            Ok(header) => {
                                                active_conv_id.set(Some(header.id));
                                                active_surface.set(Some(SurfaceMode::Agent));
                                                route.set(Route::AgentSession);
                                                create_error.set(None);
                                                conversations.restart();
                                                if crate::is_narrow_viewport() {
                                                    collapsed.set(true);
                                                }
                                            }
                                            Err(e) => create_error.set(Some(e)),
                                        }
                                    });
                                    #[cfg(not(target_arch = "wasm32"))]
                                    {
                                        let _ = (base, stamp);
                                    }
                                },
                                "+"
                            }
                        }
                        CreatorError { message: create_error() }

                        PinnedList {
                            items: grouped.pinned.iter().map(|c| (*c).clone()).collect(),
                            active: active_conv_id,
                            menu_for,
                            protected_ids: creator_ids.clone(),
                            on_rename: move |pair: (String, String)| rename_conv.call(pair),
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
                                menu_for,
                                protected_ids: creator_ids.clone(),
                                on_rename: move |pair: (String, String)| rename_conv.call(pair),
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
                                    menu_for,
                                    protected_ids: creator_ids.clone(),
                                    on_rename: move |pair: (String, String)| rename_conv.call(pair),
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
                    } else {
                    section {
                        class: "sidebar-section",
                        header {
                            class: "sidebar-section-header",
                            span { class: "sidebar-section-label", "Chats" }
                            button {
                                class: "sidebar-section-add",
                                r#type: "button",
                                title: "New chat",
                                "aria-label": "New chat",
                                onclick: move |_| {
                                    new_chat_nonce += 1;
                                    if active_conv_id.read().is_some() {
                                        active_conv_id.set(None);
                                    }
                                    active_surface.set(Some(SurfaceMode::Chat));
                                    route.set(Route::Chat);
                                    if crate::is_narrow_viewport() {
                                        collapsed.set(true);
                                    }
                                },
                                "+"
                            }
                        }
                        ChatList {
                            items: chat_rows.iter().map(|c| (*c).clone()).collect(),
                            active: active_conv_id,
                            menu_for,
                            on_pick: move |id: String| {
                                active_conv_id.set(Some(id));
                                active_surface.set(Some(SurfaceMode::Chat));
                                route.set(Route::ChatSession);
                                if crate::is_narrow_viewport() {
                                    collapsed.set(true);
                                }
                            },
                            on_delete: move |id: String| delete_conv.call(id),
                            on_rename: move |pair: (String, String)| rename_conv.call(pair),
                            action_error,
                        }
                        if chat_rows.is_empty() {
                            div { class: "sidebar-empty", "No chats yet." }
                        }
                    }
                    }

                    section {
                        class: "sidebar-section",
                        header {
                            class: "sidebar-section-header",
                            button {
                                class: "sidebar-section-toggle",
                                r#type: "button",
                                onclick: move |_| {
                                    route.set(Route::Projects);
                                    close_phone_drawer(collapsed);
                                },
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
                        ProjectList {
                            api_base: projects_api,
                            route,
                            active_project,
                            collapsed,
                        }
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
                                    close_phone_drawer(collapsed);
                                },
                                span { class: "sidebar-section-label", "Plugins" }
                            }
                        }
                        PluginList {
                            collapsed: collapsed,
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

#[cfg(test)]
mod tests;
