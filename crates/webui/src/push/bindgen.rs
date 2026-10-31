//! wasm-bindgen imports from the crate-root `js/push.js` module (`catch` on every import).

use wasm_bindgen::prelude::*;

#[wasm_bindgen(module = "/js/push.js")]
extern "C" {
    #[wasm_bindgen(catch, js_name = enablePushSubscribe)]
    pub async fn enable_push_subscribe(
        vapid_key: &str,
        api_base: &str,
        label: &str,
        csrf_token: &str,
    ) -> Result<JsValue, JsValue>;

    #[wasm_bindgen(catch, js_name = fetchPushDeviceStateJson)]
    pub async fn fetch_push_device_state_json_raw() -> Result<JsValue, JsValue>;
}
