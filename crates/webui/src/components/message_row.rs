//! One message row, plus the sub-components it composes (tool result, thinking group, copy
//! affordance).
//!
//! Split out of `chat.rs` so the message-rendering side of the surface lives in one place
//! independent of the chat's input, stream, and history logic. The mobile copy button is a
//! chrome gesture over the assistant row, so it belongs with the row rather than in its own
//! module that the row would have to import.

use dioxus::prelude::*;

use super::chat::chat_fold::{ReasoningBlock, show_answer};
use super::chat::{ChatMsg, ThinkingStep};
use crate::components::markdown::MarkdownText;
use crate::icons::{IconCheck, IconChevronDown, IconChevronRight, IconCopy, IconSpinner, IconX};

/// The one rendered bubble. Assistant rows also get a small Copy button below the bubble;
/// the button is hidden until the first tap on this response, shown on the second, hidden
/// again on the third, and so on. Per-row state — a tap on one message does not move the
/// button on another. Mirrors the conversation-row's `menu_open` ownership.
#[component]
pub(super) fn MessageRow(msg: ChatMsg) -> Element {
    let has_steps = !msg.thinking_steps.is_empty();
    let has_reasoning = msg.reasoning.is_some();
    // A think-only turn keeps the disclosure and skips the blank answer bubble.
    let show = show_answer(msg.content.is_empty(), has_steps, has_reasoning);
    let reasoning = msg.reasoning.clone().unwrap_or_default();
    let mut copy_button_visible = use_signal(|| false);
    // The button only renders for assistant responses; user/tool/system/error messages get no
    // copy affordance. The source we hand the clipboard is the raw markdown — pasted into a
    // markdown surface it renders, pasted into plain text it is the source, same as the
    // conversation log.
    let is_assistant = msg.role == "assistant";
    let copy_text = msg.content.clone();

    let on_bubble_click = move |evt: dioxus::prelude::MouseEvent| {
        if !is_assistant {
            return;
        }
        if click_target_was_link(&evt) {
            return;
        }
        copy_button_visible.set(next_copy_button_visible(copy_button_visible()));
    };

    rsx! {
        div {
            class: "bubble-row {msg.role}",
            div {
                class: "bubble-wrap",
                // Always attached, with the role check inside: a missing handler would mean the
                // browser's text-selection long-press has nothing to suppress the default
                // click-during-drag for, and a non-assistant row still gets *some* taps. A no-op
                // closure is the smaller change.
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
                if is_assistant && copy_button_visible() {
                    div {
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

/// Whether the copy button should be visible after the next tap lands on this response.
///
/// First tap on a hidden button shows it, the second tap hides it again, the third shows it
/// again — alternating per response. Extracted from the row so the alternation is testable
/// without rendering the Dioxus component.
pub(super) fn next_copy_button_visible(currently_visible: bool) -> bool {
    !currently_visible
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
