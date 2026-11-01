//! Map browser push errors to text the Notifications page can show (host-testable).

pub fn format_error_name_message(name: &str, message: &str) -> String {
    match (name.is_empty(), message.is_empty()) {
        (true, true) => "unknown JavaScript error".into(),
        (true, false) => message.to_string(),
        (false, true) => name.to_string(),
        (false, false) => format!("{name}: {message}"),
    }
}

pub fn map_js_error_text(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return "unknown JavaScript error".into();
    }
    if let Some(friendly) = friendly_push_error(trimmed) {
        return friendly;
    }
    trimmed.to_string()
}

fn friendly_push_error(raw: &str) -> Option<String> {
    let lower = raw.to_lowercase();
    if raw.contains("NotAllowedError")
        || lower.contains("permission not granted")
        || lower.contains("permission") && lower.contains("denied")
    {
        return Some(
            "Notifications are blocked for this site or app. Allow notifications in the browser \
             site settings (lock icon → Site settings → Notifications), then try again."
                .into(),
        );
    }
    if raw.contains("AbortError")
        || lower.contains("push service not available")
        || lower.contains("registration failed")
    {
        return Some(
            "Push registration failed — the browser push service is unavailable. On Brave Android, \
             open brave://settings/privacy and turn ON \"Use Google services for push messaging\", \
             then reload and try again."
                .into(),
        );
    }
    if lower.contains("service worker not ready within 10 seconds") {
        return Some(
            "The service worker did not become ready within 10 seconds. Reload the installed app \
             and confirm /sw.js is registered at the site origin."
                .into(),
        );
    }
    if let Some(rest) = raw.strip_prefix("HTTP ") {
        if let Some((code, body)) = rest.split_once(": ") {
            return Some(format!("Subscribe failed (HTTP {code}): {body}"));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_message_format() {
        assert_eq!(
            format_error_name_message("AbortError", "Registration failed"),
            "AbortError: Registration failed"
        );
        assert_eq!(format_error_name_message("", "plain"), "plain");
        assert_eq!(
            format_error_name_message("NotAllowedError", ""),
            "NotAllowedError"
        );
    }

    #[test]
    fn not_allowed_maps_to_site_settings_hint() {
        let msg = map_js_error_text("NotAllowedError: Notification permission not granted: denied");
        assert!(msg.contains("blocked"));
        assert!(msg.contains("site settings"));
    }

    #[test]
    fn abort_and_brave_hint() {
        let msg = map_js_error_text("AbortError: Registration failed - push service not available");
        assert!(msg.contains("Brave"));
        assert!(msg.contains("Google services for push messaging"));
    }

    #[test]
    fn timeout_message_is_clear() {
        let raw = "Error: Service worker not ready within 10 seconds — reload the app";
        let msg = map_js_error_text(raw);
        assert!(msg.contains("10 seconds"));
    }

    #[test]
    fn http_error_includes_status_and_body() {
        let msg = map_js_error_text(r#"HTTP 403: {"error":"csrf"}"#);
        assert!(msg.contains("403"));
        assert!(msg.contains("csrf"));
    }

    #[test]
    fn plain_string_passes_through() {
        assert_eq!(map_js_error_text("custom failure"), "custom failure");
    }
}
