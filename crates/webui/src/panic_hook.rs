//! Console + on-page banner when the WASM app panics or hits an uncaught wasm-bindgen error.

#[cfg(target_arch = "wasm32")]
pub fn install() {
    install_global_wasm_error_banner();
    std::panic::set_hook(Box::new(|info| {
        let message = info.to_string();
        web_sys::console::error_1(&format!("liberado-webui panic: {message}").into());
        show_reload_banner(&message);
        report_client_error(&message);
    }));
}

#[cfg(not(target_arch = "wasm32"))]
pub fn install() {}

#[cfg(target_arch = "wasm32")]
fn install_global_wasm_error_banner() {
    use std::sync::atomic::{AtomicBool, Ordering};
    use wasm_bindgen::JsCast;
    use wasm_bindgen::closure::Closure;

    static INSTALLED: AtomicBool = AtomicBool::new(false);
    if INSTALLED.swap(true, Ordering::Relaxed) {
        return;
    }
    let Some(window) = web_sys::window() else {
        return;
    };
    let handler = Closure::<dyn FnMut(web_sys::Event)>::new(move |event: web_sys::Event| {
        let Some(error) = event.dyn_ref::<web_sys::ErrorEvent>() else {
            return;
        };
        let message = error.message();
        let file = error.filename();
        if is_wasm_bindgen_fatal(&message, &file) {
            web_sys::console::error_1(&format!("liberado-webui error: {message}").into());
            show_reload_banner(&format!("{message} ({file})"));
            report_client_error(&message);
        }
    });
    let _ = window.add_event_listener_with_callback("error", handler.as_ref().unchecked_ref());
    handler.forget();
}

#[cfg(target_arch = "wasm32")]
fn is_wasm_bindgen_fatal(message: &str, filename: &str) -> bool {
    filename.ends_with(".wasm")
        || filename.contains("wasm-bindgen")
        || message.contains("wasm-bindgen")
        || message.contains("RuntimeError")
        || message.contains("unreachable")
}

#[cfg(target_arch = "wasm32")]
fn show_reload_banner(message: &str) {
    let Some(window) = web_sys::window() else {
        return;
    };
    let Some(document) = window.document() else {
        return;
    };
    if document
        .get_element_by_id("liberado-panic-banner")
        .is_some()
    {
        return;
    }
    let Some(body) = document.body() else {
        return;
    };
    let Ok(banner) = document.create_element("div") else {
        return;
    };
    let _ = banner.set_id("liberado-panic-banner");
    let _ = banner.set_class_name("panic-banner");
    let safe = html_escape(message);
    let _ = banner.set_inner_html(&format!(
        "<p class=\"panic-banner-title\">Something broke — Reload</p>\
         <button type=\"button\" class=\"btn-primary panic-banner-reload\" \
         onclick=\"location.reload()\">Reload</button>\
         <pre class=\"panic-banner-detail\">{safe}</pre>"
    ));
    let _ = body.append_child(&banner);
}

#[cfg(target_arch = "wasm32")]
fn html_escape(raw: &str) -> String {
    raw.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(target_arch = "wasm32")]
fn csrf_token_from_cookie() -> Option<String> {
    use wasm_bindgen::JsCast;
    let window = web_sys::window()?;
    let doc = window
        .document()?
        .dyn_into::<web_sys::HtmlDocument>()
        .ok()?;
    let cookie = doc.cookie().ok()?;
    for part in cookie.split(';') {
        let part = part.trim();
        if let Some(value) = part.strip_prefix("liberado_csrf=") {
            if !value.is_empty() {
                return Some(value.to_string());
            }
        }
    }
    None
}

#[cfg(target_arch = "wasm32")]
fn report_client_error(message: &str) {
    let Some(window) = web_sys::window() else {
        return;
    };
    let base = window.location().origin().unwrap_or_default();
    if base.is_empty() {
        return;
    }
    let truncated: String = message.chars().take(500).collect();
    let body = serde_json::json!({ "message": truncated }).to_string();
    let url = format!("{base}/api/client-errors");
    let opts = web_sys::RequestInit::new();
    opts.set_method("POST");
    opts.set_credentials(web_sys::RequestCredentials::Include);
    opts.set_body(&wasm_bindgen::JsValue::from_str(&body));
    let _ = js_sys::Reflect::set(
        &opts,
        &wasm_bindgen::JsValue::from_str("keepalive"),
        &wasm_bindgen::JsValue::TRUE,
    );
    if let Ok(headers) = web_sys::Headers::new() {
        let _ = headers.set("content-type", "application/json");
        if let Some(csrf) = csrf_token_from_cookie() {
            let _ = headers.set(crate::http::CSRF_HEADER, &csrf);
        }
        opts.set_headers(&headers);
    }
    if let Ok(req) = web_sys::Request::new_with_str_and_init(&url, &opts) {
        let _ = window.fetch_with_request(&req);
    }
}
