//! One message row, plus the sub-components it composes (tool result, thinking group, copy
//! affordance).
//!
//! Split out of `chat.rs` so the message-rendering side of the surface lives in one place
//! independent of the chat's input, stream, and history logic. User and assistant rows carry
//! a Copy button. A tap on the message text copies it too, except when the tap is a link, a
//! control, or a text selection.

use dioxus::prelude::*;

use super::chat::chat_fold::{ReasoningBlock, show_answer};
use super::chat::chat_reasoning::fold_thinking;
use super::chat::{ChatMsg, ThinkingStep};
use crate::components::markdown::MarkdownText;
use crate::icons::{IconCheck, IconChevronDown, IconChevronRight, IconCopy, IconSpinner, IconX};

/// The transcript rows. Each user or assistant row owns its own Copy button.
#[component]
pub(super) fn MessageList(messages: Signal<Vec<ChatMsg>>) -> Element {
    rsx! {
        for (i, msg) in messages.read().iter().enumerate() {
            MessageRow {
                key: "{i}",
                msg: msg.clone(),
            }
        }
    }
}

/// The one rendered bubble. User and assistant rows show a Copy button. Tool, system, and
/// error rows have none. The clipboard receives the visible reply. Assistant thinking
/// stays in the disclosure, so a copy does not include it.
///
/// The button stays inside `.bubble-wrap`. A user row is `justify-content: flex-end`, so the
/// wrap — bubble and button together — sits on the right. On a narrow or touch viewport the
/// button stays visible. On a desktop with a hover pointer it appears on hover or focus.
#[component]
pub(super) fn MessageRow(msg: ChatMsg) -> Element {
    let has_steps = !msg.thinking_steps.is_empty();
    // Streaming keeps `<think>` in assistant `content` until the close tag, and history
    // can store the same words on `reasoning`. Fold before render so the disclosure and
    // the reply do not show that text twice. User text is copied as typed.
    let (content, reasoning_text) = if msg.role == "assistant" {
        fold_thinking(&msg.content, msg.reasoning.as_deref())
    } else {
        (msg.content.clone(), None)
    };
    let has_reasoning = reasoning_text.is_some();
    let show = show_answer(content.is_empty(), has_steps, has_reasoning);
    let reasoning = reasoning_text.unwrap_or_default();
    let offers_copy = message_offers_copy(msg.role);
    let copy_text = if content.is_empty() {
        reasoning.clone()
    } else {
        content.clone()
    };
    let mut copied = use_signal(|| false);
    let mut copy_nonce = use_signal(|| 0u32);

    let text_for_button = copy_text.clone();
    let on_copy_click = move |evt: dioxus::prelude::MouseEvent| {
        evt.stop_propagation();
        mark_copied(&text_for_button, &mut copied, &mut copy_nonce);
    };
    let text_for_tap = copy_text.clone();
    let on_bubble_click = move |evt: dioxus::prelude::MouseEvent| {
        if !offers_copy || click_target_skips_copy(&evt) || !selection_is_collapsed() {
            return;
        }
        mark_copied(&text_for_tap, &mut copied, &mut copy_nonce);
    };

    let label = copy_feedback_label(copied());
    rsx! {
        div {
            class: "bubble-row {msg.role}",
            div {
                class: "bubble-wrap",
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
                                MarkdownText { content: content.clone() }
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
                if offers_copy {
                    div {
                        class: "response-copy-row",
                        button {
                            class: "response-copy-btn",
                            r#type: "button",
                            aria_label: "{label}",
                            onclick: on_copy_click,
                            IconCopy {}
                            span { class: "response-copy-label", aria_live: "polite", "{label}" }
                        }
                    }
                }
            }
        }
    }
}

/// User-sent messages and assistant responses carry the copy control. Tool results, system
/// notes, and errors do not — there is nothing there the reader would paste as markdown.
pub(super) fn message_offers_copy(role: &str) -> bool {
    matches!(role, "user" | "assistant")
}

/// The button label. `true` is the short confirmation after a successful copy.
pub(super) fn copy_feedback_label(copied: bool) -> &'static str {
    if copied { "Copied" } else { "Copy" }
}

/// `navigator.clipboard.writeText` rejects when the page is not a secure context. The WebUI
/// is often served as plain `http` on a tailnet, so that call fails and the tap looks dead.
/// Those pages use a textarea plus `document.execCommand("copy")` instead.
pub(super) fn clipboard_needs_exec_command(secure_context: bool) -> bool {
    !secure_context
}

fn mark_copied(text: &str, copied: &mut Signal<bool>, nonce: &mut Signal<u32>) {
    if !copy_to_clipboard(text) {
        return;
    }
    let next = nonce().wrapping_add(1).max(1);
    nonce.set(next);
    copied.set(true);
    schedule_clear_copied(*copied, *nonce, next);
}

