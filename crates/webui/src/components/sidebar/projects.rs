//! Project and plugin rows in the sidebar.
//!
//! Projects are the daemon's `GET /api/projects` list. A row selects that project and
//! opens the chat. Plugin entry opens the plugins page.

use dioxus::prelude::*;

use chat_client_contract::ConvHeader;

use super::Route;
use super::rail::{PluginGlyph, ProjectGlyph};
use crate::components::project_data::{
    chats_for_project, close_phone_drawer, collapsed_after_pick, fetch_projects, project_dot,
    queue_project_patch, selected_project_name,
};

#[component]
pub(super) fn ProjectList(
    api_base: String,
    chats: Vec<ConvHeader>,
    mut route: Signal<Route>,
    mut active_project: Signal<Option<String>>,
    active_conv_id: Signal<Option<String>>,
    list_epoch: Signal<u64>,
    mut collapsed: Signal<bool>,
) -> Element {
    let projects = use_resource({
        let base = api_base.clone();
        move || {
            let base = base.clone();
            async move { fetch_projects(&base).await }
        }
    });

    rsx! {
        ul {
            class: "grok-list",
            match projects.read().as_ref() {
                None => rsx! { li { class: "sidebar-empty", "Loading..." } },
                Some(Err(error)) => rsx! { li { class: "sidebar-empty", "{error}" } },
                Some(Ok(rows)) if rows.is_empty() => rsx! {
                    li { class: "sidebar-empty", "No projects are configured." }
                },
                Some(Ok(rows)) => rsx! {
                    for (index, project) in rows.iter().enumerate() {
                        SidebarProject {
                            name: project.name.clone(),
                            dot: project_dot(index),
                            active: active_project().as_deref() == Some(project.name.as_str()),
                            chats: chats_for_project(&chats, &project.name)
                                .into_iter()
                                .cloned()
                                .collect(),
                            api_base: api_base.clone(),
                            route,
                            active_project,
                            active_conv_id,
                            list_epoch,
                            collapsed,
                        }
                    }
                },
            }
        }
    }
}

#[component]
fn SidebarProject(
    name: String,
    dot: &'static str,
    active: bool,
    chats: Vec<ConvHeader>,
    api_base: String,
    mut route: Signal<Route>,
    mut active_project: Signal<Option<String>>,
    mut active_conv_id: Signal<Option<String>>,
    list_epoch: Signal<u64>,
    mut collapsed: Signal<bool>,
) -> Element {
    let row = if active {
        "grok-row grok-row-active"
    } else {
        "grok-row"
    };
    let pick_name = name.clone();
    rsx! {
        li {
            class: "{row}",
            button {
                class: "grok-row-btn",
                r#type: "button",
                onclick: move |_| {
                    let Some(name) = selected_project_name(&pick_name) else {
                        return;
                    };
                    let previous = active_project();
                    active_project.set(Some(name.clone()));
                    route.set(Route::Chat);
                    collapsed.set(collapsed_after_pick(
                        crate::is_narrow_viewport(),
                        collapsed(),
                    ));
                    let Some(id) = active_conv_id() else {
                        return;
                    };
                    queue_project_patch(
                        api_base.clone(),
                        id,
                        Some(name),
                        list_epoch,
                        active_project,
                        previous,
                    );
                },
                span {
                    class: "grok-row-dot",
                    style: "background: {dot};",
                    "aria-hidden": "true"
                }
                ProjectGlyph {}
                span { class: "grok-row-title", "{name}" }
            }
            if !chats.is_empty() {
                ul {
                    class: "grok-list project-chats",
                    for chat in chats {
                        ProjectChat {
                            id: chat.id.clone(),
                            title: chat.title.clone().unwrap_or_else(|| "(untitled)".into()),
                            project: name.clone(),
                            route,
                            active_project,
                            active_conv_id,
                            collapsed,
                        }
                    }
                }
            }
        }
    }
}

#[component]
fn ProjectChat(
    id: String,
    title: String,
    project: String,
    mut route: Signal<Route>,
    mut active_project: Signal<Option<String>>,
    mut active_conv_id: Signal<Option<String>>,
    mut collapsed: Signal<bool>,
) -> Element {
    let open_id = id.clone();
    rsx! {
        li {
            class: "grok-row project-chat",
            button {
                class: "grok-row-btn",
                r#type: "button",
                onclick: move |_| {
                    active_project.set(Some(project.clone()));
                    active_conv_id.set(Some(open_id.clone()));
                    route.set(Route::ChatSession);
                    collapsed.set(collapsed_after_pick(
                        crate::is_narrow_viewport(),
                        collapsed(),
                    ));
                },
                span { class: "grok-row-title", "{title}" }
            }
        }
    }
}

pub(super) fn project_for_open(
    loaded: Option<&Result<super::ConversationPage, String>>,
    id: &str,
) -> Option<String> {
    loaded?
        .as_ref()
        .ok()?
        .headers
        .iter()
        .find(|header| header.id == id)?
        .project
        .clone()
}

#[component]
pub(super) fn PluginList(on_open: EventHandler<()>, collapsed: Signal<bool>) -> Element {
    rsx! {
        ul {
            class: "grok-list",
            li {
                class: "grok-row",
                button {
                    class: "grok-row-btn",
                    r#type: "button",
                    onclick: move |_| {
                        on_open.call(());
                        close_phone_drawer(collapsed);
                    },
                    PluginGlyph {}
                    span { class: "grok-row-title", "Manage plugins" }
                }
            }
        }
    }
}
