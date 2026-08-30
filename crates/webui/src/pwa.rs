//! Pin the install-shell files Chrome and Safari check.
//!
//! The WASM UI is already a same-origin client of `/api/*`. Installability is a
//! handful of static files next to that shell: a custom `index.html`, a web app
//! manifest, two PNG icons, and a service worker whose fetch handler does not
//! intercept the daemon API (chat streams over EventSource).

use std::path::PathBuf;

use serde_json::Value;

fn crate_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read_crate_file(relative: &str) -> String {
    std::fs::read_to_string(crate_root().join(relative))
        .unwrap_or_else(|e| panic!("read {relative}: {e}"))
}

fn read_crate_bytes(relative: &str) -> Vec<u8> {
    std::fs::read(crate_root().join(relative)).unwrap_or_else(|e| panic!("read {relative}: {e}"))
}

/// Chromium installability: name, start_url, standalone display, 192 and 512 icons.
fn manifest_install_errors(text: &str) -> Vec<String> {
    let mut errors = Vec::new();
    let value: Value = match serde_json::from_str(text) {
        Ok(v) => v,
        Err(e) => return vec![format!("manifest is not JSON: {e}")],
    };
    let name = value
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    let short_name = value
        .get("short_name")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    if name.is_empty() && short_name.is_empty() {
        errors.push("manifest needs name or short_name".into());
    }
    if value
        .get("start_url")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .is_empty()
    {
        errors.push("manifest needs start_url".into());
    }
    let display = value.get("display").and_then(Value::as_str).unwrap_or("");
    if !matches!(display, "standalone" | "fullscreen" | "minimal-ui") {
        errors.push(format!("manifest display {display:?} is not installable"));
    }
    if value
        .get("prefer_related_applications")
        .and_then(Value::as_bool)
        == Some(true)
    {
        errors.push("prefer_related_applications must not be true".into());
    }
    let icons = value
        .get("icons")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if !icons_claim_size(&icons, "192x192") {
        errors.push("manifest missing 192x192 icon".into());
    }
    if !icons_claim_size(&icons, "512x512") {
        errors.push("manifest missing 512x512 icon".into());
    }
    errors
}

fn icons_claim_size(icons: &[Value], size: &str) -> bool {
    icons.iter().any(|icon| {
        icon.get("sizes")
            .and_then(Value::as_str)
            .unwrap_or("")
            .split_whitespace()
            .any(|s| s == size)
    })
}

/// The worker must own `/` (not `/assets/`) and must not intercept `/api/`.
fn service_worker_errors(text: &str) -> Vec<String> {
    let mut errors = Vec::new();
    if !text.contains("addEventListener(\"fetch\"") && !text.contains("addEventListener('fetch'") {
        errors.push("service worker has no fetch handler".into());
    }
    if !text.contains("pathname.startsWith(\"/api/\")")
        && !text.contains("pathname.startsWith('/api/')")
    {
        errors.push("service worker does not bypass /api/".into());
    }
    if text.contains("caches.") {
        errors.push("service worker caches responses; that can pin a stale WASM".into());
    }
    errors
}

fn index_html_errors(text: &str) -> Vec<String> {
    let mut errors = Vec::new();
    if !text.contains("rel=\"manifest\"") {
        errors.push("index.html does not link a web app manifest".into());
    }
    if !text.contains("href=\"/manifest.json\"") {
        errors.push("index.html must link /manifest.json at the origin root".into());
    }
    if !text.contains("serviceWorker.register(\"/sw.js\")") {
        errors.push("index.html must register /sw.js so the worker scope is the origin".into());
    }
    if !text.contains("id=\"main\"") {
        errors.push("index.html is missing div#main for the Dioxus mount".into());
    }
    if !text.contains("{app_title}") {
        errors.push("index.html is missing the {app_title} Dioxus placeholder".into());
    }
    if !text.contains("apple-mobile-web-app-capable") {
        errors.push("index.html is missing the iOS home-screen meta tag".into());
    }
    errors
}

/// PNG IHDR width and height. None if the bytes are not a PNG.
fn png_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.len() < 24 {
        return None;
    }
    if bytes.get(0..8)? != b"\x89PNG\r\n\x1a\n" {
        return None;
    }
    if bytes.get(12..16)? != b"IHDR" {
        return None;
    }
    let width = u32::from_be_bytes(bytes.get(16..20)?.try_into().ok()?);
    let height = u32::from_be_bytes(bytes.get(20..24)?.try_into().ok()?);
    Some((width, height))
}

