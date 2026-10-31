//! Push notification view models and display helpers.

use serde::Deserialize;

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
pub(super) struct PushStatus {
    #[serde(default)]
    pub vapid_configured: bool,
    #[serde(default)]
    pub subscription_count: u32,
    #[serde(default)]
    pub public_key: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
pub(super) struct DevicePushState {
    #[serde(default)]
    pub notification_permission: String,
    #[serde(default)]
    pub service_worker_state: String,
    #[serde(default)]
    pub page_controlled: bool,
    #[serde(default)]
    pub browser_subscribed: bool,
    #[serde(default)]
    pub subscription_host: String,
    #[serde(default)]
    pub subscription_endpoint: String,
    #[serde(default)]
    pub is_ios: bool,
    #[serde(default)]
    pub display_standalone: bool,
    #[serde(default)]
    pub push_available: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub(super) struct SubscriptionRow {
    pub id: i64,
    pub label: String,
    pub host: String,
    pub endpoint: String,
    #[serde(default)]
    pub created_at: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum DevicePushStatus {
    NotSupported,
    NotConfigured,
    Blocked,
    On,
    Off,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum StatusLine {
    Error(String),
    Success(String),
}

pub(super) fn parse_status(body: &str) -> Result<PushStatus, String> {
    serde_json::from_str(body).map_err(|e| format!("Bad push status: {e}"))
}

pub(super) fn parse_subscriptions(body: &str) -> Result<Vec<SubscriptionRow>, String> {
    serde_json::from_str(body).map_err(|e| format!("Bad subscriptions: {e}"))
}

pub(super) fn parse_device_state(body: &str) -> Result<DevicePushState, String> {
    serde_json::from_str(body).map_err(|e| format!("Bad device push state: {e}"))
}

pub(super) fn device_push_status(
    device: &DevicePushState,
    vapid_configured: bool,
) -> DevicePushStatus {
    if !device.push_available || device.notification_permission == "unsupported" {
        return DevicePushStatus::NotSupported;
    }
    if !vapid_configured {
        return DevicePushStatus::NotConfigured;
    }
    if device.notification_permission == "denied" {
        return DevicePushStatus::Blocked;
    }
    if device.browser_subscribed && device.notification_permission == "granted" {
        return DevicePushStatus::On;
    }
    DevicePushStatus::Off
}

pub(super) fn status_pill_label(status: DevicePushStatus) -> &'static str {
    match status {
        DevicePushStatus::On => "On",
        DevicePushStatus::Off => "Off",
        DevicePushStatus::Blocked => "Blocked",
        DevicePushStatus::NotSupported => "Not supported",
        DevicePushStatus::NotConfigured => "Not configured",
    }
}

pub(super) fn status_pill_class(status: DevicePushStatus) -> &'static str {
    match status {
        DevicePushStatus::On => "lib-status-pill on",
        DevicePushStatus::Off => "lib-status-pill",
        DevicePushStatus::Blocked => "lib-status-pill blocked",
        DevicePushStatus::NotSupported => "lib-status-pill muted",
        DevicePushStatus::NotConfigured => "lib-status-pill muted",
    }
}

pub(super) fn api_error_message(status: u16, body: &str) -> String {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|value| {
            value
                .get("error")
                .and_then(|error| error.as_str())
                .map(str::to_owned)
        })
        .unwrap_or_else(|| format!("Request failed (HTTP {status})"))
}

pub(super) fn format_added_date(iso: &str) -> String {
    if iso.is_empty() {
        return String::new();
    }
    chrono::DateTime::parse_from_rfc3339(iso)
        .ok()
        .map(|dt| dt.format("%b %d, %Y").to_string())
        .unwrap_or_else(|| iso.to_string())
}

pub(super) fn troubleshooting_lines(
    device: &DevicePushState,
    status: &PushStatus,
) -> Vec<(String, String)> {
    let sw = if device.page_controlled {
        format!("{} (controlled)", device.service_worker_state)
    } else {
        format!("{} (not controlled)", device.service_worker_state)
    };
    let vapid = if status.vapid_configured {
        "yes".into()
    } else {
        "no".into()
    };
    let host = if device.subscription_host.is_empty() {
        "—".into()
    } else {
        device.subscription_host.clone()
    };
    vec![
        (
            "Notification permission".into(),
            device.notification_permission.clone(),
        ),
        ("Service worker".into(), sw),
        ("VAPID configured".into(), vapid),
        ("Push host".into(), host),
        (
            "Daemon subscriptions".into(),
            status.subscription_count.to_string(),
        ),
    ]
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
        let rows = parse_subscriptions(
            r#"[{"id":1,"label":"phone","host":"fcm.googleapis.com","endpoint":"https://fcm.googleapis.com/x","created_at":"2026-01-15T12:00:00Z"}]"#,
        )
        .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].label, "phone");
        assert_eq!(rows[0].created_at, "2026-01-15T12:00:00Z");
    }

    #[test]
    fn device_state_json_parses() {
        let row = parse_device_state(
            r#"{"notification_permission":"granted","service_worker_state":"active","page_controlled":true,"browser_subscribed":true,"subscription_host":"fcm.googleapis.com","subscription_endpoint":"https://fcm.googleapis.com/x","is_ios":false,"display_standalone":false,"push_available":true}"#,
        )
        .unwrap();
        assert_eq!(row.notification_permission, "granted");
        assert!(row.browser_subscribed);
        assert_eq!(row.subscription_endpoint, "https://fcm.googleapis.com/x");
    }

    #[test]
    fn device_push_status_matrix() {
        let mut device = DevicePushState::default();
        device.notification_permission = "unsupported".into();
        assert_eq!(
            device_push_status(&device, true),
            DevicePushStatus::NotSupported
        );

        device = DevicePushState::default();
        device.push_available = true;
        assert_eq!(
            device_push_status(&device, false),
            DevicePushStatus::NotConfigured
        );

        device.notification_permission = "denied".into();
        device.push_available = true;
        assert_eq!(device_push_status(&device, true), DevicePushStatus::Blocked);

        device.notification_permission = "granted".into();
        device.browser_subscribed = true;
        device.push_available = true;
        assert_eq!(device_push_status(&device, true), DevicePushStatus::On);

        device.browser_subscribed = false;
        device.push_available = true;
        assert_eq!(device_push_status(&device, true), DevicePushStatus::Off);
    }

    #[test]
    fn format_added_date_parses_rfc3339() {
        assert_eq!(format_added_date("2026-01-15T12:00:00Z"), "Jan 15, 2026");
    }
}
