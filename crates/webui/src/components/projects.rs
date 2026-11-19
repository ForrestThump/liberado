//! Projects grid. A person can create a chat project here.
//!
//! Topology rows stay in the list and keep their coding root. They are not renamed here.
//! The create control is a round button fixed to the viewport. The fields live in a dialog.

use dioxus::prelude::*;

#[cfg(target_arch = "wasm32")]
use super::project_catalog::{create_user_project, delete_user_project, update_user_project};
use super::project_data::{
    PROJECT_SOURCE, close_phone_drawer, delete_prompt, fetch_projects, new_project_body,
    project_dot, project_editable, replace_project_path,
};
use super::sidebar::Route;
#[cfg(target_arch = "wasm32")]
use crate::signal_alive::{restart_if_alive, set_if_alive};

const NEW_PROJECT_FAB_ID: &str = "new-project-fab";
const NEW_PROJECT_NAME_ID: &str = "new-project-name";

#[cfg(target_arch = "wasm32")]
fn focus_by_id(id: &str) {
    use wasm_bindgen::JsCast;

    let Some(el) = web_sys::window()
        .and_then(|window| window.document())
        .and_then(|document| document.get_element_by_id(id))
        .and_then(|el| el.dyn_into::<web_sys::HtmlElement>().ok())
    else {
        return;
    };
    let _ = el.focus();
}

#[cfg(not(target_arch = "wasm32"))]
fn focus_by_id(_id: &str) {}

#[component]
pub fn Projects(
    api_base: String,
    mut route: Signal<Route>,
    mut project_focus: Signal<Option<String>>,
    mut collapsed: Signal<bool>,
) -> Element {
    let mut form_open = use_signal(|| false);
    let mut was_open = use_signal(|| false);
    let name = use_signal(String::new);
    let instructions = use_signal(String::new);
    let mut error = use_signal(|| None::<String>);
    use_effect(move || {
        let open = form_open();
        if was_open() && !open {
            focus_by_id(NEW_PROJECT_FAB_ID);
        }
        if was_open() != open {
            was_open.set(open);
        }
    });
    let editing = use_signal(|| None::<String>);
    let draft = use_signal(String::new);
    let confirming = use_signal(|| None::<String>);
    let projects = use_resource({
        let base = api_base.clone();
        move || {
            let base = base.clone();
            async move { fetch_projects(&base).await }
        }
    });
    use_effect(move || {
        replace_project_path("/projects");
    });

    rsx! {
        div {
            class: "grok-page projects-page",
            div {
                class: "grok-page-header",
                div { class: "grok-page-eyebrow", "Projects" }
                h1 { class: "grok-page-title", "Projects" }
                p { class: "grok-page-subtitle", "{PROJECT_SOURCE}" }
            }
            if form_open() {
                NewProjectDialog {
                    api_base: api_base.clone(),
                    name: name,
                    instructions: instructions,
                    error: error,
                    projects: projects,
                    form_open: form_open,
                    route: route,
                    project_focus: project_focus,
                    collapsed: collapsed,
                }
            }
            div {
                class: "grok-page-body",
                if !form_open() {
                    if let Some(message) = error() {
                        p { class: "project-status", "{message}" }
                    }
                }
                match projects.read().as_ref() {
                    None => rsx! { p { class: "project-status", "Loading..." } },
                    Some(Err(message)) => rsx! { p { class: "project-status", "{message}" } },
                    Some(Ok(rows)) if rows.is_empty() => rsx! {
                        p { class: "project-status", "No projects yet." }
                    },
                    Some(Ok(rows)) => rsx! {
                        ul {
                            class: "project-grid",
                            for (index, project) in rows.iter().enumerate() {
                                ProjectCard {
                                    id: project.id.clone(),
                                    name: project.name.clone(),
                                    source: project.source.clone(),
                                    root: project.root.clone().unwrap_or_default(),
                                    instructions: project.instructions.clone().unwrap_or_default(),
                                    dot: project_dot(index),
                                    editing: editing() == Some(project.id.clone()),
                                    confirming: confirming() == Some(project.id.clone()),
                                    draft: draft(),
                                    api_base: api_base.clone(),
                                    projects: projects,
                                    error: error,
                                    editing_id: editing,
                                    draft_name: draft,
                                    confirming_id: confirming,
                                    route: route,
                                    project_focus: project_focus,
                                    collapsed: collapsed,
                                }
                            }
                        }
                    },
                }
            }
            button {
                id: "{NEW_PROJECT_FAB_ID}",
                class: "project-fab",
                r#type: "button",
                "aria-label": "New project",
                onclick: move |_| {
                    error.set(None);
                    form_open.set(true);
                },
                "+"
            }
        }
    }
}

