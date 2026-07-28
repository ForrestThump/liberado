//! The model browser behind `/model`.
//!
//! Data and action only: the filter box, keyboard handling and dismissal all live in
//! [`crate::components::picker::Picker`], shared with the theme browser.
//!
//! Reads `GET /api/models` and switches with `POST /api/models/select`, which hot-swaps the daemon's
//! active model without a restart. The catalog is a provider-wide list (hundreds of ids from
//! OpenRouter), so filtering is not a nicety — it is the only way to reach anything.

use dioxus::prelude::*;

use chat_client_contract::ModelsResponse;

use crate::components::picker::Picker;

async fn fetch_models(api_base: String) -> Result<ModelsResponse, String> {
    let url = format!("{api_base}/api/models");
    reqwest::get(&url)
        .await
        .map_err(|e| format!("Failed to reach daemon: {e}"))?
        .json::<ModelsResponse>()
        .await
        .map_err(|e| format!("Bad response: {e}"))
}

async fn select_model(api_base: String, model: String) -> Result<ModelsResponse, String> {
    let url = format!("{api_base}/api/models/select");
    let resp = reqwest::Client::new()
        .post(&url)
        .json(&serde_json::json!({ "model": model }))
        .send()
        .await
        .map_err(|e| format!("Failed to reach daemon: {e}"))?;
    let body: ModelsResponse = resp
        .json()
        .await
        .map_err(|e| format!("Bad response: {e}"))?;
    // The daemon reports a refusal in the body's `error`, not only by status code.
    match body.error {
        Some(err) => Err(err),
        None => Ok(body),
    }
}

#[component]
pub fn ModelBrowser(
    api_base: String,
    open: Signal<bool>,
    /// Reports back so the chat can record the switch as a system message — the same place every
    /// other slash-command outcome is narrated.
    on_switched: EventHandler<String>,
) -> Element {
    // `mut` for the close-on-success below, which is wasm-only; on native that writer is cfg'd out.
    #[cfg_attr(not(target_arch = "wasm32"), allow(unused_mut))]
    let mut open = open;
    let mut busy = use_signal(|| false);
    let mut error = use_signal(|| None::<String>);

    let catalog = use_resource({
        let base = api_base.clone();
        move || fetch_models(base.clone())
    });

    let (models, current, load_error) = match &*catalog.read() {
        Some(Ok(resp)) => (resp.models.clone(), resp.current.clone(), None),
        Some(Err(e)) => (Vec::new(), None, Some(e.clone())),
        None => (Vec::new(), None, None),
    };
    let loading = catalog.read().is_none();

    // Unlike a theme switch, this is a network round trip: hold the picker open until the daemon
    // answers, so a refusal is shown rather than swallowed by a panel that already closed.
    let switch_to = {
        let base = api_base.clone();
        use_callback(move |model: String| {
            if busy() {
                return;
            }
            busy.set(true);
            error.set(None);
            let base = base.clone();
            #[cfg(target_arch = "wasm32")]
            wasm_bindgen_futures::spawn_local(async move {
                match select_model(base, model.clone()).await {
                    Ok(_) => {
                        busy.set(false);
                        on_switched.call(model);
                        open.set(false);
                    }
                    Err(e) => {
                        busy.set(false);
                        error.set(Some(e));
                    }
                }
            });
            #[cfg(not(target_arch = "wasm32"))]
            {
                let _ = (base, model);
                busy.set(false);
            }
        })
    };

    let status = if loading {
        Some("Loading models\u{2026}".to_string())
    } else if busy() {
        Some("Switching\u{2026}".to_string())
    } else {
        None
    };

    rsx! {
        Picker {
            title: "Switch model",
            current,
            items: models,
            status,
            error: error().or(load_error),
            open,
            on_pick: move |model: String| switch_to.call(model),
        }
    }
}