/// True when the click is a link or a control inside the row. Those keep their own action.
/// A text selection is handled separately so a drag-select does not also copy.
#[cfg(target_arch = "wasm32")]
fn click_target_skips_copy(evt: &dioxus::prelude::MouseEvent) -> bool {
    use dioxus::web::WebEventExt;
    use wasm_bindgen::JsCast;

    let data = evt.data();
    let Some(web_evt): Option<web_sys::MouseEvent> = data.try_as_web_event() else {
        return false;
    };
    let Some(target) = web_evt.target() else {
        return false;
    };
    let Ok(target) = target.dyn_into::<web_sys::Element>() else {
        return false;
    };
    target
        .closest("a, button, input, textarea, select, summary")
        .ok()
        .flatten()
        .is_some()
}

#[cfg(not(target_arch = "wasm32"))]
fn click_target_skips_copy(_evt: &dioxus::prelude::MouseEvent) -> bool {
    false
}

/// A drag that selected text must not also copy the whole message.
#[cfg(target_arch = "wasm32")]
fn selection_is_collapsed() -> bool {
    let Some(window) = web_sys::window() else {
        return true;
    };
    window
        .get_selection()
        .ok()
        .flatten()
        .is_none_or(|sel| sel.is_collapsed())
}

#[cfg(not(target_arch = "wasm32"))]
fn selection_is_collapsed() -> bool {
    true
}

/// Push `text` to the system clipboard. Returns whether the browser accepted the write.
///
/// `navigator.clipboard` exists on insecure `http` and then rejects the promise, which is why
/// a tap used to do nothing. The textarea fallback runs in that case, inside the same gesture.
#[cfg(target_arch = "wasm32")]
pub(super) fn copy_to_clipboard(text: &str) -> bool {
    let Some(window) = web_sys::window() else {
        return false;
    };
    if clipboard_needs_exec_command(window.is_secure_context()) {
        return copy_via_textarea(&window, text);
    }
    let clipboard = window.navigator().clipboard();
    // The promise resolves later. A rejection here is a permission denial, not the
    // missing-API case the textarea path covers. The gesture has already been accepted.
    let _ = clipboard.write_text(text);
    true
}

#[cfg(not(target_arch = "wasm32"))]
pub(super) fn copy_to_clipboard(_text: &str) -> bool {
    false
}

#[cfg(target_arch = "wasm32")]
fn copy_via_textarea(window: &web_sys::Window, text: &str) -> bool {
    use wasm_bindgen::JsCast;

    let Some(document) = window.document() else {
        return false;
    };
    let Ok(element) = document.create_element("textarea") else {
        return false;
    };
    let Ok(textarea) = element.dyn_into::<web_sys::HtmlTextAreaElement>() else {
        return false;
    };
    textarea.set_value(text);
    textarea.set_read_only(true);
    let style = textarea.style();
    let _ = style.set_property("position", "fixed");
    let _ = style.set_property("top", "0");
    let _ = style.set_property("left", "0");
    let _ = style.set_property("opacity", "0");
    let Some(body) = document.body() else {
        return false;
    };
    if body.append_child(&textarea).is_err() {
        return false;
    }
    textarea.focus().ok();
    textarea.select();
    let end = u32::try_from(text.len()).unwrap_or(u32::MAX);
    let _ = textarea.set_selection_range(0, end);
    let copied = exec_copy(&document);
    let _ = body.remove_child(&textarea);
    copied
}

/// `Document.execCommand` left `web-sys`. Call it on the document object directly.
#[cfg(target_arch = "wasm32")]
fn exec_copy(document: &web_sys::Document) -> bool {
    use wasm_bindgen::JsCast;

    let Ok(value) = js_sys::Reflect::get(document, &wasm_bindgen::JsValue::from_str("execCommand"))
    else {
        return false;
    };
    let Ok(function) = value.dyn_into::<js_sys::Function>() else {
        return false;
    };
    function
        .call1(document, &wasm_bindgen::JsValue::from_str("copy"))
        .ok()
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
}

#[cfg(target_arch = "wasm32")]
fn schedule_clear_copied(mut copied: Signal<bool>, nonce: Signal<u32>, expected: u32) {
    use wasm_bindgen::JsCast;
    use wasm_bindgen::closure::Closure;

    let Some(window) = web_sys::window() else {
        return;
    };
    let callback = Closure::once(move || {
        if nonce() == expected {
            copied.set(false);
        }
    });
    if window
        .set_timeout_with_callback_and_timeout_and_arguments_0(
            callback.as_ref().unchecked_ref(),
            1_600,
        )
        .is_ok()
    {
        callback.forget();
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn schedule_clear_copied(_copied: Signal<bool>, _nonce: Signal<u32>, _expected: u32) {}

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
