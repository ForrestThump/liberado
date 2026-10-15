//! Projects view. Rows come from `GET /api/projects`.
//!
//! A click selects that project for this window and opens the chat. See
//! [`crate::components::project_data`] for why the chat itself cannot store it.

use dioxus::prelude::*;

use super::project_data::{
    PROJECT_BIND_GAP, PROJECT_SOURCE, collapsed_after_pick, fetch_projects, project_dot,
    selected_project_name,
};
use super::sidebar::Route;

#[component]
pub fn Projects(
    api_base: String,
    active_project: Signal<Option<String>>,
    mut route: Signal<Route>,
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
        div {
            class: "grok-page projects-page",
            div {
                class: "grok-page-header",
                div { class: "grok-page-eyebrow", "Projects" }
                h1 { class: "grok-page-title", "Projects" }
                p { class: "grok-page-subtitle", "{PROJECT_SOURCE}" }
                p { class: "grok-page-subtitle", "{PROJECT_BIND_GAP}" }
            }
            div {
                class: "grok-page-body",
                match projects.read().as_ref() {
                    None => rsx! { p { class: "project-status", "Loading..." } },
                    Some(Err(error)) => rsx! { p { class: "project-status", "{error}" } },
                    Some(Ok(rows)) if rows.is_empty() => rsx! {
                        p { class: "project-status", "No projects are configured. They are declared in topology.toml." }
                    },
                    Some(Ok(rows)) => rsx! {
                        ul {
                            class: "project-list",
                            for (index, project) in rows.iter().enumerate() {
                                ProjectPick {
                                    name: project.name.clone(),
                                    root: project.root.clone(),
                                    dot: project_dot(index),
                                    active: active_project().as_deref() == Some(project.name.as_str()),
                                    active_project,
                                    route,
                                    collapsed,
                                }
                            }
                        }
                    },
                }
            }
        }
    }
}

#[component]
fn ProjectPick(
    name: String,
    root: String,
    dot: &'static str,
    active: bool,
    mut active_project: Signal<Option<String>>,
    mut route: Signal<Route>,
    mut collapsed: Signal<bool>,
) -> Element {
    let class = if active {
        "project-select active"
    } else {
        "project-select"
    };
    let pick_name = name.clone();
    rsx! {
        li {
            button {
                class: "{class}",
                r#type: "button",
                onclick: move |_| pick_project(pick_name.clone(), active_project, route, collapsed),
                span {
                    class: "grok-row-dot",
                    style: "background: {dot};",
                    "aria-hidden": "true"
                }
                span { class: "project-select-text",
                    span { class: "project-select-name", "{name}" }
                    span { class: "project-root", "{root}" }
                }
            }
        }
    }
}

fn pick_project(
    name: String,
    mut active_project: Signal<Option<String>>,
    mut route: Signal<Route>,
    mut collapsed: Signal<bool>,
) {
    let Some(name) = selected_project_name(&name) else {
        return;
    };
    active_project.set(Some(name));
    route.set(Route::Chat);
    collapsed.set(collapsed_after_pick(
        crate::is_narrow_viewport(),
        collapsed(),
    ));
}
