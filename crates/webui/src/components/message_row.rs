//! One message row, plus the sub-components it composes (tool result, thinking group, copy
//! affordance).
//!
//! Split out of `chat.rs` so the message-rendering side of the surface lives in one place
//! independent of the chat's input, stream, and history logic. The mobile copy button is a
//! chrome gesture over a user or assistant row, so the control belongs with the row. Which
//! single row is showing it is owned by [`MessageList`] — the same shape as the sidebar's
//! `menu_open` — because two buttons are never up at once. That list also scrolls the
//! revealed button into `#chat-messages` when the tap was on the last row.

use dioxus::prelude::*;

use super::chat::chat_fold::{ReasoningBlock, show_answer};
use super::chat::{ChatMsg, ThinkingStep};
use crate::components::markdown::MarkdownText;
use crate::icons::{IconCheck, IconChevronDown, IconChevronRight, IconCopy, IconSpinner, IconX};

/// The transcript rows and the one Copy button they share.
///
/// The open index and the scroll request live here, not on `Chat`: hooks have to run inside a
/// component, and the button is a property of the list (one visible at a time). `Chat` renders
/// the streaming ellipsis after this list, so the ellipsis is not a message index and a tap on
/// the last real row still counts as last while a turn is in flight.
#[component]
pub(super) fn MessageList(messages: Signal<Vec<ChatMsg>>) -> Element {
    // `None` hides every button. A second tap on the open row returns to `None`.
    let mut copy_open = use_signal(|| None::<usize>);
    // `(generation, index)` of a scroll request. Generation 0 is the initial "do not scroll".
    // Only a tap that reveals the button on the last message bumps it. The effect reads this
    // pair and not `copy_open`, so hiding, or revealing an earlier row, does not scroll.
    let mut copy_scroll = use_signal(|| (0u32, 0usize));
    use_effect(move || {
        let (nonce, index) = copy_scroll();
        if nonce == 0 {
            return;
        }
        schedule_scroll_messages_to_copy_button(index);
    });

    let toggle_copy = use_callback(move |index: usize| {
        let before = copy_open();
        let msgs = messages.read();
        let Some(last) = msgs.len().checked_sub(1) else {
            return;
        };
        if index > last || !message_offers_copy(msgs[index].role) {
            return;
        }
        drop(msgs);
        copy_open.set(next_open_copy(before, index));
        if should_scroll_copy_button(before, index, last) {
            let (nonce, _) = copy_scroll();
            // `max(1)` keeps a wrapping generation from landing back on 0, which the effect
            // treats as "no request".
            copy_scroll.set((nonce.wrapping_add(1).max(1), index));
        }
    });

    rsx! {
        for (i, msg) in messages.read().iter().enumerate() {
            MessageRow {
                key: "{i}",
                msg: msg.clone(),
                index: i,
                copy_visible: message_offers_copy(msg.role) && copy_open() == Some(i),
                on_toggle_copy: move |index| toggle_copy.call(index),
            }
        }
    }
}

