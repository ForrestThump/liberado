//! Single outbox row detail view (linked from push `payload.url`).

mod model;

use dioxus::prelude::*;

use crate::components::sidebar::Route;
use crate::http;
use crate::notification_route::{
    detail_actions, detail_body_adds_information, detail_status_line, resolution_state_text,
};

use self::model::OutboxDetail;

#[component]
pub fn NotificationDetailPage(api_base: String, outbox_id: i64, route: Signal<Route>) -> Element {
    let mut detail = use_signal(|| None::<OutboxDetail>);
    let mut error = use_signal(|| None::<String>);
    let mut busy = use_signal(|| false);
    let base = api_base.clone();

    use_effect(move || {
        let base = base.clone();
        spawn(async move {
            match fetch_detail(&base, outbox_id).await {
                Ok(row) => {
                    detail.set(Some(row));
                    error.set(None);
                }
                Err(err) => error.set(Some(err)),
            }
        });
    });

    let back = move |_| {
        route.set(Route::Notifications);
        sync_path("/notifications");
    };

    let row_snapshot = detail();
    let source_type = row_snapshot
        .as_ref()
        .and_then(|row| row.source.as_ref())
        .and_then(|value| value.get("type"))
        .and_then(|value| value.as_str());
    let actions = row_snapshot.as_ref().map(|row| {
        let resolved = row.resolution.is_some();
        detail_actions(&row.kind, source_type, resolved)
    });
    let resolved_line = row_snapshot.as_ref().and_then(|row| {
        resolution_state_text(row.resolution.as_deref(), row.snooze_until_local.as_deref())
    });
    let status_line = row_snapshot
        .as_ref()
        .map(|row| detail_status_line(&row.kind, row.late, row.catch_up, &row.due_at_local));
    let source_path = row_snapshot
        .as_ref()
        .and_then(|row| source_note_path(&row.source));
    let body_adds_information = row_snapshot
        .as_ref()
        .map(|row| {
            detail_body_adds_information(
                &row.body,
                status_line.as_deref().unwrap_or_default(),
                source_path.as_deref(),
            )
        })
        .unwrap_or(false);
    let obsidian_url = row_snapshot
        .as_ref()
        .and_then(|row| row.obsidian_url.clone());

    let api_for_complete = api_base.clone();
    let api_for_snooze_15 = api_base.clone();
    let api_for_snooze_60 = api_base.clone();

    rsx! {
        div { class: "grok-page notification-detail",
            div { class: "notification-detail-header",
                button {
                    class: "lib-btn notification-detail-back",
                    r#type: "button",
                    onclick: back,
                    "← Notifications"
                }
            }
            div { class: "card notification-detail-card",
                div { class: "card-body notification-detail-content",
                    if let Some(row) = row_snapshot {
                        if let Some(err) = error() {
                            p { class: "notification-detail-error", "{err}" }
                        }
                        h1 { class: "grok-page-title", "{row.title}" }
                        if let Some(state) = resolved_line {
                            p { class: "notification-detail-resolved", "{state}" }
                        }
                        if let Some(status) = status_line {
                            p { class: "notification-detail-status", "{status}" }
                        }
                        if body_adds_information {
                            p { class: "notification-detail-body", "{row.body}" }
                        }
                        if let Some(path) = source_path {
                            p { class: "notification-detail-source", "Source: {path}" }
                        }
                        if let Some(actions) = actions {
                            div { class: "notification-detail-actions",
                                if actions.show_complete {
                                    button {
                                        class: "lib-btn primary",
                                        r#type: "button",
                                        disabled: busy(),
                                        onclick: move |_| {
                                            let base = api_for_complete.clone();
                                            spawn(async move {
                                                busy.set(true);
                                                match post_action(&base, outbox_id, "complete").await {
                                                    Ok(()) => {
                                                        error.set(None);
                                                        if let Ok(row) = fetch_detail(&base, outbox_id).await {
                                                            detail.set(Some(row));
                                                        }
                                                    }
                                                    Err(err) => error.set(Some(err)),
                                                }
                                                busy.set(false);
                                            });
                                        },
                                        "{actions.complete_label}"
                                    }
                                }
                                if actions.show_snooze {
                                    div { class: "notification-detail-snooze-actions",
                                        button {
                                            class: "lib-btn",
                                            r#type: "button",
                                            disabled: busy(),
                                            onclick: move |_| {
                                                let base = api_for_snooze_15.clone();
                                                spawn(async move {
                                                    busy.set(true);
                                                    if let Err(err) = snooze(&base, outbox_id, 15).await {
                                                        error.set(Some(err));
                                                    } else if let Ok(row) = fetch_detail(&base, outbox_id).await {
                                                        detail.set(Some(row));
                                                        error.set(None);
                                                    }
                                                    busy.set(false);
                                                });
                                            },
                                            // Same length as liberado_notify::web_push::DEFAULT_SNOOZE_MINUTES.
                                            // This surface crate cannot import that constant.
                                            "Snooze 15m"
                                        }
                                        button {
                                            class: "lib-btn",
                                            r#type: "button",
                                            disabled: busy(),
                                            onclick: move |_| {
                                                let base = api_for_snooze_60.clone();
                                                spawn(async move {
                                                    busy.set(true);
                                                    if let Err(err) = snooze(&base, outbox_id, 60).await {
                                                        error.set(Some(err));
                                                    } else if let Ok(row) = fetch_detail(&base, outbox_id).await {
                                                        detail.set(Some(row));
                                                        error.set(None);
                                                    }
                                                    busy.set(false);
                                                });
                                            },
                                            "Snooze 1h"
                                        }
                                    }
                                }
                                if actions.show_obsidian {
                                    if let Some(url) = obsidian_url.clone() {
                                        a {
                                            class: "lib-btn notification-detail-obsidian-action",
                                            href: "{url}",
                                            target: "_blank",
                                            rel: "noopener noreferrer",
                                            "Open in Obsidian"
                                        }
                                    }
                                }
                            }
                        }
                    } else if let Some(err) = error() {
                        p { class: "notification-detail-error", "{err}" }
                    } else {
                        p { class: "notification-detail-loading", "Loading…" }
                    }
                }
            }
        }
    }
}

