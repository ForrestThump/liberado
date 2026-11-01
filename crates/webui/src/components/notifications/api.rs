//! HTTP and browser calls for the Notifications page.

#[cfg(target_arch = "wasm32")]
use super::model::parse_device_state;
use super::model::{
    DeleteSubscriptionOutcome, DevicePushState, PushStatus, PushTestOutcome, ReconcileAction,
    SubscriptionRow, api_error_message, classify_delete_subscription_status,
    classify_push_test_response, parse_status, parse_subscriptions, re_register_failure_message,
    reconcile_action,
};
#[cfg(target_arch = "wasm32")]
use crate::device_label::derive_device_label;

pub(super) struct RefreshBundle {
    pub status: PushStatus,
    pub subs: Vec<SubscriptionRow>,
    pub device: DevicePushState,
    pub reconcile_error: Option<String>,
}

pub(super) async fn fetch_status(api_base: &str) -> Result<PushStatus, String> {
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

pub(super) async fn fetch_subscriptions(api_base: &str) -> Result<Vec<SubscriptionRow>, String> {
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

pub(super) async fn refresh_all(
    api_base: &str,
) -> Result<(PushStatus, Vec<SubscriptionRow>, DevicePushState), String> {
    let status = fetch_status(api_base).await?;
    let subs = fetch_subscriptions(api_base).await.unwrap_or_default();
    let device = fetch_device_push_state().await.unwrap_or_default();
    Ok((status, subs, device))
}

#[cfg(target_arch = "wasm32")]
async fn reregister_browser_subscription(api_base: &str) -> Result<(), String> {
    use crate::push::{fetch_device_label_hints, reregister_existing_push_on_server};

    let hints = fetch_device_label_hints().await.unwrap_or_default();
    let label = derive_device_label(&hints);
    reregister_existing_push_on_server(api_base, &label).await
}

#[cfg(not(target_arch = "wasm32"))]
async fn reregister_browser_subscription(_api_base: &str) -> Result<(), String> {
    Ok(())
}

pub(super) async fn refresh_and_reconcile(api_base: &str) -> Result<RefreshBundle, String> {
    let (status, subs, device) = refresh_all(api_base).await?;
    if reconcile_action(&device, status.vapid_configured, &subs)
        != ReconcileAction::ReRegisterOnServer
    {
        return Ok(RefreshBundle {
            status,
            subs,
            device,
            reconcile_error: None,
        });
    }

    match reregister_browser_subscription(api_base).await {
        Ok(()) => {
            let (status, subs, device) = refresh_all(api_base).await?;
            Ok(RefreshBundle {
                status,
                subs,
                device,
                reconcile_error: None,
            })
        }
        Err(_) => Ok(RefreshBundle {
            status,
            subs,
            device,
            reconcile_error: Some(re_register_failure_message().into()),
        }),
    }
}

#[cfg(target_arch = "wasm32")]
pub(super) async fn enable_push(api_base: &str) -> Result<(), String> {
    use crate::push::{enable_push_subscribe, fetch_device_label_hints, js_value_to_error_text};

    let hints = fetch_device_label_hints().await.unwrap_or_default();
    let label = derive_device_label(&hints);

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
    enable_push_subscribe(public_key, api_base, &label, "")
        .await
        .map_err(|e| js_value_to_error_text(&e))
        .map(|_| ())
}

#[cfg(not(target_arch = "wasm32"))]
pub(super) async fn enable_push(_api_base: &str) -> Result<(), String> {
    Err("Enable push from the installed WebUI in a browser.".into())
}

enum PostPushTestResult {
    Queued,
    NotSubscribed,
    Failed(String),
}

async fn post_push_test(api_base: &str, endpoint: &str) -> PostPushTestResult {
    let url = format!("{api_base}/api/push/test");
    let resp =
        match crate::http::post_json(&url, &serde_json::json!({ "endpoint": endpoint })).await {
            Ok(resp) => resp,
            Err(e) => return PostPushTestResult::Failed(format!("Failed to reach daemon: {e}")),
        };
    let status = resp.status().as_u16();
    let body = resp.text().await.unwrap_or_default();
    match classify_push_test_response(status, &body) {
        PushTestOutcome::Queued => PostPushTestResult::Queued,
        PushTestOutcome::NotSubscribed => PostPushTestResult::NotSubscribed,
        PushTestOutcome::OtherFailure => {
            PostPushTestResult::Failed(api_error_message(status, &body))
        }
    }
}

pub(super) async fn send_test(api_base: &str, endpoint: &str) -> Result<(), String> {
    if endpoint.is_empty() {
        return Err("this device is not subscribed — turn push on first".into());
    }

    match post_push_test(api_base, endpoint).await {
        PostPushTestResult::Queued => return Ok(()),
        PostPushTestResult::Failed(msg) => return Err(msg),
        PostPushTestResult::NotSubscribed => {}
    }

    reregister_browser_subscription(api_base).await?;

    let (_, _subs, device) = refresh_all(api_base).await?;
    let endpoint = device.subscription_endpoint;
    if endpoint.is_empty() {
        return Err(re_register_failure_message().into());
    }

    match post_push_test(api_base, &endpoint).await {
        PostPushTestResult::Queued => Ok(()),
        PostPushTestResult::NotSubscribed | PostPushTestResult::Failed(_) => {
            Err(re_register_failure_message().into())
        }
    }
}

pub(super) async fn remove_subscription(api_base: &str, endpoint: &str) -> Result<(), String> {
    let url = format!("{api_base}/api/push/subscriptions");
    let resp = crate::http::delete_json(&url, &serde_json::json!({ "endpoint": endpoint }))
        .await
        .map_err(|e| format!("Failed to reach daemon: {e}"))?;
    let status = resp.status().as_u16();
    match classify_delete_subscription_status(status) {
        DeleteSubscriptionOutcome::Removed | DeleteSubscriptionOutcome::AlreadyAbsent => Ok(()),
        DeleteSubscriptionOutcome::Failed => {
            let body = resp.text().await.unwrap_or_default();
            Err(api_error_message(status, &body))
        }
    }
}

#[cfg(target_arch = "wasm32")]
pub(super) async fn turn_off_push(api_base: &str) -> Result<(), String> {
    use crate::push::disable_push_local;

    let endpoint = disable_push_local().await?;
    if endpoint.is_empty() {
        return Ok(());
    }
    remove_subscription(api_base, &endpoint).await
}

#[cfg(not(target_arch = "wasm32"))]
pub(super) async fn turn_off_push(_api_base: &str) -> Result<(), String> {
    Err("Turn off push from the installed WebUI in a browser.".into())
}
