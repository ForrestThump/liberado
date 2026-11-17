//! One project's chats.

use dioxus::prelude::*;

#[cfg(target_arch = "wasm32")]
use super::project_data::create_project_chat;
use super::project_data::{
    chat_date, close_phone_drawer, fetch_project_chats, fetch_projects, project_path,
    replace_project_path,
};
use super::sidebar::Route;
#[cfg(target_arch = "wasm32")]
use crate::signal_alive::{bump_if_alive, set_if_alive};

#[component]
pub fn ProjectPage(
    api_base: String,
    project_id: String,
    mut route: Signal<Route>,
    mut active_project: Signal<Option<String>>,
    mut active_conv_id: Signal<Option<String>>,
    mut active_surface: Signal<Option<chat_client_contract::SurfaceMode>>,
    mut list_epoch: Signal<u64>,
    mut collapsed: Signal<bool>,
) -> Element {
    let error = use_signal(|| None::<String>);
    let id_for_name = project_id.clone();
    let catalog = use_resource({
        let base = api_base.clone();
        move || {
            let base = base.clone();
            async move { fetch_projects(&base).await }
        }
    });
    let chats = use_resource({
        let base = api_base.clone();
        let id = project_id.clone();
        move || {
            let base = base.clone();
            let id = id.clone();
            async move { fetch_project_chats(&base, &id).await }
        }
    });
    let path = project_path(Some(&project_id));
    use_effect(move || {
        replace_project_path(&path);
    });
    let title = catalog
        .read()
        .as_ref()
        .and_then(|result| result.as_ref().ok())
        .and_then(|rows| rows.iter().find(|row| row.id == id_for_name))
        .map(|row| row.name.clone())
        .unwrap_or_else(|| project_id.clone());

    rsx! {
        div {
            class: "grok-page projects-page project-page",
            div {
                class: "grok-page-header",
                button {
                    class: "lib-btn",
                    r#type: "button",
                    "aria-label": "Back to projects",
                    onclick: move |_| route.set(Route::Projects),
                    "Back"
                }
                h1 { class: "grok-page-title", "{title}" }
            }
            div {
                class: "grok-page-body",
                div {
                    class: "project-toolbar",
                    button {
                        class: "lib-btn primary",
                        r#type: "button",
                        "aria-label": "New chat",
                        onclick: move |_| {
                            queue_new_chat(
                                api_base.clone(),
                                project_id.clone(),
                                route,
                                active_project,
                                active_conv_id,
                                active_surface,
                                list_epoch,
                                collapsed,
                                error,
                            );
                        },
                        "New chat"
                    }
                }
                if let Some(message) = error() {
                    p { class: "project-status", "{message}" }
                }
                match chats.read().as_ref() {
                    None => rsx! { p { class: "project-status", "Loading..." } },
                    Some(Err(message)) => rsx! { p { class: "project-status", "{message}" } },
                    Some(Ok(rows)) if rows.is_empty() => rsx! {
                        p { class: "project-status", "No chats in this project yet." }
                    },
                    Some(Ok(rows)) => rsx! {
                        ul {
                            class: "project-chat-list",
                            for chat in rows.iter() {
                                li {
                                    button {
                                        class: "project-chat-open",
                                        r#type: "button",
                                        onclick: {
                                            let chat_id = chat.id.clone();
                                            let project = project_id.clone();
                                            move |_| open_chat(
                                                chat_id.clone(),
                                                project.clone(),
                                                route,
                                                active_project,
                                                active_conv_id,
                                                active_surface,
                                                collapsed,
                                            )
                                        },
                                        span { class: "project-card-name", "{chat.title.clone().unwrap_or_else(|| \"Untitled\".into())}" }
                                        span { class: "project-card-meta", "{chat_date(&chat.updated_at)}" }
                                    }
                                }
                            }
                        }
                    },
                }
            }
        }
    }
}

fn open_chat(
    id: String,
    project: String,
    mut route: Signal<Route>,
    mut active_project: Signal<Option<String>>,
    mut active_conv_id: Signal<Option<String>>,
    mut active_surface: Signal<Option<chat_client_contract::SurfaceMode>>,
    collapsed: Signal<bool>,
) {
    active_project.set(Some(project));
    active_conv_id.set(Some(id));
    active_surface.set(Some(chat_client_contract::SurfaceMode::Chat));
    route.set(Route::ChatSession);
    close_phone_drawer(collapsed);
    replace_project_path("/");
}

fn queue_new_chat(
    api_base: String,
    project: String,
    route: Signal<Route>,
    active_project: Signal<Option<String>>,
    active_conv_id: Signal<Option<String>>,
    active_surface: Signal<Option<chat_client_contract::SurfaceMode>>,
    list_epoch: Signal<u64>,
    collapsed: Signal<bool>,
    error: Signal<Option<String>>,
) {
    #[cfg(target_arch = "wasm32")]
    {
        let mut route = route;
        let mut active_project = active_project;
        let mut active_conv_id = active_conv_id;
        let mut active_surface = active_surface;
        let mut list_epoch = list_epoch;
        let mut error = error;
        wasm_bindgen_futures::spawn_local(async move {
            match create_project_chat(&api_base, &project).await {
                Ok(id) => {
                    let _ = set_if_alive(&mut error, None);
                    let _ = set_if_alive(&mut active_project, Some(project));
                    let _ = set_if_alive(&mut active_conv_id, Some(id));
                    let _ = set_if_alive(
                        &mut active_surface,
                        Some(chat_client_contract::SurfaceMode::Chat),
                    );
                    let _ = set_if_alive(&mut route, Route::ChatSession);
                    let _ = bump_if_alive(&mut list_epoch);
                    close_phone_drawer(collapsed);
                    replace_project_path("/");
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
            project,
            route,
            active_project,
            active_conv_id,
            active_surface,
            list_epoch,
            collapsed,
            error,
        );
    }
}
