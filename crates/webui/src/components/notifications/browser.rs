//! Browser subscription. Wasm calls the Push API. Host builds keep stubs.

use dioxus::prelude::*;

#[cfg(target_arch = "wasm32")]
use super::model;
use super::model::BrowserBits;

pub(super) fn live_bits(subscribed: bool) -> BrowserBits {
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

pub(super) fn begin_enable(
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

pub(super) async fn existing_endpoint() -> Option<String> {
    #[cfg(target_arch = "wasm32")]
    {
        current_subscription().await.map(|sub| sub.endpoint)
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        None
    }
}

pub(super) async fn drop_browser_subscription() -> Result<(), String> {
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
        ios: model::is_ios(&ua),
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
    let bytes = model::decode_vapid_public_key(public_key)?;
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
    let body = model::subscribe_body(
        &sub.endpoint,
        &sub.p256dh,
        &sub.auth,
        model::device_label(&ua),
    );
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
    Err(model::error_text(status.as_u16(), &text))
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
