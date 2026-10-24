//! HTTP calls for notification status, unsubscribe, and the test push.

use dioxus::prelude::*;

use super::browser;
use super::model::{self, ServerPushStatus};

pub(super) async fn fetch_status(api_base: &str) -> Result<ServerPushStatus, String> {
    let url = format!("{api_base}/api/push/status");
    let response = reqwest::get(&url)
        .await
        .map_err(|_| "Failed to reach daemon".to_string())?;
    if !response.status().is_success() {
        return Err("Notification status is unavailable".into());
    }
    let text = response
        .text()
        .await
        .map_err(|_| "Bad response".to_string())?;
    model::parse_status(&text)
}

pub(super) fn begin_unsubscribe(
    api_base: String,
    mut endpoint: Signal<String>,
    mut error: Signal<Option<String>>,
    mut busy: Signal<bool>,
) {
    spawn(async move {
        busy.set(true);
        let result = unsubscribe_phone(&api_base, &endpoint()).await;
        busy.set(false);
        match result {
            Ok(()) => {
                endpoint.set(String::new());
                error.set(None);
            }
            Err(err) => error.set(Some(err)),
        }
    });
}

pub(super) fn begin_test(
    api_base: String,
    endpoint: Signal<String>,
    mut error: Signal<Option<String>>,
    mut busy: Signal<bool>,
) {
    spawn(async move {
        busy.set(true);
        let result = send_test(&api_base, &endpoint()).await;
        busy.set(false);
        error.set(result.err());
    });
}

async fn unsubscribe_phone(api_base: &str, endpoint: &str) -> Result<(), String> {
    browser::drop_browser_subscription().await?;
    if endpoint.is_empty() {
        return Ok(());
    }
    delete_subscription(api_base, endpoint).await
}

async fn send_test(api_base: &str, endpoint: &str) -> Result<(), String> {
    let body = if endpoint.is_empty() {
        serde_json::json!({})
    } else {
        serde_json::json!({ "endpoint": endpoint })
    };
    let url = format!("{api_base}/api/push/test");
    let response = reqwest::Client::new()
        .post(url)
        .json(&body)
        .send()
        .await
        .map_err(|_| "Failed to reach daemon".to_string())?;
    let status = response.status();
    let text = response.text().await.unwrap_or_default();
    if status.is_success() {
        return Ok(());
    }
    Err(model::error_text(status.as_u16(), &text))
}

async fn delete_subscription(api_base: &str, endpoint: &str) -> Result<(), String> {
    let url = format!("{api_base}/api/push/subscriptions");
    let response = reqwest::Client::new()
        .delete(url)
        .json(&serde_json::json!({ "endpoint": endpoint }))
        .send()
        .await
        .map_err(|_| "Failed to reach daemon".to_string())?;
    if response.status().is_success() {
        return Ok(());
    }
    Err("Could not remove this phone".into())
}