#[component]
fn NewProjectDialog(
    api_base: String,
    name: Signal<String>,
    instructions: Signal<String>,
    error: Signal<Option<String>>,
    projects: Resource<Result<Vec<super::project_data::ProjectRow>, String>>,
    form_open: Signal<bool>,
    route: Signal<Route>,
    project_focus: Signal<Option<String>>,
    collapsed: Signal<bool>,
) -> Element {
    use_effect(|| focus_by_id(NEW_PROJECT_NAME_ID));
    let submit_base = api_base.clone();
    let name_enter_base = api_base.clone();
    let notes_enter_base = api_base;
    rsx! {
        div {
            class: "modal-backdrop project-create-backdrop",
            role: "presentation",
            onclick: move |_| {
                error.set(None);
                form_open.set(false);
            },
            div {
                class: "project-dialog",
                role: "dialog",
                "aria-modal": "true",
                "aria-labelledby": "new-project-title",
                onclick: move |event| event.stop_propagation(),
                onkeydown: move |event| {
                    if event.key() == Key::Escape {
                        event.prevent_default();
                        error.set(None);
                        form_open.set(false);
                    }
                },
                h2 { id: "new-project-title", class: "project-dialog-title", "New project" }
                form {
                    class: "project-form",
                    novalidate: true,
                    onsubmit: move |event| {
                        event.prevent_default();
                        queue_create(
                            submit_base.clone(),
                            name(),
                            instructions(),
                            projects,
                            error,
                            form_open,
                            name,
                            instructions,
                            route,
                            project_focus,
                            collapsed,
                        );
                    },
                    label {
                        "Name"
                        input {
                            id: "{NEW_PROJECT_NAME_ID}",
                            r#type: "text",
                            required: true,
                            maxlength: "80",
                            value: "{name}",
                            oninput: move |event| name.set(event.value()),
                            onkeydown: move |event| {
                                if event.key() == Key::Enter {
                                    event.prevent_default();
                                    queue_create(
                                        name_enter_base.clone(),
                                        name(),
                                        instructions(),
                                        projects,
                                        error,
                                        form_open,
                                        name,
                                        instructions,
                                        route,
                                        project_focus,
                                        collapsed,
                                    );
                                }
                            },
                        }
                    }
                    label {
                        "Instructions"
                        textarea {
                            id: "new-project-instructions",
                            rows: "3",
                            maxlength: "2000",
                            value: "{instructions}",
                            oninput: move |event| instructions.set(event.value()),
                            onkeydown: move |event| {
                                if event.key() == Key::Enter
                                    && !event.modifiers().contains(Modifiers::SHIFT)
                                {
                                    event.prevent_default();
                                    queue_create(
                                        notes_enter_base.clone(),
                                        name(),
                                        instructions(),
                                        projects,
                                        error,
                                        form_open,
                                        name,
                                        instructions,
                                        route,
                                        project_focus,
                                        collapsed,
                                    );
                                }
                            },
                        }
                    }
                    if let Some(message) = error() {
                        p { class: "project-status", role: "alert", "{message}" }
                    }
                    div {
                        class: "project-dialog-actions",
                        button { class: "lib-btn primary", r#type: "submit", "Create" }
                        button {
                            class: "lib-btn",
                            r#type: "button",
                            onclick: move |_| {
                                error.set(None);
                                form_open.set(false);
                            },
                            "Cancel"
                        }
                    }
                }
            }
        }
    }
}

#[component]
fn ProjectCard(
    id: String,
    name: String,
    source: String,
    root: String,
    instructions: String,
    dot: &'static str,
    editing: bool,
    confirming: bool,
    draft: String,
    api_base: String,
    projects: Resource<Result<Vec<super::project_data::ProjectRow>, String>>,
    error: Signal<Option<String>>,
    editing_id: Signal<Option<String>>,
    draft_name: Signal<String>,
    confirming_id: Signal<Option<String>>,
    route: Signal<Route>,
    project_focus: Signal<Option<String>>,
    collapsed: Signal<bool>,
) -> Element {
    let editable = project_editable(&source);
    let open_id = id.clone();
    let rename_id = id.clone();
    let delete_id = id.clone();
    let edit_id = id.clone();
    let confirm_id = id.clone();
    let rename_base = api_base.clone();
    let delete_base = api_base.clone();
    rsx! {
        li {
            class: "project-card",
            if editing {
                form {
                    class: "project-form",
                    onsubmit: move |event| {
                        event.prevent_default();
                        queue_rename(
                            rename_base.clone(),
                            rename_id.clone(),
                            draft_name(),
                            projects,
                            error,
                            editing_id,
                        );
                    },
                    input {
                        r#type: "text",
                        value: "{draft}",
                        "aria-label": "Project name",
                        oninput: move |event| draft_name.set(event.value()),
                    }
                    button { class: "lib-btn primary", r#type: "submit", "Save" }
                    button {
                        class: "lib-btn",
                        r#type: "button",
                        onclick: move |_| editing_id.set(None),
                        "Cancel"
                    }
                }
            } else {
                button {
                    class: "project-card-open",
                    r#type: "button",
                    onclick: move |_| open_detail(open_id.clone(), route, project_focus, collapsed),
                    span { class: "grok-row-dot", style: "background: {dot};", "aria-hidden": "true" }
                    span { class: "project-card-name", "{name}" }
                    if !instructions.is_empty() {
                        span { class: "project-card-meta", "{instructions}" }
                    }
                    if !root.is_empty() {
                        span { class: "project-card-meta", "{root}" }
                    }
                }
            }
            if editable && !editing {
                div {
                    class: "project-card-actions",
                    if confirming {
                        p { class: "project-card-meta", "{delete_prompt(&name)}" }
                        button {
                            class: "lib-btn danger",
                            r#type: "button",
                            onclick: move |_| {
                                queue_delete(
                                    delete_base.clone(),
                                    delete_id.clone(),
                                    projects,
                                    error,
                                    confirming_id,
                                );
                            },
                            "Delete"
                        }
                        button {
                            class: "lib-btn",
                            r#type: "button",
                            onclick: move |_| confirming_id.set(None),
                            "Cancel"
                        }
                    } else {
                        button {
                            class: "lib-btn",
                            r#type: "button",
                            onclick: move |_| {
                                draft_name.set(name.clone());
                                editing_id.set(Some(edit_id.clone()));
                                confirming_id.set(None);
                            },
                            "Rename"
                        }
                        button {
                            class: "lib-btn danger",
                            r#type: "button",
                            onclick: move |_| {
                                confirming_id.set(Some(confirm_id.clone()));
                                editing_id.set(None);
                            },
                            "Delete"
                        }
                    }
                }
            }
        }
    }
}

fn open_detail(
    id: String,
    mut route: Signal<Route>,
    mut project_focus: Signal<Option<String>>,
    collapsed: Signal<bool>,
) {
    project_focus.set(Some(id));
    route.set(Route::ProjectDetail);
    close_phone_drawer(collapsed);
}

fn queue_create(
    api_base: String,
    name: String,
    instructions: String,
    projects: Resource<Result<Vec<super::project_data::ProjectRow>, String>>,
    mut error: Signal<Option<String>>,
    form_open: Signal<bool>,
    name_slot: Signal<String>,
    instructions_slot: Signal<String>,
    route: Signal<Route>,
    project_focus: Signal<Option<String>>,
    collapsed: Signal<bool>,
) {
    let Some(body) = new_project_body(&name, &instructions) else {
        error.set(Some("A project needs a name.".into()));
        return;
    };
    #[cfg(target_arch = "wasm32")]
    {
        let mut projects = projects;
        let mut error = error;
        let mut form_open = form_open;
        let mut name_slot = name_slot;
        let mut instructions_slot = instructions_slot;
        let mut route = route;
        let mut project_focus = project_focus;
        wasm_bindgen_futures::spawn_local(async move {
            match create_user_project(&api_base, &body).await {
                Ok(row) => {
                    let _ = restart_if_alive(&mut projects);
                    let _ = set_if_alive(&mut error, None);
                    let _ = set_if_alive(&mut form_open, false);
                    let _ = set_if_alive(&mut name_slot, String::new());
                    let _ = set_if_alive(&mut instructions_slot, String::new());
                    let _ = set_if_alive(&mut project_focus, Some(row.id));
                    let _ = set_if_alive(&mut route, Route::ProjectDetail);
                    close_phone_drawer(collapsed);
                }
                Err(message) => {
                    let _ = set_if_alive(&mut error, Some(message));
                }
            }
        });
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = (
            api_base,
            body,
            projects,
            form_open,
            name_slot,
            instructions_slot,
            route,
            project_focus,
            collapsed,
        );
    }
}

fn queue_rename(
    api_base: String,
    id: String,
    name: String,
    projects: Resource<Result<Vec<super::project_data::ProjectRow>, String>>,
    mut error: Signal<Option<String>>,
    editing: Signal<Option<String>>,
) {
    let Some(body) = new_project_body(&name, "") else {
        error.set(Some("A project needs a name.".into()));
        return;
    };
    #[cfg(target_arch = "wasm32")]
    {
        let mut projects = projects;
        let mut error = error;
        let mut editing = editing;
        wasm_bindgen_futures::spawn_local(async move {
            match update_user_project(&api_base, &id, &body).await {
                Ok(_) => {
                    let _ = restart_if_alive(&mut projects);
                    let _ = set_if_alive(&mut error, None);
                    let _ = set_if_alive(&mut editing, None);
                }
                Err(message) => {
                    let _ = set_if_alive(&mut error, Some(message));
                }
            }
        });
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = (api_base, id, body, projects, editing);
    }
}

fn queue_delete(
    api_base: String,
    id: String,
    projects: Resource<Result<Vec<super::project_data::ProjectRow>, String>>,
    error: Signal<Option<String>>,
    confirming: Signal<Option<String>>,
) {
    #[cfg(target_arch = "wasm32")]
    {
        let mut projects = projects;
        let mut error = error;
        let mut confirming = confirming;
        wasm_bindgen_futures::spawn_local(async move {
            match delete_user_project(&api_base, &id).await {
                Ok(()) => {
                    let _ = restart_if_alive(&mut projects);
                    let _ = set_if_alive(&mut error, None);
                    let _ = set_if_alive(&mut confirming, None);
                }
                Err(message) => {
                    let _ = set_if_alive(&mut error, Some(message));
                }
            }
        });
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = (api_base, id, projects, error, confirming);
    }
}
