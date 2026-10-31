//! Web Push browser bindings and error mapping for the Notifications page.

mod errors;

#[cfg(target_arch = "wasm32")]
mod bindgen;

#[cfg(target_arch = "wasm32")]
pub use bindgen::enable_push_subscribe;

#[cfg(target_arch = "wasm32")]
pub async fn fetch_push_device_state_json() -> Result<String, wasm_bindgen::JsValue> {
    bindgen::fetch_push_device_state_json_raw()
        .await
        .and_then(|value| {
            value.as_string().ok_or_else(|| {
                wasm_bindgen::JsValue::from_str(
                    "fetchPushDeviceStateJson did not return a JSON string",
                )
            })
        })
}

#[cfg(target_arch = "wasm32")]
pub fn js_value_to_error_text(value: &wasm_bindgen::JsValue) -> String {
    use errors::{format_error_name_message, map_js_error_text};

    if let Some(text) = value.as_string() {
        return map_js_error_text(&text);
    }
    let name = reflect_string(value, "name");
    let message = reflect_string(value, "message");
    if !name.is_empty() || !message.is_empty() {
        return map_js_error_text(&format_error_name_message(&name, &message));
    }
    js_sys::JSON::stringify(value)
        .ok()
        .and_then(|json| json.as_string())
        .map(|json| map_js_error_text(&json))
        .unwrap_or_else(|| map_js_error_text("unknown JavaScript error"))
}

#[cfg(target_arch = "wasm32")]
fn reflect_string(value: &wasm_bindgen::JsValue, key: &str) -> String {
    js_sys::Reflect::get(value, &wasm_bindgen::JsValue::from_str(key))
        .ok()
        .and_then(|v| v.as_string())
        .unwrap_or_default()
}

#[cfg(test)]
mod push_js_tests {
    use std::path::PathBuf;
    use std::process::Command;

    fn push_js_path() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("js/push.js")
    }

    #[test]
    fn push_js_is_static_and_uses_safe_subscribe_shape() {
        let js = std::fs::read_to_string(push_js_path()).expect("push.js");
        assert!(js.contains("export async function enablePushSubscribe"));
        assert!(js.contains("applicationServerKey: urlBase64ToUint8Array(vapidKey)"));
        assert!(!js.contains("}})(publicKey);"));
        assert!(!js.contains("Function("));
    }

    #[test]
    fn push_js_parses_with_node_check() {
        let path = push_js_path();
        let output = Command::new("node")
            .arg("--check")
            .arg(&path)
            .output()
            .expect("node must be installed to validate push.js syntax");
        assert!(
            output.status.success(),
            "node --check failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn notifications_page_does_not_build_js_with_function_constructor() {
        let src = include_str!("../components/notifications.rs");
        assert!(
            !src.contains("Function::new_with_args"),
            "push subscribe must use static push.js, not dynamic JS strings"
        );
    }
}