/// The one rendered bubble. User and assistant rows get a Copy button below the bubble.
/// The list decides which single index is open: the first tap on a closed row shows it and
/// hides any other, and a second tap on that same row hides it. Tool, system, and error rows
/// have no copy control. The clipboard receives the row's raw markdown (`msg.content`).
///
/// The button stays inside `.bubble-wrap`. A user row is `justify-content: flex-end`, so the
/// wrap — bubble and button together — sits on the right.
#[component]
pub(super) fn MessageRow(
    msg: ChatMsg,
    index: usize,
    copy_visible: bool,
    on_toggle_copy: EventHandler<usize>,
) -> Element {
    let has_steps = !msg.thinking_steps.is_empty();
    let has_reasoning = msg.reasoning.is_some();
    // A think-only turn keeps the disclosure and skips the blank answer bubble.
    let show = show_answer(msg.content.is_empty(), has_steps, has_reasoning);
    let reasoning = msg.reasoning.clone().unwrap_or_default();
    // User and assistant rows only. The source handed to the clipboard is the raw markdown —
    // pasted into a markdown surface it renders, pasted into plain text it is the source, same
    // as the conversation log.
    let offers_copy = message_offers_copy(msg.role);
    let show_copy = offers_copy && copy_visible;
    let copy_text = msg.content.clone();

    let on_bubble_click = move |evt: dioxus::prelude::MouseEvent| {
        if !offers_copy {
            return;
        }
        if click_target_was_link(&evt) {
            return;
        }
        on_toggle_copy.call(index);
    };

    rsx! {
        div {
            class: "bubble-row {msg.role}",
            div {
                class: "bubble-wrap",
                // Always attached, with the role check inside: a missing handler would mean the
                // browser's text-selection long-press has nothing to suppress the default
                // click-during-drag for, and a tool/system/error row still gets *some* taps. A
                // no-op closure is the smaller change.
                onclick: on_bubble_click,
                if has_reasoning {
                    ReasoningBlock { text: reasoning }
                }
                if has_steps {
                    ThinkingGroup { steps: msg.thinking_steps.clone() }
                }
                if show {
                    match msg.role {
                        "assistant" | "user" => rsx! {
                            div { class: "bubble {msg.role}",
                                MarkdownText { content: msg.content.clone() }
                            }
                        },
                        "tool" => rsx! { ToolBlock { content: msg.content.clone() } },
                        _ => rsx! {
                            div { class: "bubble {msg.role}",
                                "{msg.content}"
                            }
                        },
                    }
                }
                if show_copy {
                    div {
                        // The list scrolls this exact node into `.messages` when the tap that
                        // revealed it was on the last message. One button is visible, so one id.
                        id: "response-copy-{index}",
                        class: "response-copy-row",
                        button {
                            class: "response-copy-btn",
                            r#type: "button",
                            // The button click lands on the bubble-wrap too; the row's toggle would
                            // flip the visibility on the same tap that the user used to copy.
                            // Stopping propagation here keeps the two gestures independent.
                            onclick: move |evt| {
                                evt.stop_propagation();
                                copy_to_clipboard(&copy_text);
                            },
                            IconCopy {}
                            span { class: "response-copy-label", "Copy" }
                        }
                    }
                }
            }
        }
    }
}

/// Whether the copy button on the tapped message should be visible after this tap.
///
/// First tap on a hidden button shows it, the second hides it, the third shows it again.
/// This is the same-message half of the rule. The thread keeps one shared open index
/// ([`next_open_copy`]), so showing this message hides every other button.
pub(super) fn next_copy_button_visible(currently_visible: bool) -> bool {
    !currently_visible
}

/// User-sent messages and assistant responses carry the copy control. Tool results, system
/// notes, and errors do not — there is nothing there the reader would paste as markdown.
pub(super) fn message_offers_copy(role: &str) -> bool {
    matches!(role, "user" | "assistant")
}

/// Which message's copy button is visible after `tapped` is tapped.
///
/// `open` is the index that was visible before the tap, or `None` when every button was
/// hidden. Revealing `tapped` replaces that index. Tapping the message that is already open
/// hides it, so repeated taps on one message alternate show, hide, show, hide.
pub(super) fn next_open_copy(open: Option<usize>, tapped: usize) -> Option<usize> {
    if next_copy_button_visible(open == Some(tapped)) {
        Some(tapped)
    } else {
        None
    }
}

/// Whether this tap should scroll the thread so the revealed Copy button is on screen.
///
/// Scroll only when the tap reveals the button on the last message in the thread list.
/// Hiding that button does not scroll. A tap on any earlier message does not scroll, including
/// one that hides the last message's button by opening an earlier one. `last_index` is the
/// last `ChatMsg`. The streaming ellipsis is not a list entry, so it is never `last_index`.
pub(super) fn should_scroll_copy_button(
    open_before: Option<usize>,
    tapped: usize,
    last_index: usize,
) -> bool {
    tapped == last_index && next_open_copy(open_before, tapped) == Some(tapped)
}

/// True when the click target was a link inside the message — used to skip the toggle so the
/// browser's link navigation is not preempted by a response-chrome gesture.
#[cfg(target_arch = "wasm32")]
pub(super) fn click_target_was_link(evt: &dioxus::prelude::MouseEvent) -> bool {
    use dioxus::web::WebEventExt;
    use wasm_bindgen::JsCast;

    let data = evt.data();
    let Some(web_evt): Option<web_sys::MouseEvent> = data.try_as_web_event() else {
        return false;
    };
    let target: Option<web_sys::EventTarget> = web_evt.target();
    let Some(target) = target else {
        return false;
    };
    let target: web_sys::Element = match target.dyn_into::<web_sys::Element>() {
        Ok(el) => el,
        Err(_) => return false,
    };
    // `closest("a")` walks up to the nearest anchor; a tap on a link or any inline run inside one
    // resolves to that anchor. `<button>` for the copy button itself sits *outside* the bubble and
    // so is never the click target here.
    target.closest("a").ok().flatten().is_some()
}

#[cfg(not(target_arch = "wasm32"))]
pub(super) fn click_target_was_link(_evt: &dioxus::prelude::MouseEvent) -> bool {
    false
}

