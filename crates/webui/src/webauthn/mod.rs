#[cfg(target_arch = "wasm32")]
mod bindgen;

pub fn map_webauthn_error_name(name: &str, fallback_message: &str) -> String {
    match name {
        "NotAllowedError" => "Passkey request was cancelled or timed out".to_string(),
        "InvalidStateError" => "This passkey is already registered on this device".to_string(),
        "SecurityError" => {
            "Passkeys need a secure connection and a matching site address".to_string()
        }
        _ => fallback_message.to_string(),
    }
}

#[cfg(target_arch = "wasm32")]
pub fn map_webauthn_error(value: &wasm_bindgen::JsValue) -> String {
    let name = js_sys::Reflect::get(value, &wasm_bindgen::JsValue::from_str("name"))
        .ok()
        .and_then(|v| v.as_string())
        .unwrap_or_default();
    let message = js_sys::Reflect::get(value, &wasm_bindgen::JsValue::from_str("message"))
        .ok()
        .and_then(|v| v.as_string())
        .unwrap_or_else(|| "Passkey operation failed".to_string());
    map_webauthn_error_name(&name, &message)
}

#[cfg(target_arch = "wasm32")]
pub async fn create_passkey(options_json: &str) -> Result<String, String> {
    use bindgen::create_passkey_raw;

    create_passkey_raw(options_json)
        .await
        .map(|v| v.as_string().unwrap_or_default())
        .map_err(|e| map_webauthn_error(&e))
}

#[cfg(not(target_arch = "wasm32"))]
pub async fn create_passkey(_options_json: &str) -> Result<String, String> {
    Err("Create a passkey from the installed WebUI in a browser.".into())
}

#[cfg(target_arch = "wasm32")]
pub async fn get_passkey(options_json: &str) -> Result<String, String> {
    use bindgen::get_passkey_raw;

    get_passkey_raw(options_json)
        .await
        .map(|v| v.as_string().unwrap_or_default())
        .map_err(|e| map_webauthn_error(&e))
}

#[cfg(not(target_arch = "wasm32"))]
pub async fn get_passkey(_options_json: &str) -> Result<String, String> {
    Err("Sign in with a passkey from the installed WebUI in a browser.".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_not_allowed_to_plain_message() {
        let msg = map_webauthn_error_name("NotAllowedError", "x");
        assert!(msg.contains("cancelled"));
    }
}

#[cfg(test)]
mod webauthn_js_tests {
    use std::path::PathBuf;

    fn webauthn_js_path() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("js/webauthn.js")
    }

    #[test]
    fn webauthn_js_passes_node_syntax_check() {
        let js = std::fs::read_to_string(webauthn_js_path()).expect("webauthn.js");
        let status = std::process::Command::new("node")
            .arg("--check")
            .arg(webauthn_js_path())
            .status()
            .expect("node must be installed to validate webauthn.js syntax");
        assert!(status.success(), "{js}");
    }
}
