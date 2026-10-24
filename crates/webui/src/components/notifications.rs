//! Weekly spending notifications on the Automations page.
//!
//! The permission request runs in the click handler, before any await.
//! iOS only grants it for an installed home-screen app (iOS 16.4+).

use base64::Engine;
use dioxus::prelude::*;
use serde::Deserialize;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PushUi {
    Unsupported,
    NeedsHomeScreen,
    ServerOff,
    Denied,
    Subscribed,
    Ready,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BrowserBits {
    pub push_supported: bool,
    pub ios: bool,
    pub standalone: bool,
    pub permission_denied: bool,
    pub subscribed: bool,
}

impl BrowserBits {
    pub fn unsupported() -> Self {
        Self {
            push_supported: false,
            ios: false,
            standalone: false,
            permission_denied: false,
            subscribed: false,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServerPushStatus {
    pub vapid_configured: bool,
    pub reminder_enabled: bool,
    pub public_key: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ViewModel {
    pub sentence: String,
    pub reminder: String,
    pub ui: PushUi,
    pub public_key: String,
}

pub fn device_label(ua: &str) -> &'static str {
    if is_ios(ua) {
        return "iPhone";
    }
    if ua.to_ascii_lowercase().contains("android") {
        return "Android";
    }
    "browser"
}

fn is_ios(ua: &str) -> bool {
    let ua = ua.to_ascii_lowercase();
    ua.contains("iphone") || ua.contains("ipad") || ua.contains("ipod")
}

pub fn decode_vapid_public_key(text: &str) -> Result<Vec<u8>, String> {
    let raw = text.trim().trim_end_matches('=');
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(raw)
        .map_err(|_| "VAPID public key is not base64url".to_string())?;
    require_p256(&bytes)
}

fn require_p256(bytes: &[u8]) -> Result<Vec<u8>, String> {
    if bytes.len() == 65 && bytes.first() == Some(&0x04) {
        return Ok(bytes.to_vec());
    }
    Err("VAPID public key must be a 65-byte P-256 point".into())
}

pub fn parse_status(body: &str) -> Result<ServerPushStatus, String> {
    let parsed: StatusWire = serde_json::from_str(body).map_err(|_| "Bad response".to_string())?;
    Ok(ServerPushStatus {
        vapid_configured: parsed.vapid_configured,
        reminder_enabled: parsed.reminder_enabled,
        public_key: parsed.public_key.unwrap_or_default(),
    })
}

#[derive(Deserialize)]
struct StatusWire {
    vapid_configured: bool,
    reminder_enabled: bool,
    #[serde(default)]
    public_key: Option<String>,
}

pub fn view_model(
    server: Option<Result<&ServerPushStatus, &str>>,
    bits: &BrowserBits,
) -> ViewModel {
    let Some(server) = server else {
        return checking();
    };
    match server {
        Ok(status) => from_status(status, bits),
        Err(_) => failed_load(),
    }
}

fn checking() -> ViewModel {
    ViewModel {
        sentence: "Checking notifications...".into(),
        reminder: String::new(),
        ui: PushUi::Unsupported,
        public_key: String::new(),
    }
}

fn failed_load() -> ViewModel {
    ViewModel {
        sentence: "Could not read notification status.".into(),
        reminder: String::new(),
        ui: PushUi::Unsupported,
        public_key: String::new(),
    }
}

fn from_status(status: &ServerPushStatus, bits: &BrowserBits) -> ViewModel {
    let ui = push_ui(bits, status.vapid_configured);
    ViewModel {
        sentence: status_sentence(ui).to_string(),
        reminder: reminder_sentence(status.reminder_enabled).to_string(),
        ui,
        public_key: status.public_key.clone(),
    }
}

fn push_ui(bits: &BrowserBits, server_configured: bool) -> PushUi {
    if let Some(early) = early_block(bits) {
        return early;
    }
    later_block(bits, server_configured)
}

fn early_block(bits: &BrowserBits) -> Option<PushUi> {
    if !bits.push_supported {
        return Some(PushUi::Unsupported);
    }
    if bits.ios && !bits.standalone {
        return Some(PushUi::NeedsHomeScreen);
    }
    None
}

fn later_block(bits: &BrowserBits, server_configured: bool) -> PushUi {
    if !server_configured {
        return PushUi::ServerOff;
    }
    if bits.permission_denied {
        return PushUi::Denied;
    }
    if bits.subscribed {
        return PushUi::Subscribed;
    }
    PushUi::Ready
}

pub fn status_sentence(ui: PushUi) -> &'static str {
    match ui {
        PushUi::Unsupported => "This browser cannot show notifications.",
        PushUi::NeedsHomeScreen => {
            "On iPhone, add Liberado to the Home Screen, open it from the icon, then enable notifications."
        }
        PushUi::ServerOff => "This server has no Web Push keys. Notifications stay off.",
        other => later_sentence(other),
    }
}

fn later_sentence(ui: PushUi) -> &'static str {
    match ui {
        PushUi::Denied => "Notifications are blocked for this site.",
        PushUi::Subscribed => "This phone is subscribed.",
        _ => "Notifications are available.",
    }
}

pub fn reminder_sentence(enabled: bool) -> &'static str {
    if enabled {
        "The weekly reminder is on."
    } else {
        "The weekly reminder is off in config."
    }
}

pub fn subscribe_body(endpoint: &str, p256dh: &str, auth: &str, label: &str) -> serde_json::Value {
    serde_json::json!({
        "endpoint": endpoint,
        "keys": {"p256dh": p256dh, "auth": auth},
        "label": label,
    })
}

fn show_enable(model: &ViewModel) -> bool {
    model.ui == PushUi::Ready && !model.public_key.is_empty()
}

fn show_subscribed(model: &ViewModel) -> bool {
    model.ui == PushUi::Subscribed
}

#[component]
pub fn NotificationsCard(api_base: String) -> Element {
    let status = use_resource({
        let base = api_base.clone();
        move || {
            let base = base.clone();
            async move { fetch_status(&base).await }
        }
    });
    let endpoint = use_signal(String::new);
    let error = use_signal(|| None::<String>);
    let busy = use_signal(|| false);
    use_hook({
        let mut endpoint = endpoint;
        move || {
            spawn(async move {
                if let Some(found) = existing_endpoint().await {
                    endpoint.set(found);
                }
            });
        }
    });
    let bits = live_bits(!endpoint().is_empty());
    let model = match status.read().as_ref() {
        None => view_model(None, &bits),
        Some(Ok(server)) => view_model(Some(Ok(server)), &bits),
        Some(Err(err)) => view_model(Some(Err(err)), &bits),
    };
    let enable_base = api_base.clone();
    let unsub_base = api_base.clone();
    let test_base = api_base.clone();
    let enable_key = model.public_key.clone();

    rsx! {
        section {
            class: "push-card",
            h2 { class: "push-card-title", "Notifications" }
            p { class: "push-card-note", "{model.sentence}" }
            if !model.reminder.is_empty() {
                p { class: "push-card-note", "{model.reminder}" }
            }
            if let Some(message) = error() {
                p { class: "push-card-note", "{message}" }
            }
            div {
                class: "push-card-actions",
                if show_enable(&model) {
                    button {
                        class: "approval-option",
                        r#type: "button",
                        disabled: busy(),
                        onclick: move |_| {
                            begin_enable(
                                enable_base.clone(),
                                enable_key.clone(),
                                endpoint,
                                error,
                                busy,
                            );
                        },
                        "Enable notifications"
                    }
                }
                if show_subscribed(&model) {
                    button {
                        class: "approval-option",
                        r#type: "button",
                        disabled: busy(),
                        onclick: move |_| {
                            begin_unsubscribe(unsub_base.clone(), endpoint, error, busy)
                        },
                        "Unsubscribe"
                    }
                    button {
                        class: "approval-option",
                        r#type: "button",
                        disabled: busy(),
                        onclick: move |_| begin_test(test_base.clone(), endpoint, error, busy),
                        "Send test notification"
                    }
                }
            }
        }
    }
}

fn live_bits(subscribed: bool) -> BrowserBits {
    #[cfg(target_arch = "wasm32")]
    {
        wasm_bits(subscribed)
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let mut bits = BrowserBits::unsupported();
        bits.subscribed = subscribed;
        bits
    }
}

async fn fetch_status(api_base: &str) -> Result<ServerPushStatus, String> {
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
    parse_status(&text)
}

fn begin_enable(
    api_base: String,
    public_key: String,
    endpoint: Signal<String>,
    error: Signal<Option<String>>,
    busy: Signal<bool>,
) {
    #[cfg(target_arch = "wasm32")]
    {
        begin_enable_wasm(api_base, public_key, endpoint, error, busy);
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = (api_base, public_key, endpoint, error, busy);
    }
}

fn begin_unsubscribe(
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

fn begin_test(
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
    drop_browser_subscription().await?;
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
    Err(error_text(status.as_u16(), &text))
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

fn error_text(status: u16, body: &str) -> String {
    let parsed: serde_json::Value = serde_json::from_str(body).unwrap_or(serde_json::Value::Null);
    parsed["error"]
        .as_str()
        .map(str::to_string)
        .unwrap_or_else(|| format!("Notification request failed ({status})"))
}

async fn existing_endpoint() -> Option<String> {
    #[cfg(target_arch = "wasm32")]
    {
        current_subscription().await.map(|sub| sub.endpoint)
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        None
    }
}

async fn drop_browser_subscription() -> Result<(), String> {
    #[cfg(target_arch = "wasm32")]
    {
        unsubscribe_browser().await
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        Ok(())
    }
}

#[cfg(target_arch = "wasm32")]
fn begin_enable_wasm(
    api_base: String,
    public_key: String,
    mut endpoint: Signal<String>,
    mut error: Signal<Option<String>>,
    mut busy: Signal<bool>,
) {
    let promise = match web_sys::Notification::request_permission() {
        Ok(promise) => promise,
        Err(_) => {
            error.set(Some("The browser refused the permission request.".into()));
            return;
        }
    };
    spawn(async move {
        busy.set(true);
        let result = finish_enable(&api_base, &public_key, promise).await;
        busy.set(false);
        match result {
            Ok(saved) => {
                endpoint.set(saved);
                error.set(None);
            }
            Err(err) => error.set(Some(err)),
        }
    });
}

#[cfg(target_arch = "wasm32")]
async fn finish_enable(
    api_base: &str,
    public_key: &str,
    promise: js_sys::Promise,
) -> Result<String, String> {
    let granted = permission_granted(promise).await?;
    if !granted {
        return Err("Notifications are blocked for this site.".into());
    }
    let sub = subscribe_browser(public_key).await?;
    post_subscription(api_base, &sub).await?;
    Ok(sub.endpoint)
}

#[cfg(target_arch = "wasm32")]
async fn permission_granted(promise: js_sys::Promise) -> Result<bool, String> {
    let value = wasm_bindgen_futures::JsFuture::from(promise)
        .await
        .map_err(|_| "The permission request failed.".to_string())?;
    Ok(value.as_string().as_deref() == Some("granted"))
}

#[cfg(target_arch = "wasm32")]
struct BrowserSub {
    endpoint: String,
    p256dh: String,
    auth: String,
}

#[cfg(target_arch = "wasm32")]
fn wasm_bits(subscribed: bool) -> BrowserBits {
    let Some(window) = web_sys::window() else {
        return BrowserBits::unsupported();
    };
    let ua = window.navigator().user_agent().unwrap_or_default();
    BrowserBits {
        push_supported: push_supported(&window),
        ios: is_ios(&ua),
        standalone: standalone(&window),
        permission_denied: permission_denied(),
        subscribed,
    }
}

#[cfg(target_arch = "wasm32")]
fn push_supported(window: &web_sys::Window) -> bool {
    let nav = window.navigator();
    has_prop(&nav, "serviceWorker")
        && has_prop(window, "PushManager")
        && has_prop(window, "Notification")
}

#[cfg(target_arch = "wasm32")]
fn has_prop(target: &impl AsRef<wasm_bindgen::JsValue>, name: &str) -> bool {
    js_sys::Reflect::has(target.as_ref(), &name.into()).unwrap_or(false)
}

#[cfg(target_arch = "wasm32")]
fn standalone(window: &web_sys::Window) -> bool {
    let media = window
        .match_media("(display-mode: standalone)")
        .ok()
        .flatten()
        .is_some_and(|list| list.matches());
    media || navigator_standalone(&window.navigator())
}

#[cfg(target_arch = "wasm32")]
fn navigator_standalone(nav: &web_sys::Navigator) -> bool {
    js_sys::Reflect::get(nav.as_ref(), &"standalone".into())
        .ok()
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
}

#[cfg(target_arch = "wasm32")]
fn permission_denied() -> bool {
    web_sys::Notification::permission() == web_sys::NotificationPermission::Denied
}

#[cfg(target_arch = "wasm32")]
async fn current_subscription() -> Option<BrowserSub> {
    let manager = push_manager().await.ok()?;
    let value = wasm_bindgen_futures::JsFuture::from(manager.get_subscription().ok()?)
        .await
        .ok()?;
    if value.is_null() || value.is_undefined() {
        return None;
    }
    subscription_fields(&value).ok()
}

#[cfg(target_arch = "wasm32")]
async fn subscribe_browser(public_key: &str) -> Result<BrowserSub, String> {
    let manager = push_manager().await?;
    let options = subscribe_options(public_key)?;
    let promise = manager
        .subscribe_with_options(&options)
        .map_err(|_| "Could not subscribe".to_string())?;
    let value = wasm_bindgen_futures::JsFuture::from(promise)
        .await
        .map_err(|_| "Could not subscribe".to_string())?;
    subscription_fields(&value)
}

#[cfg(target_arch = "wasm32")]
fn subscribe_options(public_key: &str) -> Result<web_sys::PushSubscriptionOptionsInit, String> {
    let bytes = decode_vapid_public_key(public_key)?;
    let array = js_sys::Uint8Array::from(bytes.as_slice());
    let options = web_sys::PushSubscriptionOptionsInit::new();
    options.set_user_visible_only(true);
    options.set_application_server_key_opt_u8_array(Some(&array));
    Ok(options)
}

#[cfg(target_arch = "wasm32")]
fn subscription_fields(value: &wasm_bindgen::JsValue) -> Result<BrowserSub, String> {
    use wasm_bindgen::JsCast;
    let sub: web_sys::PushSubscription = value
        .clone()
        .dyn_into()
        .map_err(|_| "The browser did not return a subscription.".to_string())?;
    let json = sub
        .to_json()
        .map_err(|_| "The subscription could not be read.".to_string())?;
    let keys = json
        .get_keys()
        .ok_or_else(|| "The subscription has no keys.".to_string())?;
    Ok(BrowserSub {
        endpoint: json
            .get_endpoint()
            .ok_or_else(|| "The subscription has no endpoint.".to_string())?,
        p256dh: keys
            .get_p256dh()
            .ok_or_else(|| "The subscription has no key.".to_string())?,
        auth: keys
            .get_auth()
            .ok_or_else(|| "The subscription has no auth secret.".to_string())?,
    })
}

#[cfg(target_arch = "wasm32")]
async fn push_manager() -> Result<web_sys::PushManager, String> {
    use wasm_bindgen::JsCast;
    let window = web_sys::window().ok_or_else(|| "No browser window.".to_string())?;
    let ready = window
        .navigator()
        .service_worker()
        .ready()
        .map_err(|_| "The service worker is not ready.".to_string())?;
    let value = wasm_bindgen_futures::JsFuture::from(ready)
        .await
        .map_err(|_| "The service worker is not ready.".to_string())?;
    let registration: web_sys::ServiceWorkerRegistration = value
        .dyn_into()
        .map_err(|_| "The service worker is not ready.".to_string())?;
    registration
        .push_manager()
        .map_err(|_| "This browser cannot show notifications.".to_string())
}

#[cfg(target_arch = "wasm32")]
async fn post_subscription(api_base: &str, sub: &BrowserSub) -> Result<(), String> {
    let ua = web_sys::window()
        .and_then(|window| window.navigator().user_agent().ok())
        .unwrap_or_default();
    let body = subscribe_body(&sub.endpoint, &sub.p256dh, &sub.auth, device_label(&ua));
    let url = format!("{api_base}/api/push/subscriptions");
    let response = reqwest::Client::new()
        .post(url)
        .json(&body)
        .send()
        .await
        .map_err(|_| "Failed to reach daemon".to_string())?;
    let status = response.status();
    if status.is_success() {
        return Ok(());
    }
    let text = response.text().await.unwrap_or_default();
    Err(error_text(status.as_u16(), &text))
}

#[cfg(target_arch = "wasm32")]
async fn unsubscribe_browser() -> Result<(), String> {
    let Some(manager) = push_manager().await.ok() else {
        return Ok(());
    };
    let Ok(promise) = manager.get_subscription() else {
        return Ok(());
    };
    let Ok(value) = wasm_bindgen_futures::JsFuture::from(promise).await else {
        return Ok(());
    };
    if value.is_null() || value.is_undefined() {
        return Ok(());
    }
    use wasm_bindgen::JsCast;
    let Ok(sub) = value.dyn_into::<web_sys::PushSubscription>() else {
        return Ok(());
    };
    let Ok(promise) = sub.unsubscribe() else {
        return Ok(());
    };
    let _ = wasm_bindgen_futures::JsFuture::from(promise).await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bits(
        push: bool,
        ios: bool,
        standalone: bool,
        denied: bool,
        subscribed: bool,
    ) -> BrowserBits {
        BrowserBits {
            push_supported: push,
            ios,
            standalone,
            permission_denied: denied,
            subscribed,
        }
    }

    fn server(on: bool, key: &str) -> ServerPushStatus {
        ServerPushStatus {
            vapid_configured: !key.is_empty(),
            reminder_enabled: on,
            public_key: key.to_string(),
        }
    }

    #[test]
    fn status_order_matches_the_phone_cases() {
        let off = server(false, "");
        let on = server(true, "key");
        assert_eq!(
            view_model(Some(Ok(&off)), &bits(false, false, false, false, false)).ui,
            PushUi::Unsupported
        );
        assert_eq!(
            status_sentence(PushUi::NeedsHomeScreen),
            "On iPhone, add Liberado to the Home Screen, open it from the icon, then enable notifications."
        );
        assert_eq!(
            view_model(Some(Ok(&on)), &bits(true, true, false, false, false)).ui,
            PushUi::NeedsHomeScreen
        );
        assert_eq!(
            view_model(Some(Ok(&off)), &bits(true, false, true, false, false)).sentence,
            "This server has no Web Push keys. Notifications stay off."
        );
        assert_eq!(
            view_model(Some(Ok(&on)), &bits(true, false, true, true, false)).ui,
            PushUi::Denied
        );
        assert_eq!(
            view_model(Some(Ok(&on)), &bits(true, true, true, false, true)).ui,
            PushUi::Subscribed
        );
        assert_eq!(
            view_model(Some(Ok(&on)), &bits(true, false, false, false, false)).ui,
            PushUi::Ready
        );
        assert_eq!(
            view_model(Some(Ok(&on)), &BrowserBits::unsupported()).reminder,
            "The weekly reminder is on."
        );
        assert_eq!(
            reminder_sentence(false),
            "The weekly reminder is off in config."
        );
    }

    #[test]
    fn device_labels_follow_the_user_agent() {
        assert_eq!(device_label("Mozilla iPhone"), "iPhone");
        assert_eq!(device_label("iPad"), "iPhone");
        assert_eq!(device_label("Android 14"), "Android");
        assert_eq!(device_label("Windows"), "browser");
    }

    #[test]
    fn the_rfc_public_key_decodes_and_garbage_does_not() {
        let key = "BP4z9KsN6nGRTbVYI_c7VJSPQTBtkgcy27mlmlMoZIIgDll6e3vCYLocInmYWAmS6TlzAC8wEqKK6PBru3jl7A8";
        let bytes = decode_vapid_public_key(key).expect("rfc key");
        assert_eq!(bytes.len(), 65);
        assert_eq!(bytes[0], 0x04);
        let err = decode_vapid_public_key("aaaa").expect_err("short");
        assert!(!err.contains("aaaa"));
    }

    #[test]
    fn status_json_keeps_the_public_key_and_the_switch() {
        let parsed = parse_status(
            r#"{"vapid_configured":true,"reminder_enabled":false,"subscription_count":1,"public_key":"abc"}"#,
        )
        .expect("json");
        assert!(parsed.vapid_configured);
        assert!(!parsed.reminder_enabled);
        assert_eq!(parsed.public_key, "abc");
        let body = subscribe_body(
            "https://push.example.test/fake-subscription",
            "p256",
            "auth",
            "iPhone",
        );
        assert_eq!(body["label"], "iPhone");
        assert_eq!(body["keys"]["auth"], "auth");
    }
}