/// Push `text` to the system clipboard. Browser-only — the host build never has a clipboard to
/// write to. The `navigator.clipboard` API is gated on a secure context, which the PWA shell
/// already provides, so a missing call there is a real failure rather than a missing feature.
#[cfg(target_arch = "wasm32")]
pub(super) fn copy_to_clipboard(text: &str) {
    let Some(window) = web_sys::window() else {
        return;
    };
    let clipboard = window.navigator().clipboard();
    // Best-effort: rejections are silently dropped. Surfacing them as a toast would be more
    // helpful, but no error path here is recoverable — and the user's next tap dismisses the
    // button, so a stuck "failed to copy" message would just have to be dismissed first.
    let _ = clipboard.write_text(text);
}

#[cfg(not(target_arch = "wasm32"))]
pub(super) fn copy_to_clipboard(_text: &str) {}

/// Bring `#response-copy-{index}` inside `#chat-messages`.
///
/// Called from the effect that follows a reveal of the last message's Copy button, so the
/// row is already in the committed tree. Measurement uses viewport coordinates: the delta
/// between the button and the container is exactly the `scrollTop` adjustment, and no
/// ancestor moves. Desktop widths hide the control with the same `max-width: 768px` query
/// as `.response-copy-row`, and a `display: none` box must not be scrolled to.
#[cfg(target_arch = "wasm32")]
fn schedule_scroll_messages_to_copy_button(index: usize) {
    if !scroll_messages_to_copy_button(index) {
        // The row can commit a turn after the effect. One more frame, then stop — a second
        // miss means the button was hidden again, and hiding must not scroll.
        scroll_copy_button_on_next_frame(index);
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn schedule_scroll_messages_to_copy_button(_index: usize) {}

#[cfg(target_arch = "wasm32")]
fn scroll_copy_button_on_next_frame(index: usize) {
    use wasm_bindgen::JsCast;
    use wasm_bindgen::closure::Closure;

    let Some(window) = web_sys::window() else {
        return;
    };
    let callback = Closure::once(move |_timestamp: f64| {
        let _ = scroll_messages_to_copy_button(index);
    });
    if window
        .request_animation_frame(callback.as_ref().unchecked_ref())
        .is_ok()
    {
        callback.forget();
    }
}

/// Scroll `#chat-messages` until the copy button for `index` is fully inside it.
///
/// Returns `false` when that button is not in the document yet, so the caller can retry
/// once. Returns `true` when there is nothing to do — desktop breakpoint, or the button is
/// already visible — so a retry does not fire.
#[cfg(target_arch = "wasm32")]
fn scroll_messages_to_copy_button(index: usize) -> bool {
    let Some(window) = web_sys::window() else {
        return true;
    };
    let on_phone = window
        .match_media("(max-width: 768px)")
        .ok()
        .flatten()
        .is_some_and(|list| list.matches());
    if !on_phone {
        return true;
    }
    let Some(document) = window.document() else {
        return true;
    };
    let Some(container) = document.get_element_by_id("chat-messages") else {
        return false;
    };
    let Some(button) = document.get_element_by_id(&format!("response-copy-{index}")) else {
        return false;
    };
    let container_box = container.get_bounding_client_rect();
    let button_box = button.get_bounding_client_rect();
    // `display: none` (the desktop rule, if the query and the layout disagree) has no box.
    if button_box.width() == 0.0 && button_box.height() == 0.0 {
        return false;
    }
    let delta = if button_box.bottom() > container_box.bottom() {
        button_box.bottom() - container_box.bottom()
    } else if button_box.top() < container_box.top() {
        button_box.top() - container_box.top()
    } else {
        0.0
    };
    if delta.abs() < 0.5 {
        return true;
    }
    let next = (container.scroll_top() as f64 + delta).round();
    if next.is_finite() {
        container.set_scroll_top(next as i32);
    }
    true
}

// ── Collapsible tool result ─────────────────────────────────────────────────

/// A `tool` message — the full text a dispatched tool returned, as replayed from conversation
/// history. This is the block that used to be permanently open: the live turn shows a
/// [`ThinkingGroup`], but history has no thinking steps (they exist only on the SSE stream), so a
/// reloaded conversation rendered the whole result as a plain bubble with no way to fold it away.
/// Several hundred characters of session ids and journal paths then sat between the question and
/// the answer.
///
/// Collapsed by default, with the outcome line kept in the header, and it stays wherever the user
/// puts it.
#[component]
pub(super) fn ToolBlock(content: String) -> Element {
    let mut expanded = use_signal(|| false);

    // The daemon's first line is already a summary ("RESULT (Succeeded):"). Reuse it as the header
    // rather than inventing one, falling back only if it is empty so the header is never blank.
    let label = tool_block_label(&content);

    rsx! {
        div {
            class: "thinking-group",
            button {
                class: "thinking-header",
                // Explicit: a bare <button> defaults to type=submit, which would post the chat
                // form the moment this block is ever rendered inside one.
                r#type: "button",
                onclick: move |_| {
                    let now = expanded();
                    expanded.set(!now);
                },
                span { class: "thinking-arrow",
                    if expanded() { IconChevronDown {} } else { IconChevronRight {} }
                }
                span { class: "thinking-label", "{label}" }
            }
            if expanded() {
                div {
                    class: "thinking-body",
                    div { class: "tool-result-body", "{content}" }
                }
            }
        }
    }
}

/// The one-line header for a tool-result block: the daemon's own summary line when present,
/// falling back to a neutral label, with a trailing colon trimmed ("RESULT (Succeeded):" reads
/// as "RESULT (Succeeded)").
pub(super) fn tool_block_label(content: &str) -> String {
    content
        .lines()
        .next()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .unwrap_or("Tool result")
        .trim_end_matches(':')
        .to_string()
}

// ── Collapsible thinking-steps group ────────────────────────────────────────

#[component]
pub(super) fn ThinkingGroup(steps: Vec<ThinkingStep>) -> Element {
    // Starts collapsed and then belongs entirely to the user — nothing derived from `steps` ever
    // moves it again. It used to open itself whenever a step was pending, which meant the run
    // decided the disclosure state instead of the reader: it sprang open mid-turn, and wherever it
    // happened to be when the last step resolved was where it stuck. Progress is already in the
    // header label, which is the part that stays visible while collapsed.
    let mut expanded = use_signal(|| false);

    let has_pending = steps.iter().any(|s| s.ok.is_none());
    let toggle = move |_| {
        let now = expanded();
        expanded.set(!now);
    };

    let count = steps.len();
    let summary: Vec<String> = steps.iter().map(|s| s.tool_name.clone()).collect();
    let summary_text = summary.join(", ");

    let header_label = if has_pending {
        format!(
            "Thinking ({count} step{plural}): {summary_text} \u{2026}",
            plural = if count == 1 { "" } else { "s" }
        )
    } else {
        format!(
            "Thinking ({count} step{plural}): {summary_text}",
            plural = if count == 1 { "" } else { "s" }
        )
    };

    rsx! {
        div {
            class: "thinking-group",
            button {
                class: "thinking-header",
                r#type: "button",
                onclick: toggle,
                span { class: "thinking-arrow",
                    if expanded() { IconChevronDown {} } else { IconChevronRight {} }
                }
                span { class: "thinking-label", "{header_label}" }
            }
            if expanded() {
                div {
                    class: "thinking-body",
                    for step in steps.iter() {
                        ThinkingStepRow { step: step.clone() }
                    }
                }
            }
        }
    }
}

/// `{}` and `null` are the JSON spellings of "no arguments", and the empty string is the
/// trimmed version of the same idea — all three render as nothing.
pub(super) fn clean_args(args: &str) -> String {
    if args.is_empty() || args == "{}" || args == "null" {
        String::new()
    } else {
        args.to_string()
    }
}

/// The parenthesized argument text shown next to a tool name. Empty and JSON-"empty" args show
/// nothing at all — `tool(clean()_up)` is noise, `tool` is the same information.
pub(super) fn args_display(args: &str) -> String {
    if clean_args(args).is_empty() {
        String::new()
    } else {
        format!("({args})")
    }
}

#[component]
pub(super) fn ThinkingStepRow(step: ThinkingStep) -> Element {
    let status_cls = match step.ok {
        None => "thinking-step pending",
        Some(true) => "thinking-step ok",
        Some(false) => "thinking-step err",
    };

    let args_text = args_display(&step.tool_args);

    let name_text = format!("{}{}", step.tool_name, args_text);

    rsx! {
        div {
            class: "{status_cls}",
            span { class: "thinking-step-name", "{name_text}" }
            span { class: "thinking-step-mark",
                match step.ok {
                    None => rsx! { IconSpinner {} },
                    Some(true) => rsx! { IconCheck {} },
                    Some(false) => rsx! { IconX {} },
                }
            }
            if !step.preview.is_empty() {
                span { class: "thinking-step-preview", "{step.preview}" }
            }
        }
    }
}