fn source_note_path(source: &Option<serde_json::Value>) -> Option<String> {
    source.as_ref().and_then(|value| {
        value
            .get("path")
            .and_then(|path| path.as_str())
            .map(str::to_string)
    })
}

async fn fetch_detail(base: &str, id: i64) -> Result<OutboxDetail, String> {
    let url = format!("{base}/api/outbox/{id}");
    let response = http::get(&url)
        .await
        .map_err(|e| format!("Failed to reach daemon: {e}"))?;
    if !response.status().is_success() {
        let status = response.status();
        let message = response
            .json::<serde_json::Value>()
            .await
            .ok()
            .and_then(|value| {
                value
                    .get("error")
                    .and_then(|err| err.as_str())
                    .map(str::to_string)
            })
            .unwrap_or_else(|| format!("HTTP {status}"));
        return Err(message);
    }
    response
        .json::<OutboxDetail>()
        .await
        .map_err(|e| format!("Bad response: {e}"))
}

async fn post_action(base: &str, id: i64, action: &str) -> Result<(), String> {
    let url = format!("{base}/api/outbox/{id}/{action}");
    let response = http::post_empty(&url)
        .await
        .map_err(|e| format!("Failed to reach daemon: {e}"))?;
    if response.status().is_success() {
        Ok(())
    } else {
        let status = response.status();
        let message = response
            .json::<serde_json::Value>()
            .await
            .ok()
            .and_then(|value| {
                value
                    .get("error")
                    .and_then(|err| err.as_str())
                    .map(str::to_string)
            })
            .unwrap_or_else(|| format!("HTTP {status}"));
        Err(message)
    }
}

async fn snooze(base: &str, id: i64, minutes: i64) -> Result<(), String> {
    let url = format!("{base}/api/outbox/{id}/snooze");
    let body = serde_json::json!({ "minutes": minutes });
    let response = http::post_json(&url, &body)
        .await
        .map_err(|e| format!("Failed to reach daemon: {e}"))?;
    if response.status().is_success() {
        Ok(())
    } else {
        let status = response.status();
        let message = response
            .json::<serde_json::Value>()
            .await
            .ok()
            .and_then(|value| {
                value
                    .get("error")
                    .and_then(|err| err.as_str())
                    .map(str::to_string)
            })
            .unwrap_or_else(|| format!("HTTP {status}"));
        Err(message)
    }
}

#[cfg(target_arch = "wasm32")]
fn sync_path(path: &str) {
    if let Some(window) = web_sys::window() {
        if let Ok(history) = window.history() {
            let _ = history.push_state_with_url(&wasm_bindgen::JsValue::NULL, "", Some(path));
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn sync_path(_path: &str) {}
