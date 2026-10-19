//! Input sizing and the pure stream-URL / title helpers.
//!
//! Split from `chat.rs` so that file stays inside the module-health ploc boundary.

// ── Input auto-grow (browser-only) ──────────────────────────────────────────

/// Grow the `#chat-input` textarea to fit its content. Resets to `auto` first so deleting text
/// shrinks it back down (reading `scroll_height` without that reset would just echo the current,
/// already-grown height). The CSS `max-height` on `.input` silently caps the visible box and
/// turns on the scrollbar once content exceeds it — this only needs to set the natural height.
#[cfg(target_arch = "wasm32")]
pub(super) fn resize_input_to_content() {
    use wasm_bindgen::JsCast;

    let Some(el) = web_sys::window()
        .and_then(|w| w.document())
        .and_then(|d| d.get_element_by_id("chat-input"))
        .and_then(|el| el.dyn_into::<web_sys::HtmlElement>().ok())
    else {
        return;
    };
    let style = el.style();
    let _ = style.set_property("height", "auto");
    let _ = style.set_property("height", &format!("{}px", el.scroll_height()));
}

#[cfg(not(target_arch = "wasm32"))]
pub(super) fn resize_input_to_content() {}

/// Put the caret back in the chat box after tapping a palette row.
///
/// The row's `onmousedown` already prevents the blur on desktop, but a touch tap on a phone can
/// still take focus away — and being dropped out of the input right after asking for a completion is
/// the opposite of helpful.
#[cfg(target_arch = "wasm32")]
pub(super) fn focus_chat_input() {
    use wasm_bindgen::JsCast;

    let Some(el) = web_sys::window()
        .and_then(|w| w.document())
        .and_then(|d| d.get_element_by_id("chat-input"))
        .and_then(|el| el.dyn_into::<web_sys::HtmlElement>().ok())
    else {
        return;
    };
    let _ = el.focus();
}

#[cfg(not(target_arch = "wasm32"))]
pub(super) fn focus_chat_input() {}

/// The `/api/chat/stream` URL for one turn.
///
/// Deliberately **not** `cfg(wasm32)` and deliberately pure: this is where a request parameter goes
/// missing, and on the wasm-only path that can only be found by deploying and watching what the
/// daemon does. `message` arrives already percent-encoded, since the caller needs it anyway.
///
/// The shape that matters: `session` / `incognito` / `profile` are mutually exclusive — the first
/// says *which* conversation, the other two say how to open one — while `model` is orthogonal to all
/// three and appends to whichever applied.
pub(super) fn stream_url(
    api_base: &str,
    encoded_message: &str,
    session: Option<&str>,
    incognito: bool,
    pending_profile: Option<&str>,
    pending_model: Option<&str>,
) -> String {
    let mut url = match session {
        Some(id) => format!("{api_base}/api/chat/stream?message={encoded_message}&session={id}"),
        // `true`, not `1`: axum's `Query` deserializes a bool through `FromStr`, which accepts only
        // "true"/"false". `incognito=1` fails the whole extraction, so the request would 400 and the
        // chat would simply not answer — a loud failure, but for a silly reason.
        None if incognito => {
            format!("{api_base}/api/chat/stream?message={encoded_message}&incognito=true")
        }
        // A profile picked before the first message has nowhere to be applied yet — the session it
        // scopes does not exist. Carrying it on the request that *creates* the session is what makes
        // the first turn run under it, which for a "basic chat" profile is the turn that matters.
        None if pending_profile.is_some() => format!(
            "{api_base}/api/chat/stream?message={encoded_message}&profile={}",
            urlencoding::encode(pending_profile.unwrap_or_default())
        ),
        None => format!("{api_base}/api/chat/stream?message={encoded_message}"),
    };
    // Appended rather than folded into the match above, which is the whole point: as a fifth arm it
    // would be dropped silently whenever a session id or a profile was also present — and a model
    // that goes missing does not fail, it just answers on the wrong one.
    if let Some(model) = pending_model.filter(|m| !m.is_empty()) {
        url.push_str(&format!("&model={}", urlencoding::encode(model)));
    }
    url
}

/// Truncate a conversation title to 60 bytes, walking back to a char boundary so the cut never
/// lands mid-codepoint (any curly quote, em-dash, etc.), and appending an ellipsis.
///
/// Bytes, not chars, because this is a display cap; the walk-back is what makes the byte cut
/// safe for non-ASCII text.
pub(super) fn truncate_title(text: &str) -> String {
    let t = text.trim();
    if t.len() > 60 {
        let mut cut = 57;
        while cut > 0 && !t.is_char_boundary(cut) {
            cut -= 1;
        }
        format!("{}…", &t[..cut])
    } else {
        t.to_string()
    }
}
