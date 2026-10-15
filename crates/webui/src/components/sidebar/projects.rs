//! Project and plugin rows in the sidebar.
//!
//! Projects are the daemon's `GET /api/projects` list. A row selects that project and
//! opens the chat. Plugin entry opens the plugins page.

use dioxus::prelude::*;

use super::Route;
use super::rail::{PluginGlyph, ProjectGlyph};
use crate::components::project_data::{
    collapsed_after_pick, fetch_projects, project_dot, selected_project_name,
};

#[component]
pub(super) fn ProjectList(
    api_base: String,
    mut route: Signal<Route>,
    mut active_project: Signal<Option<String>>,
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
                            route,
                            active_project,
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
    mut route: Signal<Route>,
    mut active_project: Signal<Option<String>>,
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
                    active_project.set(Some(name));
                    route.set(Route::Chat);
                    collapsed.set(collapsed_after_pick(
                        crate::is_narrow_viewport(),
                        collapsed(),
                    ));
                },
                span {
                    class: "grok-row-dot",
                    style: "background: {dot};",
                    "aria-hidden": "true"
                }
                ProjectGlyph {}
                span { class: "grok-row-title", "{name}" }
            }
        }
    }
}

#[component]
pub(super) fn PluginList(on_open: EventHandler<()>) -> Element {
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
