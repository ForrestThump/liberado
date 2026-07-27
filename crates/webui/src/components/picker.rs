//! The searchable picker shell — filter box, keyboard navigation, dismissal, rows.
//!
//! Extracted from the model browser so `/model` and `/theme` are the same widget with different
//! data. Everything a picker does *as a picker* lives here; a caller supplies the list, says what is
//! currently active, and handles a pick. That split is what keeps the two commands from drifting
//! apart in feel, and it is why adding a third picker is a list and a callback rather than a
//! rewrite.
//!
//! Deliberately does **not** close itself when something is picked. `/theme` applies instantly and
//! wants to close; `/model` has to await an HTTP round trip and must stay open to report a failure.
//! The owner knows which it is; the shell does not.

use dioxus::prelude::*;

/// How many filtered rows to render. Long catalogs (the model list runs to hundreds) make building
/// every row on each keystroke wasted work — past the first screen people type more, not scroll.
const MAX_ROWS: usize = 50;

/// Id used to grab the filter box after render. Focus is taken imperatively rather than with the
/// `autofocus` attribute: measured against the live app, neither `autofocus` nor an `onmounted`
/// `set_focus` moved focus off the chat textarea, so every keystroke, arrow and Esc went to the
/// chat box and the picker's whole keyboard contract was dead. Same `get_element_by_id` idiom the
/// chat input's auto-grow uses.
const FILTER_INPUT_ID: &str = "picker-filter-input";

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

/// Case-insensitive substring match, preserving the caller's order.
fn filtered(items: &[String], query: &str) -> Vec<String> {
    let q = query.trim().to_lowercase();
    items
        .iter()
        .filter(|m| q.is_empty() || m.to_lowercase().contains(&q))
        .take(MAX_ROWS)
        .cloned()
        .collect()
}

#[component]
pub fn Picker(
    /// Heading, e.g. "Switch model".
    title: String,
    /// The active item, badged in the list and shown in the header. `None` when unknown.
    current: Option<String>,
    /// Everything selectable. Empty + `status` set reads as "still loading".
    items: Vec<String>,
    /// Transient line under the filter ("Loading models…", "Switching…"). `None` for nothing.
    status: Option<String>,
    /// Failure to show in place, keeping the picker open so it can be read.
    error: Option<String>,
    /// Cleared by Esc and by a backdrop click. The owner also clears it after a successful pick.
    open: Signal<bool>,
    on_pick: EventHandler<String>,
) -> Element {
    let mut open = open;
    let mut query = use_signal(String::new);
    let mut highlighted = use_signal(|| 0usize);

    // Runs after the panel is in the DOM, so the element exists to focus.
    use_effect(focus_filter_input);

    let close = use_callback(move |_: ()| {
        open.set(false);
        query.set(String::new());
        highlighted.set(0);
    });

    let rows = filtered(&items, query.read().as_str());
    let rows_for_keys = rows.clone();

    let on_key = move |e: Event<KeyboardData>| match e.key() {
        Key::Escape => {
            e.prevent_default();
            close.call(());
        }
        Key::Enter => {
            e.prevent_default();
            if let Some(item) = rows_for_keys.get(highlighted()) {
                on_pick.call(item.clone());
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
                class: "picker",
                onclick: move |e| e.stop_propagation(),
                div {
                    class: "picker-header",
                    span { class: "picker-title", "{title}" }
                    if let Some(cur) = current.clone() {
                        span { class: "picker-current", "current: {cur}" }
                    }
                }
                input {
                    id: FILTER_INPUT_ID,
                    class: "picker-input",
                    r#type: "text",
                    placeholder: "Type to filter\u{2026}  (Enter to select, Esc to close)",
                    value: "{query}",
                    oninput: move |e| {
                        query.set(e.value());
                        highlighted.set(0);
                    },
                    onkeydown: on_key,
                }
                if let Some(err) = error.clone() {
                    p { class: "picker-error", "{err}" }
                }
                if let Some(msg) = status.clone() {
                    p { class: "picker-empty", "{msg}" }
                } else if rows.is_empty() {
                    p { class: "picker-empty", "Nothing matches that filter." }
                }
                if !rows.is_empty() {
                    div {
                        class: "picker-list",
                        for (i, item) in rows.iter().enumerate() {
                            {
                                let is_current = current.as_deref() == Some(item.as_str());
                                let cls = if i == highlighted() { "picker-row active" } else { "picker-row" };
                                let pick = item.clone();
                                rsx! {
                                    button {
                                        key: "{item}",
                                        class: "{cls}",
                                        r#type: "button",
                                        // The pointer moves the same index the arrows do, so the two
                                        // never disagree about which row Enter would take.
                                        onmouseenter: move |_| highlighted.set(i),
                                        onclick: move |_| on_pick.call(pick.clone()),
                                        span { class: "picker-row-name", "{item}" }
                                        if is_current {
                                            span { class: "picker-row-badge", "active" }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}
