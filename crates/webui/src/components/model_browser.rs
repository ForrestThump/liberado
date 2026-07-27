//! The model browser — the picker `/model` promises ("type to filter, Enter to switch, Esc to
//! close") and the web UI did not have, so the command printed that sentence and nothing opened.
//!
//! Reads `GET /api/models` and switches with `POST /api/models/select`, which hot-swaps the
//! daemon's active model without a restart. The catalog is a provider-wide list (hundreds of ids
//! from OpenRouter), so filtering is not a nicety — it is the only way to reach anything.

use dioxus::prelude::*;

use chat_client_contract::ModelsResponse;

/// How many filtered rows to render. The full catalog is long enough that building every row on
/// each keystroke is wasted work — nobody scrolls past the first screen of an already-filtered
/// list, they type more instead.
const MAX_ROWS: usize = 50;

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

/// Id used to grab the filter box after render. Focus is taken imperatively rather than with the
/// `autofocus` attribute: measured against the live app, neither `autofocus` nor an `onmounted`
/// `set_focus` moved focus off the chat textarea, so every keystroke, arrow and Esc went to the
/// chat box and the picker's whole keyboard contract was dead. Same `get_element_by_id` idiom the
/// chat input's auto-grow already uses.
const FILTER_INPUT_ID: &str = "model-filter-input";

#[cfg(target_arch = "wasm32")]
fn focus_filter_input() {
    use wasm_bindgen::JsCast;

    let Some(el) = web_sys::window()
        .and_then(|w| w.document())
        .and_then(|d| d.get_element_by_id(FILTER_INPUT_ID))
        .and_then(|el| el.dyn_into::<web_sys::HtmlElement>().ok())
    else {
        return;
    };
    let _ = el.focus();
}

#[cfg(not(target_arch = "wasm32"))]
fn focus_filter_input() {}

/// Case-insensitive substring match over model ids, in catalog order.
fn filtered(models: &[String], query: &str) -> Vec<String> {
    let q = query.trim().to_lowercase();
    models
        .iter()
        .filter(|m| q.is_empty() || m.to_lowercase().contains(&q))
        .take(MAX_ROWS)
        .cloned()
        .collect()
}

#[component]
pub fn ModelBrowser(
    api_base: String,
    open: Signal<bool>,
    /// Reports back so the chat can record the switch as a system message — the same place every
    /// other slash-command outcome is narrated.
    on_switched: EventHandler<String>,
) -> Element {
    let mut open = open;
    let mut query = use_signal(String::new);
    let mut highlighted = use_signal(|| 0usize);
    let mut busy = use_signal(|| false);
    let mut error = use_signal(|| None::<String>);

    // Runs after the panel is in the DOM, so the element exists to focus.
    use_effect(focus_filter_input);

    let catalog = use_resource({
        let base = api_base.clone();
        move || fetch_models(base.clone())
    });

    // `use_callback` (not a plain closure) so the handle is `Copy` and can be used from the backdrop
    // click, the Escape key, and the switch path without "closure moved twice" — the same reason
    // `chat.rs::submit` is one.
    let close = use_callback(move |_: ()| {
        open.set(false);
        query.set(String::new());
        highlighted.set(0);
        error.set(None);
    });

    let (models, current, load_error) = match &*catalog.read() {
        Some(Ok(resp)) => (resp.models.clone(), resp.current.clone(), None),
        Some(Err(e)) => (Vec::new(), None, Some(e.clone())),
        None => (Vec::new(), None, None),
    };
    let rows = filtered(&models, query.read().as_str());
    let loading = catalog.read().is_none();

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
                        query.set(String::new());
                        highlighted.set(0);
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

    let rows_for_keys = rows.clone();
    let on_key = move |e: Event<KeyboardData>| match e.key() {
        Key::Escape => {
            e.prevent_default();
            close.call(());
        }
        Key::Enter => {
            e.prevent_default();
            if let Some(model) = rows_for_keys.get(highlighted()) {
                switch_to.call(model.clone());
            }
        }
        Key::ArrowDown => {
            e.prevent_default();
            let max = rows_for_keys.len().saturating_sub(1);
            highlighted.set((highlighted() + 1).min(max));
        }
        Key::ArrowUp => {
            e.prevent_default();
            highlighted.set(highlighted().saturating_sub(1));
        }
        _ => {}
    };

    rsx! {
        div {
            class: "modal-backdrop",
            // Clicking the backdrop dismisses; clicks inside the panel must not bubble out to it.
            onclick: move |_| close.call(()),
            div {
                class: "model-browser",
                onclick: move |e| e.stop_propagation(),
                div {
                    class: "model-browser-header",
                    span { class: "model-browser-title", "Switch model" }
                    if let Some(cur) = current.clone() {
                        span { class: "model-browser-current", "current: {cur}" }
                    }
                }
                input {
                    id: FILTER_INPUT_ID,
                    class: "model-browser-input",
                    r#type: "text",
                    placeholder: "Type to filter\u{2026}  (Enter to switch, Esc to close)",
                    value: "{query}",
                    oninput: move |e| {
                        query.set(e.value());
                        highlighted.set(0);
                    },
                    onkeydown: on_key,
                }
                if let Some(err) = error() {
                    p { class: "model-browser-error", "{err}" }
                }
                if let Some(err) = load_error {
                    p { class: "model-browser-error", "Could not load models: {err}" }
                } else if loading {
                    p { class: "model-browser-empty", "Loading models\u{2026}" }
                } else if rows.is_empty() {
                    p { class: "model-browser-empty", "No model matches that filter." }
                } else {
                    div {
                        class: "model-list",
                        for (i, model) in rows.iter().enumerate() {
                            {
                                let is_current = current.as_deref() == Some(model.as_str());
                                let cls = if i == highlighted() { "model-row active" } else { "model-row" };
                                let pick = model.clone();
                                rsx! {
                                    button {
                                        key: "{model}",
                                        class: "{cls}",
                                        r#type: "button",
                                        disabled: busy(),
                                        onmouseenter: move |_| highlighted.set(i),
                                        onclick: move |_| switch_to.call(pick.clone()),
                                        span { class: "model-row-name", "{model}" }
                                        if is_current {
                                            span { class: "model-row-badge", "active" }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                if busy() {
                    p { class: "model-browser-empty", "Switching\u{2026}" }
                }
            }
        }
    }
}
