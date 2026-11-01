//! Owner notifications — Web Push subscriptions and outbox visibility.

use dioxus::prelude::*;
use serde::Deserialize;

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
pub(crate) struct PushStatus {
    #[serde(default)]
    pub vapid_configured: bool,
    #[serde(default)]
    pub subscription_count: u32,
    #[serde(default)]
    pub public_key: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
pub(crate) struct DevicePushState {
    #[serde(default)]
    notification_permission: String,
    #[serde(default)]
    service_worker_state: String,
    #[serde(default)]
    page_controlled: bool,
    #[serde(default)]
    browser_subscribed: bool,
    #[serde(default)]
    subscription_host: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub(crate) struct SubscriptionRow {
    pub id: i64,
    pub label: String,
    pub host: String,
    pub endpoint: String,
}

fn parse_status(body: &str) -> Result<PushStatus, String> {
    serde_json::from_str(body).map_err(|e| format!("Bad push status: {e}"))
}

fn parse_subscriptions(body: &str) -> Result<Vec<SubscriptionRow>, String> {
    serde_json::from_str(body).map_err(|e| format!("Bad subscriptions: {e}"))
}

fn parse_device_state(body: &str) -> Result<DevicePushState, String> {
    serde_json::from_str(body).map_err(|e| format!("Bad device push state: {e}"))
}

async fn fetch_status(api_base: &str) -> Result<PushStatus, String> {
    let url = format!("{api_base}/api/push/status");
    let resp = crate::http::get(&url)
        .await
        .map_err(|e| format!("Failed to reach daemon: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("Push status unavailable ({})", resp.status()));
    }
    let text = resp
        .text()
        .await
        .map_err(|e| format!("Bad response: {e}"))?;
    parse_status(&text)
}

async fn fetch_subscriptions(api_base: &str) -> Result<Vec<SubscriptionRow>, String> {
    let url = format!("{api_base}/api/push/subscriptions");
    let resp = crate::http::get(&url)
        .await
        .map_err(|e| format!("Failed to reach daemon: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("Subscriptions unavailable ({})", resp.status()));
    }
    let text = resp
        .text()
        .await
        .map_err(|e| format!("Bad response: {e}"))?;
    parse_subscriptions(&text)
}

#[cfg(target_arch = "wasm32")]
async fn fetch_device_push_state() -> Result<DevicePushState, String> {
    use crate::push::{fetch_push_device_state_json, js_value_to_error_text};

    fetch_push_device_state_json()
        .await
        .map_err(|e| js_value_to_error_text(&e))
        .and_then(|json| parse_device_state(&json))
}

#[cfg(not(target_arch = "wasm32"))]
async fn fetch_device_push_state() -> Result<DevicePushState, String> {
    Ok(DevicePushState::default())
}

async fn refresh_all(
    api_base: &str,
) -> Result<(PushStatus, Vec<SubscriptionRow>, DevicePushState), String> {
    let status = fetch_status(api_base).await?;
    let subs = fetch_subscriptions(api_base).await.unwrap_or_default();
    let device = fetch_device_push_state().await.unwrap_or_default();
    Ok((status, subs, device))
}

#[cfg(target_arch = "wasm32")]
async fn enable_push(api_base: &str, label: &str) -> Result<(), String> {
    use crate::push::{enable_push_subscribe, js_value_to_error_text};

    let key_url = format!("{api_base}/api/push/vapid-public-key");
    let resp = crate::http::get(&key_url)
        .await
        .map_err(|e| format!("Failed to reach daemon: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!(
            "Web Push keys are not configured on the daemon (HTTP {}).",
            resp.status()
        ));
    }
    let body: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("Bad VAPID response: {e}"))?;
    let Some(public_key) = body.get("public_key").and_then(|v| v.as_str()) else {
        return Err("Daemon returned no VAPID public key.".into());
    };
    enable_push_subscribe(public_key, api_base, label, "")
        .await
        .map_err(|e| js_value_to_error_text(&e))
        .map(|_| ())
}

#[cfg(not(target_arch = "wasm32"))]
async fn enable_push(_api_base: &str, _label: &str) -> Result<(), String> {
    Err("Enable push from the installed WebUI in a browser.".into())
}

async fn send_test(api_base: &str) -> Result<(), String> {
    let url = format!("{api_base}/api/push/test");
    let resp = crate::http::post_json(&url, &serde_json::json!({}))
        .await
        .map_err(|e| format!("Failed to reach daemon: {e}"))?;
    if resp.status().is_success() {
        Ok(())
    } else {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        Err(format!("Test send failed (HTTP {status}): {body}"))
    }
}

async fn remove_subscription(api_base: &str, endpoint: &str) -> Result<(), String> {
    let url = format!("{api_base}/api/push/subscriptions");
    let resp = crate::http::delete_json(&url, &serde_json::json!({ "endpoint": endpoint }))
        .await
        .map_err(|e| format!("Failed to reach daemon: {e}"))?;
    if resp.status().is_success() {
        Ok(())
    } else {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        Err(format!("Remove failed (HTTP {status}): {body}"))
    }
}

fn device_state_summary(device: &DevicePushState, server_count: u32) -> String {
    let controlled = if device.page_controlled {
        "controlled"
    } else {
        "not controlled"
    };
    let local = if device.browser_subscribed {
        format!(
            "this browser subscribed ({})",
            if device.subscription_host.is_empty() {
                "push service".into()
            } else {
                device.subscription_host.clone()
            }
        )
    } else {
        "this browser has no push subscription".into()
    };
    format!(
        "Notification permission: {}. Service worker: {} ({controlled}). {local}. Daemon subscriptions: {server_count}.",
        device.notification_permission, device.service_worker_state,
    )
}

#[component]
fn RemoveSubscriptionButton(
    api_base: String,
    endpoint: String,
    on_done: EventHandler<Result<(PushStatus, Vec<SubscriptionRow>, DevicePushState), String>>,
    on_error: EventHandler<String>,
) -> Element {
    rsx! {
        button {
            class: "btn-link",
            onclick: move |_| {
                let base = api_base.clone();
                let endpoint = endpoint.clone();
                spawn(async move {
                    if let Err(err) = remove_subscription(&base, &endpoint).await {
                        on_error.call(err);
                    } else if let Ok(bundle) = refresh_all(&base).await {
                        on_done.call(Ok(bundle));
                    }
                });
            },
            "Remove"
        }
    }
}

#[component]
pub fn Notifications(api_base: String) -> Element {
    let mut status = use_signal(PushStatus::default);
    let mut subs = use_signal(Vec::<SubscriptionRow>::new);
    let mut device_state = use_signal(DevicePushState::default);
    let mut label = use_signal(|| "This device".to_string());
    let mut message = use_signal(String::new);
    let base = api_base.clone();

    use_effect(move || {
        let base = base.clone();
        spawn(async move {
            match refresh_all(&base).await {
                Ok((row, rows, device)) => {
                    status.set(row);
                    subs.set(rows);
                    device_state.set(device);
                }
                Err(err) => message.set(err),
            }
        });
    });

    let ios_hint = "On iPhone: Safari → Share → Add to Home Screen (iOS 16.4+), open from the icon, then enable notifications here.";
    let enable_base = api_base.clone();
    let test_base = api_base.clone();
    let list_base = api_base.clone();

    rsx! {
        section { class: "page-surface notifications-page",
            h1 { class: "page-title", "Notifications" }
            p { class: "page-lead",
                "Enable Web Push on this device. Subscription management requires owner sign-in; delivered pushes ack without the owner cookie."
            }
            if !message().is_empty() {
                p { class: "notifications-error", "{message()}" }
            }
            p { class: "notifications-meta",
                if status().vapid_configured {
                    "Web Push is configured on the daemon."
                } else {
                    "Web Push keys are not configured — set LIBERADO_VAPID_* on the homelab node."
                }
            }
            p { class: "notifications-meta",
                "{device_state_summary(&device_state(), status().subscription_count)}"
            }
            p { class: "notifications-hint", "{ios_hint}" }
            label { class: "notifications-label",
                "Device label"
                input {
                    r#type: "text",
                    value: "{label()}",
                    oninput: move |evt| label.set(evt.value()),
                }
            }
            div { class: "notifications-actions",
                button {
                    class: "btn-primary",
                    onclick: move |_| {
                        let base = enable_base.clone();
                        let name = label();
                        spawn(async move {
                            match enable_push(&base, &name).await {
                                Ok(()) => {
                                    message.set("Push enabled on this device.".into());
                                    if let Ok((row, rows, device)) = refresh_all(&base).await {
                                        status.set(row);
                                        subs.set(rows);
                                        device_state.set(device);
                                    }
                                }
                                Err(err) => message.set(err),
                            }
                        });
                    },
                    "Enable push on this device"
                }
                button {
                    class: "btn-secondary",
                    onclick: move |_| {
                        let base = test_base.clone();
                        spawn(async move {
                            match send_test(&base).await {
                                Ok(()) => message.set("Test notification queued.".into()),
                                Err(err) => message.set(err),
                            }
                        });
                    },
                    "Send test"
                }
            }
            if !subs().is_empty() {
                h2 { class: "page-subtitle", "Devices" }
                ul { class: "notifications-list",
                    for row in subs().iter().cloned() {
                        li { key: "{row.id}",
                            span { "{row.label} ({row.host})" }
                            RemoveSubscriptionButton {
                                api_base: list_base.clone(),
                                endpoint: row.endpoint.clone(),
                                on_done: move |result: Result<(PushStatus, Vec<SubscriptionRow>, DevicePushState), String>| {
                                    if let Ok((row, rows, device)) = result {
                                        status.set(row);
                                        subs.set(rows);
                                        device_state.set(device);
                                    }
                                },
                                on_error: move |err: String| message.set(err),
                            }
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_status_json_parses() {
        let row =
            parse_status(r#"{"vapid_configured":true,"subscription_count":2,"public_key":"abc"}"#)
                .unwrap();
        assert!(row.vapid_configured);
        assert_eq!(row.subscription_count, 2);
        assert_eq!(row.public_key.as_deref(), Some("abc"));
    }

    #[test]
    fn subscriptions_json_parses() {
        let rows = parse_subscriptions(r#"[{"id":1,"label":"phone","host":"fcm.googleapis.com","endpoint":"https://fcm.googleapis.com/x"}]"#)
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].label, "phone");
    }

    #[test]
    fn device_state_json_parses() {
        let row = parse_device_state(
            r#"{"notification_permission":"granted","service_worker_state":"active","page_controlled":true,"browser_subscribed":true,"subscription_host":"fcm.googleapis.com"}"#,
        )
        .unwrap();
        assert_eq!(row.notification_permission, "granted");
        assert!(row.browser_subscribed);
    }
}
