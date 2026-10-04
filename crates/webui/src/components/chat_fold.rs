//! Collapsed disclosures for tool steps and model thinking.
//!
//! Split from `chat.rs` so that file can keep its module-health budget.

use dioxus::prelude::*;

use super::ThinkingStep;
use crate::icons::{IconCheck, IconChevronDown, IconChevronRight, IconSpinner, IconX};

/// Show the answer bubble. A think-only turn (empty content, a thinking
/// disclosure, no tool steps) does not also render a blank bubble. Tool steps
/// with no prose stay folded without an empty bubble, as they did before.
pub(super) fn show_answer(content_empty: bool, has_steps: bool, has_reasoning: bool) -> bool {
    !content_empty || (!has_steps && !has_reasoning)
}

/// Model thinking. Collapsed until the reader opens it. Not a tool step.
#[component]
pub(super) fn ReasoningBlock(text: String) -> Element {
    let mut expanded = use_signal(|| false);
    rsx! {
        div {
            class: "thinking-group reasoning-disclosure",
            button {
                class: "thinking-header",
                r#type: "button",
                onclick: move |_| {
                    let now = expanded();
                    expanded.set(!now);
                },
                span { class: "thinking-arrow",
                    if expanded() { IconChevronDown {} } else { IconChevronRight {} }
                }
                span { class: "thinking-label", "Thinking" }
            }
            if expanded() {
                div {
                    class: "thinking-body",
                    div { class: "tool-result-body", "{text}" }
                }
            }
        }
    }
}

/// The one-line header for a tool-result block.
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

#[component]
pub(super) fn ToolBlock(content: String) -> Element {
    let mut expanded = use_signal(|| false);
    let label = tool_block_label(&content);
    rsx! {
        div {
            class: "thinking-group",
            button {
                class: "thinking-header",
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

#[component]
pub(super) fn ThinkingGroup(steps: Vec<ThinkingStep>) -> Element {
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

/// `{}` and `null` are the JSON spellings of "no arguments".
pub(super) fn clean_args(args: &str) -> String {
    if args.is_empty() || args == "{}" || args == "null" {
        String::new()
    } else {
        args.to_string()
    }
}

pub(super) fn args_display(args: &str) -> String {
    if clean_args(args).is_empty() {
        String::new()
    } else {
        format!("({args})")
    }
}

#[component]
fn ThinkingStepRow(step: ThinkingStep) -> Element {
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
