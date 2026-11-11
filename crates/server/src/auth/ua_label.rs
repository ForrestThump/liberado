//! Derive a short passkey label from `User-Agent`.

pub fn passkey_name_from_user_agent(user_agent: &str) -> String {
    let ua = user_agent.trim();
    if ua.is_empty() {
        return "Unknown device".to_string();
    }
    let browser = detect_browser(ua);
    let os = detect_os(ua);
    format!("{os} · {browser}")
}

fn detect_browser(ua: &str) -> &'static str {
    if ua.contains("Edg/") {
        "Edge"
    } else if ua.contains("Chrome/") && !ua.contains("Chromium") {
        "Chrome"
    } else if ua.contains("Firefox/") {
        "Firefox"
    } else if ua.contains("Safari/") && !ua.contains("Chrome") {
        "Safari"
    } else {
        "Browser"
    }
}

fn detect_os(ua: &str) -> &'static str {
    if ua.contains("Android") {
        "Android"
    } else if ua.contains("iPhone") || ua.contains("iPad") {
        "iPhone"
    } else if ua.contains("Mac OS X") || ua.contains("Macintosh") {
        "Mac"
    } else if ua.contains("Windows") {
        "Windows"
    } else if ua.contains("Linux") {
        "Linux"
    } else {
        "Device"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn android_chrome_label() {
        let name = passkey_name_from_user_agent(
            "Mozilla/5.0 (Linux; Android 14) AppleWebKit/537.36 Chrome/120.0.0.0 Mobile",
        );
        assert_eq!(name, "Android · Chrome");
    }
}
