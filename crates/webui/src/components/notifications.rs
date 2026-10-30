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
fn js_error_message(value: &wasm_bindgen::JsValue) -> String {
    if let Some(text) = value.as_string() {
        return text;
    }
    js_sys::JSON::stringify(value)
        .ok()
        .and_then(|json| json.as_string())
        .unwrap_or_else(|| "unknown JavaScript error".into())
}

#[cfg(target_arch = "wasm32")]
async fn enable_push(api_base: &str, label: &str) -> Result<(), String> {
    use js_sys::{Function, Promise};
    use wasm_bindgen::JsValue;
    use wasm_bindgen_futures::JsFuture;

    let key_url = format!("{api_base}/api/push/vapid-public-key");
    let resp = crate::http::get(&key_url)
        .await
        .map_err(|e| format!("Failed to reach daemon: {e}"))?;
    if !resp.status().is_success() {
        return Err("Web Push keys are not configured on the daemon.".into());
    }
    let body: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("Bad VAPID response: {e}"))?;
    let Some(public_key) = body.get("public_key").and_then(|v| v.as_str()) else {
        return Err("Daemon returned no VAPID public key.".into());
    };
    let public_key_json =
        serde_json::to_string(public_key).map_err(|e| format!("Bad public key: {e}"))?;
    let api_base_json =
        serde_json::to_string(api_base).map_err(|e| format!("Bad api base: {e}"))?;
    let label_json = serde_json::to_string(label).map_err(|e| format!("Bad label: {e}"))?;
    let script = format!(
        r#"
const reg = await navigator.serviceWorker.ready;
const publicKey = {public_key_json};
const sub = await reg.pushManager.subscribe({{
  userVisibleOnly: true,
  applicationServerKey: (function (b64) {{
    const pad = "=".repeat((4 - (b64.length % 4)) % 4);
    const base64 = (b64 + pad).replace(/-/g, "+").replace(/_/g, "/");
    const raw = atob(base64);
    const out = new Uint8Array(raw.length);
    for (let i = 0; i < raw.length; i++) out[i] = raw.charCodeAt(i);
    return out;
  }})(publicKey);
}});
const json = sub.toJSON();
const apiBase = {api_base_json};
const resp = await fetch(apiBase + "/api/push/subscriptions", {{
  method: "POST",
  credentials: "same-origin",
  headers: {{ "content-type": "application/json", "x-liberado-csrf": document.cookie.match(/liberado_csrf=([^;]+)/)?.[1] || "" }},
  body: JSON.stringify({{ endpoint: json.endpoint, keys: json.keys, label: {label_json} }}),
}});
if (!resp.ok) {{
  const text = await resp.text();
  throw new Error("subscribe failed: " + resp.status + " " + text);
}}
return {{ ok: true }};
"#
    );
    let runner = format!("return (async () => {{\n{script}\n}})();");
    let func = Function::new_with_args("", &runner);
    let promise_val = func
        .call0(&JsValue::NULL)
        .map_err(|e| format!("Push subscribe failed: {}", js_error_message(&e)))?;
    let promise = Promise::from(promise_val);
    JsFuture::from(promise)
        .await
        .map(|_| ())
        .map_err(|e| format!("Push subscribe failed: {}", js_error_message(&e)))
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
        Err(format!("Test send failed ({})", resp.status()))
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
        Err(format!("Remove failed ({})", resp.status()))
    }
}

#[component]
fn RemoveSubscriptionButton(
    api_base: String,
    endpoint: String,
    on_done: EventHandler<Result<Vec<SubscriptionRow>, String>>,
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
                    } else if let Ok(rows) = fetch_subscriptions(&base).await {
                        on_done.call(Ok(rows));
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
    let mut label = use_signal(|| "This device".to_string());
    let mut message = use_signal(String::new);
    let base = api_base.clone();

    use_effect(move || {
        let base = base.clone();
        spawn(async move {
            match fetch_status(&base).await {
                Ok(row) => status.set(row),
                Err(err) => message.set(err),
            }
            if let Ok(rows) = fetch_subscriptions(&base).await {
                subs.set(rows);
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
                " Subscriptions: {status().subscription_count}."
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
                                    if let Ok(rows) = fetch_subscriptions(&base).await {
                                        subs.set(rows);
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
                                on_done: move |result: Result<Vec<SubscriptionRow>, String>| {
                                    if let Ok(rows) = result {
                                        subs.set(rows);
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
}
