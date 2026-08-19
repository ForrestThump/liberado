//! Incognito mode — a chat that lives in the daemon's RAM and is discarded when you leave it.
//!
//! # Why RAM and not write-then-delete
//!
//! The other design was to persist normally and delete the log on the way out. It was rejected
//! because it **fails open**: every crash, `kill -9`, power cut or network drop between the write
//! and the delete leaves the transcript on disk permanently, and those are exactly the moments a
//! privacy feature is judged on. A mode that is private only when nothing goes wrong is worse than
//! no mode at all, because it is trusted.
//!
//! RAM-only fails *closed*. The daemon side is a single check in `SessionStore::path_for`, through
//! which every durable write in that store already funnels, so there is no second code path to keep
//! honest. It also makes the chat invisible to `GET /api/conversations/search` for free — that
//! endpoint greps the session logs directly, and there is no file to match.
//!
//! # What incognito does not cover
//!
//! The transcript, not the consequences. If the agent calls a tool that writes a vault note or a
//! memory during an incognito chat, that write is as real as any other — the tool has no idea which
//! session called it, and teaching every tool about incognito is a much larger promise than this
//! button makes. The label says the conversation is not saved, which is the true and narrow claim.
//!
//! # "Loses focus"
//!
//! Read as *leaving the conversation* — switching chats, turning the mode off, or closing the tab —
//! not as the browser window losing focus. Alt-tabbing to read something and coming back to a
//! destroyed chat would make the mode unusable for the reading-and-comparing that people actually
//! open a private chat to do.

use dioxus::prelude::*;

/// The path the daemon refuses to serve for anything but an incognito session.
///
/// Teardown happens on its own, from an effect, with nobody watching — so it must not be able to
/// delete a saved conversation even if this code hands it the wrong id. It once did exactly that.
/// With the guard, the worst a bug of that shape can do is leave a private session behind for the
/// idle sweeper.
pub const DISCARD_QUERY: &str = "?ephemeral_only=true";

/// The URL that discards an incognito session, carrying the guard that makes the daemon refuse to
/// touch anything else.
fn discard_url(api_base: &str, id: &str) -> String {
    format!("{api_base}/api/conversations/{id}{DISCARD_QUERY}")
}

/// Discard a session we are walking away from.
///
/// Failures are swallowed on purpose: nothing useful can be done about them here, a 404 already
/// means the outcome we wanted, and a 409 means the guard above just saved us.
pub async fn discard(api_base: String, id: String) {
    let _ = reqwest::Client::new()
        .delete(discard_url(&api_base, &id))
        .send()
        .await;
}

// The live incognito session, mirrored out of the Dioxus signal.
//
// `pagehide` fires outside any reactive scope and cannot read a signal, so the id is kept here as
// well. One writer (`remember`/`forget`, both called from the chat), one reader.
#[cfg(target_arch = "wasm32")]
thread_local! {
    static GHOST: std::cell::RefCell<Option<(String, String)>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(target_arch = "wasm32")]
pub fn remember(api_base: &str, id: &str) {
    GHOST.with(|cell| {
        *cell.borrow_mut() = Some((api_base.to_string(), id.to_string()));
    });
}

#[cfg(not(target_arch = "wasm32"))]
pub fn remember(_api_base: &str, _id: &str) {}

#[cfg(target_arch = "wasm32")]
pub fn forget() {
    GHOST.with(|cell| *cell.borrow_mut() = None);
}

#[cfg(not(target_arch = "wasm32"))]
pub fn forget() {}

/// Register the one `pagehide` handler that discards the live incognito session as the tab goes
/// away. Idempotent — safe to call from an effect that may re-run.
///
/// `pagehide` rather than `beforeunload`: it also fires when a mobile browser backgrounds the page
/// into the back/forward cache, which is the common way a phone "closes" a tab.
///
/// The request is a `keepalive` fetch, the one kind the browser promises to finish after the
/// document is gone. It is still best-effort — the daemon's periodic sweep is what covers the cases
/// where this never runs at all (a killed browser, a dead network, a closed laptop).
#[cfg(target_arch = "wasm32")]
pub fn install_unload_discard() {
    use std::sync::atomic::{AtomicBool, Ordering};
    use wasm_bindgen::JsCast;
    use wasm_bindgen::closure::Closure;

    static INSTALLED: AtomicBool = AtomicBool::new(false);
    if INSTALLED.swap(true, Ordering::Relaxed) {
        return;
    }
    let Some(window) = web_sys::window() else {
        return;
    };

    let handler = Closure::<dyn FnMut(web_sys::Event)>::new(move |_e: web_sys::Event| {
        let Some((base, id)) = GHOST.with(|cell| cell.borrow().clone()) else {
            return;
        };
        let Some(window) = web_sys::window() else {
            return;
        };
        let opts = web_sys::RequestInit::new();
        opts.set_method("DELETE");
        // `keepalive` has no web-sys binding (checked against 0.3.103 — `RequestInit` has setters
        // for every other member but this one), so it is set as a plain property. `RequestInit` is
        // a JS dictionary object, so this is the same thing the missing setter would do. Without it
        // the browser cancels the request as the document tears down and nothing is discarded.
        let _ = js_sys::Reflect::set(
            &opts,
            &wasm_bindgen::JsValue::from_str("keepalive"),
            &wasm_bindgen::JsValue::TRUE,
        );
        let url = discard_url(&base, &id);
        if let Ok(req) = web_sys::Request::new_with_str_and_init(&url, &opts) {
            // The promise is deliberately dropped: the document is going away and there is nobody
            // left to tell. `keepalive` is what makes the request outlive us, not the awaiting.
            let _ = window.fetch_with_request(&req);
        }
    });
    let _ = window.add_event_listener_with_callback("pagehide", handler.as_ref().unchecked_ref());
    handler.forget();
}

#[cfg(not(target_arch = "wasm32"))]
pub fn install_unload_discard() {}

/// The header toggle. Reads as pressed while on, and says plainly what the mode is and is not.
#[component]
pub fn IncognitoToggle(on: Signal<bool>) -> Element {
    let mut on = on;
    let cls = if on() {
        "incognito-btn active"
    } else {
        "incognito-btn"
    };
    let hint = if on() {
        "Incognito on — this chat is not saved and is discarded when you leave it. Tool actions still apply."
    } else {
        "Incognito: start a chat that is never written to disk"
    };

    rsx! {
        button {
            class: "{cls}",
            r#type: "button",
            title: "{hint}",
            "aria-pressed": if on() { "true" } else { "false" },
            onclick: move |_| {
                let now = on();
                on.set(!now);
            },
            span { class: "incognito-glyph", "\u{1F576}" }
            span { class: "incognito-label", "Incognito" }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::discard_url;

    /// The safety property the whole feature hangs on: the teardown request names the guard query,
    /// so even a wrong id cannot delete a saved conversation. Asserted on the URL because that is
    /// exactly what the daemon sees.
    #[test]
    fn discard_url_carries_the_ephemeral_only_guard() {
        let url = discard_url("http://d", "01HZABC");
        assert_eq!(
            url,
            "http://d/api/conversations/01HZABC?ephemeral_only=true"
        );
    }
}
