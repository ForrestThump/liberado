//! Derive a short push subscription label from User-Agent hints (host-testable).

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DeviceLabelHints {
    pub user_agent: String,
    pub platform: String,
    pub brands: Vec<String>,
    pub model: String,
    pub is_brave: bool,
}

pub fn derive_device_label(hints: &DeviceLabelHints) -> String {
    let browser = browser_name(hints);
    let device = device_name(hints);
    if device.is_empty() {
        browser
    } else if browser.is_empty() {
        device
    } else {
        format!("{device} · {browser}")
    }
}

fn browser_name(hints: &DeviceLabelHints) -> String {
    if hints.is_brave {
        return "Brave".into();
    }
    for brand in &hints.brands {
        let lower = brand.to_lowercase();
        if lower.contains("microsoft edge") || lower == "edge" {
            return "Edge".into();
        }
        if lower.contains("chrome") && !lower.contains("chromium") {
            return "Chrome".into();
        }
        if lower.contains("firefox") {
            return "Firefox".into();
        }
        if lower.contains("safari") && !lower.contains("chrome") {
            return "Safari".into();
        }
    }
    let ua = hints.user_agent.to_lowercase();
    if ua.contains("edg/") || ua.contains(" edg/") {
        return "Edge".into();
    }
    if ua.contains("firefox/") {
        return "Firefox".into();
    }
    if ua.contains("chrome/") && !ua.contains("chromium") {
        return "Chrome".into();
    }
    if ua.contains("safari/") && !ua.contains("chrome/") && !ua.contains("chromium") {
        return "Safari".into();
    }
    String::new()
}

fn device_name(hints: &DeviceLabelHints) -> String {
    let model = hints.model.trim();
    if !model.is_empty() {
        return model.to_string();
    }
    let ua = hints.user_agent.to_lowercase();
    let platform = hints.platform.to_lowercase();
    if ua.contains("iphone") || platform == "iphone" {
        return "iPhone".into();
    }
    if ua.contains("ipad") || platform == "ipad" {
        return "iPad".into();
    }
    if ua.contains("android") {
        return "Android".into();
    }
    if platform.contains("win") || ua.contains("windows") {
        return "Windows".into();
    }
    if platform == "macintel" || ua.contains("macintosh") || ua.contains("mac os x") {
        return "Mac".into();
    }
    if platform.contains("linux") || ua.contains("linux") {
        return "Linux".into();
    }
    String::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hints(
        ua: &str,
        platform: &str,
        brands: &[&str],
        model: &str,
        brave: bool,
    ) -> DeviceLabelHints {
        DeviceLabelHints {
            user_agent: ua.to_string(),
            platform: platform.to_string(),
            brands: brands.iter().map(|s| (*s).to_string()).collect(),
            model: model.to_string(),
            is_brave: brave,
        }
    }

    #[test]
    fn pixel_chrome_from_model_and_brands() {
        let label = derive_device_label(&hints(
            "Mozilla/5.0 (Linux; Android 14; Pixel 9) AppleWebKit/537.36 Chrome/120.0.0.0 Mobile Safari/537.36",
            "Linux",
            &["Google Chrome", "Chromium"],
            "Pixel 9",
            false,
        ));
        assert_eq!(label, "Pixel 9 · Chrome");
    }

    #[test]
    fn iphone_safari() {
        let label = derive_device_label(&hints(
            "Mozilla/5.0 (iPhone; CPU iPhone OS 17_0 like Mac OS X) AppleWebKit/605.1.15 Version/17.0 Mobile/15E148 Safari/604.1",
            "iPhone",
            &[],
            "",
            false,
        ));
        assert_eq!(label, "iPhone · Safari");
    }

    #[test]
    fn windows_edge() {
        let label = derive_device_label(&hints(
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 Chrome/120.0.0.0 Safari/537.36 Edg/120.0.0.0",
            "Win32",
            &["Microsoft Edge", "Chromium"],
            "",
            false,
        ));
        assert_eq!(label, "Windows · Edge");
    }

    #[test]
    fn linux_chrome() {
        let label = derive_device_label(&hints(
            "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 Chrome/120.0.0.0 Safari/537.36",
            "Linux",
            &["Google Chrome"],
            "",
            false,
        ));
        assert_eq!(label, "Linux · Chrome");
    }

    #[test]
    fn brave_overrides_chrome_brand() {
        let label = derive_device_label(&hints(
            "Mozilla/5.0 (Linux; Android 14) Chrome/120.0.0.0 Mobile Safari/537.36",
            "Linux",
            &["Google Chrome"],
            "Pixel 9",
            true,
        ));
        assert_eq!(label, "Pixel 9 · Brave");
    }
}
