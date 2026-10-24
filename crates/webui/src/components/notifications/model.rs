//! Pure notification state. Host tests cover this file. No browser APIs.

use base64::Engine;
use serde::Deserialize;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum PushUi {
    Unsupported,
    NeedsHomeScreen,
    ServerOff,
    Denied,
    Subscribed,
    Ready,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct BrowserBits {
    pub push_supported: bool,
    pub ios: bool,
    pub standalone: bool,
    pub permission_denied: bool,
    pub subscribed: bool,
}

impl BrowserBits {
    pub(super) fn unsupported() -> Self {
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
pub(super) struct ServerPushStatus {
    pub vapid_configured: bool,
    pub reminder_enabled: bool,
    pub public_key: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ViewModel {
    pub sentence: String,
    pub reminder: String,
    pub ui: PushUi,
    pub public_key: String,
}

pub(super) fn device_label(ua: &str) -> &'static str {
    if is_ios(ua) {
        return "iPhone";
    }
    if ua.to_ascii_lowercase().contains("android") {
        return "Android";
    }
    "browser"
}

pub(super) fn is_ios(ua: &str) -> bool {
    let ua = ua.to_ascii_lowercase();
    ua.contains("iphone") || ua.contains("ipad") || ua.contains("ipod")
}

pub(super) fn decode_vapid_public_key(text: &str) -> Result<Vec<u8>, String> {
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

pub(super) fn parse_status(body: &str) -> Result<ServerPushStatus, String> {
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

pub(super) fn view_model(
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

pub(super) fn status_sentence(ui: PushUi) -> &'static str {
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

pub(super) fn reminder_sentence(enabled: bool) -> &'static str {
    if enabled {
        "The weekly reminder is on."
    } else {
        "The weekly reminder is off in config."
    }
}

pub(super) fn subscribe_body(
    endpoint: &str,
    p256dh: &str,
    auth: &str,
    label: &str,
) -> serde_json::Value {
    serde_json::json!({
        "endpoint": endpoint,
        "keys": {"p256dh": p256dh, "auth": auth},
        "label": label,
    })
}

pub(super) fn error_text(status: u16, body: &str) -> String {
    let parsed: serde_json::Value = serde_json::from_str(body).unwrap_or(serde_json::Value::Null);
    parsed["error"]
        .as_str()
        .map(str::to_string)
        .unwrap_or_else(|| format!("Notification request failed ({status})"))
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
