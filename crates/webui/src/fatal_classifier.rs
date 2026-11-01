//! Classify browser errors that mean the WASM app is broken (host-testable).

/// Whether an uncaught `error` or `unhandledrejection` should show the reload banner and POST
/// `/api/client-errors`.
///
/// `stack` is optional; glue failures often appear only on the rejection reason's stack.
pub fn wasm_bindgen_fatal_signal(message: &str, filename: &str, stack: Option<&str>) -> bool {
    if message.contains("wasm-bindgen")
        || message.contains("RuntimeError")
        || message.contains("unreachable")
    {
        return true;
    }
    if filename.ends_with(".wasm") || filename.contains("wasm-bindgen") {
        return true;
    }
    let glue = wasm_glue_origin(filename, stack);
    if glue && glue_failure_message(message) {
        return true;
    }
    false
}

fn glue_failure_message(message: &str) -> bool {
    message.contains("Cannot read properties")
        || message.contains("is not a function")
        || message.contains("is not defined")
        || message.starts_with("TypeError")
        || message.contains("TypeError:")
}

fn wasm_glue_origin(filename: &str, stack: Option<&str>) -> bool {
    if wasm_glue_script_path(filename) {
        return true;
    }
    stack.is_some_and(|text| {
        text.contains("__wbg_")
            || text.contains("wasm-bindgen")
            || text.contains("imported JS function")
    })
}

fn wasm_glue_script_path(filename: &str) -> bool {
    if !filename.ends_with(".js") {
        return false;
    }
    if filename.contains("/snippets/") {
        return true;
    }
    filename.contains("liberado-webui")
}

#[cfg(test)]
mod tests {
    use super::wasm_bindgen_fatal_signal;

    #[test]
    fn glue_typeerror_on_generated_bundle_is_fatal() {
        assert!(wasm_bindgen_fatal_signal(
            "Cannot read properties of undefined (reading 'then')",
            "http://127.0.0.1:8765/liberado-webui-abc123.js",
            None,
        ));
    }

    #[test]
    fn glue_typeerror_with_wbg_stack_is_fatal() {
        assert!(wasm_bindgen_fatal_signal(
            "Cannot read properties of undefined (reading 'then')",
            "http://127.0.0.1:8765/app.js",
            Some("    at __wbg_fetchPushDeviceStateJson (liberado-webui.js:42:10)"),
        ));
    }

    #[test]
    fn snippet_glue_path_is_fatal_for_glue_messages() {
        assert!(wasm_bindgen_fatal_signal(
            "Cannot read properties of undefined (reading 'call')",
            "http://127.0.0.1:8765/snippets/wasm-bindgen-rayon-123.js",
            None,
        ));
    }

    #[test]
    fn unrelated_third_party_script_is_not_fatal() {
        assert!(!wasm_bindgen_fatal_signal(
            "TypeError: Cannot set property 'x' of null",
            "https://cdn.example.com/analytics.js",
            None,
        ));
    }

    #[test]
    fn wasm_bindgen_console_prefix_in_message_is_fatal() {
        assert!(wasm_bindgen_fatal_signal(
            "wasm-bindgen: imported JS function that was not marked as catch threw an error",
            "http://127.0.0.1:8765/page.html",
            None,
        ));
    }

    #[test]
    fn wasm_module_filename_is_fatal() {
        assert!(wasm_bindgen_fatal_signal(
            "unreachable",
            "http://127.0.0.1:8765/liberado_webui_bg.wasm",
            None,
        ));
    }
}
