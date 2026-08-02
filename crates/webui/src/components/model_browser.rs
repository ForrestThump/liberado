//! The model browser behind `/model`.
//!
//! Data and action only: the filter box, keyboard handling and dismissal all live in
//! [`crate::components::picker::Picker`], shared with the theme browser.
//!
//! Reads `GET /api/models`. A pick applies to **this conversation only** — via
//! `POST /api/models/select` with its id, or, when the chat has not started yet, by riding the
//! request that creates it (`model` on `/api/chat/stream`). This picker never changes the daemon-wide
//! default; that is deliberately not something a chat surface should be able to do by accident.
//!
//! The catalog is a provider-wide list (hundreds of ids from OpenRouter), so filtering is not a
//! nicety — it is the only way to reach anything.

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

/// Bind the model to `conversation`.
///
/// Always scoped — this function is never called without an id. A chat that has not sent its first
/// message has none, and that case is handled by the caller carrying the pick onto the request that
/// creates the conversation, exactly as `/profile` does.
///
/// It used to fall back to the daemon-wide swap instead, on the reasoning that a conversation with
/// no history takes the daemon default anyway. That reasoning only looked at the chat being picked
/// *for* and ignored every other conversation on the daemon: the fallback silently retuned all of
/// them. Worse, it was not an edge case — new chat, pick a model, type is the ordinary way anyone
/// chooses one, so the common path was the broken one.
async fn select_model(
    api_base: String,
    model: String,
    conversation: String,
) -> Result<ModelsResponse, String> {
    let url = format!("{api_base}/api/models/select");
    let body = serde_json::json!({ "model": model, "conversation": conversation });
    let resp = reqwest::Client::new()
        .post(&url)
        .json(&body)
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
    /// The conversation to scope the choice to. `None` before a chat has sent anything, in which
    /// case the pick is handed straight back for the owner to carry — see [`select_model`].
    conversation: Option<String>,
    /// Reports the chosen model back. The owner narrates it *and*, when there was no conversation to
    /// scope to, is responsible for putting it on the next message — without that the pick is
    /// silently dropped.
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
        let scope = conversation.clone();
        use_callback(move |model: String| {
            if busy() {
                return;
            }
            // No conversation yet: accept the pick and let the owner carry it onto the request that
            // creates one. Same shape as `/profile`, and for the same reason — this is the turn the
            // choice most obviously means to apply to.
            let Some(scope) = scope.clone() else {
                on_switched.call(model);
                open.set(false);
                return;
            };
            busy.set(true);
            error.set(None);
            let base = base.clone();
            #[cfg(target_arch = "wasm32")]
            wasm_bindgen_futures::spawn_local(async move {
                match select_model(base, model.clone(), scope).await {
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
                let _ = (base, model, scope);
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