fn public_file_exists(name: &str) -> bool {
    crate_root().join("public").join(name).is_file()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shipped_manifest_meets_chromium_install_fields() {
        let errors = manifest_install_errors(&read_crate_file("public/manifest.json"));
        assert!(errors.is_empty(), "{errors:?}");
    }

    #[test]
    fn shipped_service_worker_bypasses_the_daemon_api() {
        let errors = service_worker_errors(&read_crate_file("public/sw.js"));
        assert!(errors.is_empty(), "{errors:?}");
    }

    #[test]
    fn shipped_index_html_is_the_pwa_shell() {
        let errors = index_html_errors(&read_crate_file("index.html"));
        assert!(errors.is_empty(), "{errors:?}");
    }

    #[test]
    fn shipped_icons_are_the_claimed_png_sizes() {
        assert_eq!(
            png_dimensions(&read_crate_bytes("public/icon-192.png")),
            Some((192, 192))
        );
        assert_eq!(
            png_dimensions(&read_crate_bytes("public/icon-512.png")),
            Some((512, 512))
        );
        assert!(public_file_exists("icon-192.png"));
        assert!(public_file_exists("icon-512.png"));
    }

    #[test]
    fn dioxus_toml_does_not_nest_public_under_assets() {
        let toml = read_crate_file("Dioxus.toml");
        assert!(
            !toml.lines().any(|line| {
                let trimmed = line.trim();
                !trimmed.starts_with('#') && trimmed.contains("asset_dir")
            }),
            "asset_dir copies public/ to /assets/; sw.js must stay at /sw.js"
        );
    }

    #[test]
    fn empty_manifest_is_not_installable() {
        let errors = manifest_install_errors("{}");
        assert!(errors.iter().any(|e| e.contains("name")), "{errors:?}");
        assert!(errors.iter().any(|e| e.contains("start_url")), "{errors:?}");
        assert!(errors.iter().any(|e| e.contains("display")), "{errors:?}");
        assert!(errors.iter().any(|e| e.contains("192x192")), "{errors:?}");
        assert!(errors.iter().any(|e| e.contains("512x512")), "{errors:?}");
    }

    #[test]
    fn prefer_related_applications_blocks_install() {
        let text = r#"{
            "name": "x",
            "start_url": "/",
            "display": "standalone",
            "prefer_related_applications": true,
            "icons": [
                {"sizes": "192x192"},
                {"sizes": "512x512"}
            ]
        }"#;
        let errors = manifest_install_errors(text);
        assert!(
            errors
                .iter()
                .any(|e| e.contains("prefer_related_applications")),
            "{errors:?}"
        );
    }

    #[test]
    fn service_worker_without_fetch_handler_fails() {
        let errors = service_worker_errors("self.addEventListener('install', function () {});");
        assert!(
            errors.iter().any(|e| e.contains("fetch handler")),
            "{errors:?}"
        );
    }

    #[test]
    fn service_worker_that_caches_or_misses_api_bypass_fails() {
        let no_bypass = r#"self.addEventListener("fetch", function (event) {
            event.respondWith(fetch(event.request));
        });"#;
        let errors = service_worker_errors(no_bypass);
        assert!(errors.iter().any(|e| e.contains("/api/")), "{errors:?}");

        let caches = r#"self.addEventListener("fetch", function (event) {
            if (new URL(event.request.url).pathname.startsWith("/api/")) { return; }
            caches.open("v1");
        });"#;
        let errors = service_worker_errors(caches);
        assert!(errors.iter().any(|e| e.contains("caches")), "{errors:?}");
    }

    #[test]
    fn index_html_that_registers_an_assets_worker_fails() {
        let html = r#"<!DOCTYPE html><html><head>
            <title>{app_title}</title>
            <link rel="manifest" href="/manifest.json" />
            <meta name="apple-mobile-web-app-capable" content="yes" />
            <script>navigator.serviceWorker.register("/assets/sw.js");</script>
            </head><body><div id="main"></div></body></html>"#;
        let errors = index_html_errors(html);
        assert!(errors.iter().any(|e| e.contains("/sw.js")), "{errors:?}");
    }

    #[test]
    fn png_dimensions_rejects_non_png() {
        assert_eq!(png_dimensions(b"not a png"), None);
        assert_eq!(png_dimensions(b""), None);
    }
}
