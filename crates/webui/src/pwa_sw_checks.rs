//! Static checks for the PWA service worker (fetch bypass, push, notification ack).

pub fn service_worker_errors(text: &str) -> Vec<String> {
    let mut errors = Vec::new();
    errors.extend(fetch_handler_errors(text));
    errors.extend(push_handler_errors(text));
    if text.contains("caches.") {
        errors.push("service worker caches responses; that can pin a stale WASM".into());
    }
    errors
}

fn fetch_handler_errors(text: &str) -> Vec<String> {
    let mut errors = Vec::new();
    if !text.contains("addEventListener(\"fetch\"") && !text.contains("addEventListener('fetch'") {
        errors.push("service worker has no fetch handler".into());
    }
    if !text.contains("pathname.startsWith(\"/api/\")")
        && !text.contains("pathname.startsWith('/api/')")
    {
        errors.push("service worker does not bypass /api/".into());
    }
    errors
}

fn push_handler_errors(text: &str) -> Vec<String> {
    let mut errors = Vec::new();
    if !text.contains("addEventListener(\"push\"") && !text.contains("addEventListener('push'") {
        errors.push("service worker must handle push".into());
    }
    if !text.contains("notificationclick") {
        errors.push("service worker must ack on notificationclick".into());
    }
    if !text.contains("showNotification") {
        errors.push("service worker must showNotification on push".into());
    }
    errors
}
