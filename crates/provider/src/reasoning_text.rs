//! Fold `<think>` interiors and the reasoning channel into one optional string.
//!
//! Identical or contained blocks collapse to the longer text so a provider that
//! repeats the same thought in `reasoning_content`, `reasoning_details`, and a
//! `<think>` block is stored once.

use serde_json::Value;

use crate::think_text::split_answer;

/// `reasoning_content` fragments plus `reasoning_details` text for one completion.
#[derive(Default)]
pub(crate) struct ChannelAccum {
    content: String,
    details: String,
}

impl ChannelAccum {
    pub(crate) fn note_content(&mut self, text: &str) {
        if text.is_empty() || self.content.ends_with(text) {
            return;
        }
        if text.starts_with(&self.content) {
            self.content = text.to_string();
            return;
        }
        self.content.push_str(text);
    }

    pub(crate) fn note_details(&mut self, value: &Value) {
        let Some(text) = details_text(value) else {
            return;
        };
        if self.details.is_empty() || text.contains(&self.details) {
            self.details = text;
            return;
        }
        if self.details.contains(&text) {
            return;
        }
        self.details.push('\n');
        self.details.push_str(&text);
    }

    pub(crate) fn finish(&self) -> Option<String> {
        let content = nonempty(&self.content);
        let details = nonempty(&self.details);
        merge(content, details)
    }
}

/// Visible answer plus thinking from one OpenAI-compatible `message` object.
pub(crate) fn from_choice_message(message: &Value) -> (Option<String>, Option<String>) {
    let raw = message.get("content").and_then(Value::as_str).unwrap_or("");
    let (visible, tagged) = split_answer(raw);
    let channel = ChannelAccum::from_message(message);
    let reasoning = merge(tagged, channel);
    let content = (!visible.is_empty()).then_some(visible);
    (content, reasoning)
}

impl ChannelAccum {
    fn from_message(message: &Value) -> Option<String> {
        let mut channel = ChannelAccum::default();
        if let Some(text) = message.get("reasoning_content").and_then(Value::as_str) {
            channel.note_content(text);
        }
        if let Some(details) = message.get("reasoning_details") {
            channel.note_details(details);
        }
        channel.finish()
    }
}

/// Assistant history: strip `<think>` out of stored content and keep the interior
/// together with reasoning already stored on the node. Other roles pass through.
pub fn transcript_parts(
    role: &str,
    content: String,
    stored: Option<String>,
) -> (String, Option<String>) {
    if role != "assistant" {
        return (content, None);
    }
    let (visible, tagged) = split_answer(&content);
    (visible, merge(tagged, stored))
}

pub(crate) fn merge(first: Option<String>, second: Option<String>) -> Option<String> {
    match (nonempty_owned(first), nonempty_owned(second)) {
        (None, next) => next,
        (prev, None) => prev,
        (Some(prev), Some(next)) => Some(combine(&prev, &next)),
    }
}

fn nonempty(text: &str) -> Option<String> {
    let text = text.trim();
    if text.is_empty() {
        None
    } else {
        Some(text.to_string())
    }
}

fn nonempty_owned(text: Option<String>) -> Option<String> {
    text.and_then(|value| nonempty(&value))
}

fn combine(prev: &str, next: &str) -> String {
    let next = next.trim();
    let prev = prev.trim();
    if next.is_empty() || prev == next || prev.contains(next) {
        return prev.to_string();
    }
    if next.contains(prev) {
        return next.to_string();
    }
    format!("{prev}\n{next}")
}

fn details_text(value: &Value) -> Option<String> {
    let items = value.as_array()?;
    let mut out = String::new();
    for item in items {
        let Some(text) = item.get("text").and_then(Value::as_str) else {
            continue;
        };
        let text = text.trim();
        if text.is_empty() {
            continue;
        }
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(text);
    }
    nonempty(&out)
}
