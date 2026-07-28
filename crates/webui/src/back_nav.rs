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
    /// The layer count we are trying to reach. Read by [`drive`], which may need several turns of
    /// the event loop to get there.
    static DESIRED: Cell<usize> = const { Cell::new(0) };
    /// A `history.back()` of ours is in flight, awaiting its `popstate`. Nothing may touch history
    /// until it lands — see [`drive`].
    static RETIRING: Cell<bool> = const { Cell::new(false) };
    static INSTALLED: Cell<bool> = const { Cell::new(false) };
}

/// Ask for `depth` guard entries. Safe to call on every render; it is a target, not a command.
///
/// Call from an effect that reads the same state the layer count is derived from, so the two cannot
/// drift.
#[cfg(target_arch = "wasm32")]
pub fn sync_depth(depth: usize) {
    DESIRED.set(depth);
    drive();
}

/// Move one step toward [`DESIRED`], then stop.
///
/// # Why this is serialized rather than a loop
///
/// `pushState` takes effect synchronously, but `history.back()` only *queues* a traversal — its
/// `popstate` arrives later. So a push issued between our `back()` and its traversal gets eaten by
/// that traversal, and the entry we meant to retire survives instead. The counts then say we hold a
/// guard we do not, and the next Back leaves the app.
///
/// That is not hypothetical. It appeared the moment the slash palette became a layer, because the
/// palette closes as a command is submitted while the picker that command opens appears an async tick
/// later: retire-then-push, straddling the traversal, and Back walked out of the app.
///
/// So at most one history mutation is outstanding at a time. While [`RETIRING`] is set we only record
/// the new target; the `popstate` handler clears the flag and calls back in. A burst of open/close
/// inside one tick therefore costs zero history calls — the target simply never changed by the time
/// we look at it.
#[cfg(target_arch = "wasm32")]
fn drive() {
    if RETIRING.get() {
        return;
    }
    let Some(history) = web_sys::window().and_then(|w| w.history().ok()) else {
        return;
    };
    let pushed = PUSHED.get();
    let desired = DESIRED.get();
    if desired > pushed {
        // Synchronous, so several at once are safe — and in practice layers open one at a time.
        for _ in pushed..desired {
            // No URL: the address bar must not change, only the history depth.
            let _ = history.push_state_with_url(&wasm_bindgen::JsValue::NULL, "", None);
        }
        PUSHED.set(desired);
    } else if desired < pushed {
        RETIRING.set(true);
        let _ = history.back();
    }
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
        if RETIRING.get() {
            // Our own `history.back()` landing. The entry is gone; release the lock and continue
            // toward whatever the target became while we were waiting.
            RETIRING.set(false);
            PUSHED.set(PUSHED.get().saturating_sub(1));
            drive();
            return;
        }
        let pushed = PUSHED.get();
        if pushed == 0 {
            // Nothing of ours was on the stack, so this Back belongs to the browser. Letting it
            // through is the point: once every layer is closed, Back leaves the app.
            return;
        }
        // The browser already popped the entry; keep our count level with it *before* closing the
        // layer, so the effect that follows sees a consistent pair and pushes no replacement.
        PUSHED.set(pushed - 1);
        DESIRED.set(pushed - 1);
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
