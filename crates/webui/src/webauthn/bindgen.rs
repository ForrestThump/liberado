//! wasm-bindgen imports from `js/webauthn.js`.

use wasm_bindgen::prelude::*;

#[wasm_bindgen(module = "/js/webauthn.js")]
extern "C" {
    #[wasm_bindgen(catch, js_name = createPasskey)]
    pub async fn create_passkey_raw(options_json: &str) -> Result<JsValue, JsValue>;

    #[wasm_bindgen(catch, js_name = getPasskey)]
    pub async fn get_passkey_raw(options_json: &str) -> Result<JsValue, JsValue>;
}
