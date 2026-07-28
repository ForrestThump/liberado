//! Make the phone's Back gesture close the top UI layer instead of leaving the app.
//!
//! A swipe-back with a menu open should close the menu. In a single-page app it instead leaves the
//! page entirely, because as far as the browser is concerned nothing has happened since load — the
//! sidebar, the pickers and the Status view are all just state, and state is invisible to history.
//!
//! # How
//!
//! Give the browser something to go back *to*. Every open layer gets one pushed history entry, so
//! the entry count mirrors the layer count. A Back press pops one entry, we close one layer, and
//! when the last layer closes there are no entries left and the next Back really does leave — which
//! is the correct behaviour, not a fallback.
//!
//! The invariant is the whole design: **pushed entries == open layers**, maintained from both ends.
//! Opening a layer pushes; closing one by other means (Esc, a backdrop tap, picking something) calls
//! `history.back()` to retire its entry. Without that second half every non-Back dismissal would
//! leave a stale entry behind, and the user's next Back press would visibly do nothing.
//!
//! Our own `history.back()` calls also fire `popstate`, indistinguishable from a real one — the
//! event carries no hint of who caused it. Hence [`SELF_INFLICTED`], a count of pops we owe
//! ourselves. If it ever drifts the cost is one dead Back press, not a lost page.
//!
//! # Not handled: Forward
//!
//! Going Back and then Forward lands on a retired guard entry, and the counts drift by one. It costs
//! a dead Back press and self-corrects on the next open. Forward is close to nonexistent as a phone
//! gesture, and the bookkeeping to track direction is not worth that.

#[cfg(target_arch = "wasm32")]
use std::cell::Cell;

#[cfg(target_arch = "wasm32")]
thread_local! {
    /// History entries pushed to stand in for open layers.
    static PUSHED: Cell<usize> = const { Cell::new(0) };
    /// `popstate` events we caused with our own `history.back()`, still to be ignored.
    static SELF_INFLICTED: Cell<usize> = const { Cell::new(0) };
    static INSTALLED: Cell<bool> = const { Cell::new(false) };
}

/// Match the number of guard entries to `depth`, the number of layers currently open.
///
/// Call from an effect that reads the same state the layer count is derived from, so the two cannot
/// drift. Pushing and popping are both idempotent with respect to `depth`: calling this repeatedly
/// with an unchanged value does nothing.
#[cfg(target_arch = "wasm32")]
pub fn sync_depth(depth: usize) {
    let Some(history) = web_sys::window().and_then(|w| w.history().ok()) else {
        return;
    };
    let pushed = PUSHED.get();
    if depth > pushed {
        for _ in pushed..depth {
            // No URL: the address bar must not change, only the history depth.
            let _ = history.push_state_with_url(&wasm_bindgen::JsValue::NULL, "", None);
        }
    } else if depth < pushed {
        let owed = pushed - depth;
        SELF_INFLICTED.set(SELF_INFLICTED.get() + owed);
        for _ in 0..owed {
            let _ = history.back();
        }
    }
    PUSHED.set(depth);
}

#[cfg(not(target_arch = "wasm32"))]
pub fn sync_depth(_depth: usize) {}

/// Install the one `popstate` listener. `on_back` must close exactly one layer — the innermost.
///
/// Idempotent; later calls are ignored.
#[cfg(target_arch = "wasm32")]
pub fn install<F>(on_back: F)
where
    F: FnMut() + 'static,
{
    use wasm_bindgen::JsCast;
    use wasm_bindgen::closure::Closure;

    // `FnMut`, not `Fn`: closing a layer writes a Dioxus signal, which takes `&mut self`.
    let mut on_back = on_back;
    if INSTALLED.get() {
        return;
    }
    INSTALLED.set(true);
    let Some(window) = web_sys::window() else {
        return;
    };

    let handler = Closure::<dyn FnMut(web_sys::Event)>::new(move |_e: web_sys::Event| {
        let owed = SELF_INFLICTED.get();
        if owed > 0 {
            // We caused this one by retiring an entry for a layer that closed some other way.
            SELF_INFLICTED.set(owed - 1);
            return;
        }
        let pushed = PUSHED.get();
        if pushed == 0 {
            // Nothing of ours was on the stack, so this Back belongs to the browser. Letting it
            // through is the point: once every layer is closed, Back leaves the app.
            return;
        }
        // The browser already popped the entry; keep our count level with it *before* closing the
        // layer, or the effect that follows will read a stale depth and push a replacement.
        PUSHED.set(pushed - 1);
        on_back();
    });
    let _ = window.add_event_listener_with_callback("popstate", handler.as_ref().unchecked_ref());
    handler.forget();
}

#[cfg(not(target_arch = "wasm32"))]
pub fn install<F>(_on_back: F)
where
    F: FnMut() + 'static,
{
}
