//! Model thinking on a chat row: history reload and the live turn's finish.
//!
//! Token events append onto `content`. A `<think>` block in that stream is the
//! same text history later stores as `reasoning`, so the disclosure and the
//! reply would show it twice. Folding pulls the block out of the reply, drops
//! a thought that only repeats the reply, and collapses a thought that repeats
//! itself.

use super::ChatMsg;

const OPEN: &str = "<think>";
const CLOSE: &str = "</think>";

/// Drop blank thinking so an answer-only reply has no disclosure.
pub(super) fn kept(reasoning: Option<&str>) -> Option<String> {
    let text = reasoning?.trim();
    if text.is_empty() {
        None
    } else {
        Some(text.to_string())
    }
}

/// Visible reply plus the thinking that should sit in the disclosure.
///
/// A closed `<think>` block leaves the answer and becomes thinking. A thought
/// that equals the reply, or only repeats the reply, is omitted. A thought
/// stored twice (`secret` then `secret` again) is kept once.
pub(in super::super) fn fold_thinking(
    content: &str,
    reasoning: Option<&str>,
) -> (String, Option<String>) {
    let (visible, tagged) = split_think(content);
    let merged = merge_reason(tagged, kept(reasoning));
    let merged = merged.map(|text| collapse_repeat(&text));
    let merged = merged.and_then(|text| drop_visible_overlap(text, &visible));
    (visible, merged)
}

/// Append one SSE token. Deltas concatenate. A finished `<think>` block is
/// moved onto `reasoning` so later tokens extend the answer, not the thought.
pub(super) fn append_token(msg: &mut ChatMsg, token: &str) {
    if token.is_empty() {
        return;
    }
    msg.content.push_str(token);
    if msg.content.to_ascii_lowercase().contains(CLOSE) {
        let (content, reasoning) = fold_thinking(&msg.content, msg.reasoning.as_deref());
        msg.content = content;
        msg.reasoning = reasoning;
    }
}

/// Copy stored thinking onto the assistant rows already on screen, and append a
/// row when the turn was think-only and the stream never opened one.
///
/// The live row may still hold the streamed `<think>` block. Folding replaces
/// that row's text with the visible answer and one copy of the thought.
pub(super) fn merge_history(live: &mut Vec<ChatMsg>, history: &[ChatMsg]) {
    let saved: Vec<&ChatMsg> = history.iter().filter(|m| m.role == "assistant").collect();
    let live_at: Vec<usize> = live
        .iter()
        .enumerate()
        .filter(|(_, m)| m.role == "assistant")
        .map(|(i, _)| i)
        .collect();
    let shared = live_at.len().min(saved.len());
    for i in 0..shared {
        let stored = saved[i]
            .reasoning
            .clone()
            .or_else(|| live[live_at[i]].reasoning.clone());
        let (content, reasoning) = fold_thinking(&live[live_at[i]].content, stored.as_deref());
        live[live_at[i]].content = content;
        live[live_at[i]].reasoning = reasoning;
    }
    if saved.len() <= live_at.len() {
        return;
    }
    for extra in &saved[live_at.len()..] {
        push_saved(live, extra);
    }
}

fn push_saved(live: &mut Vec<ChatMsg>, extra: &ChatMsg) {
    let (content, reasoning) = fold_thinking(&extra.content, extra.reasoning.as_deref());
    if reasoning.is_none() && content.is_empty() {
        return;
    }
    let mut msg = super::ChatMsg::new_assistant(content);
    msg.reasoning = reasoning;
    live.push(msg);
}

fn split_think(raw: &str) -> (String, Option<String>) {
    let lower = raw.to_ascii_lowercase();
    if !lower.contains(OPEN) {
        return (raw.to_string(), None);
    }
    let mut visible = String::new();
    let mut reasoning = String::new();
    let mut start = 0usize;
    let mut answer_started = false;
    while let Some(rel) = lower[start..].find(OPEN) {
        let idx = start + rel;
        let prefix = &raw[start..idx];
        if !prefix.trim().is_empty() {
            visible.push_str(prefix);
            answer_started = true;
        } else if answer_started {
            visible.push_str(prefix);
        }
        let after_open = idx + OPEN.len();
        if let Some(rel_end) = lower[after_open..].find(CLOSE) {
            let end = after_open + rel_end;
            note_reason(&mut reasoning, &raw[after_open..end]);
            start = end + CLOSE.len();
            continue;
        }
        if !answer_started {
            note_reason(&mut reasoning, &raw[after_open..]);
            start = raw.len();
            break;
        }
        visible.push_str(&raw[idx..]);
        start = raw.len();
        break;
    }
    if start < raw.len() {
        visible.push_str(&raw[start..]);
    }
    (visible.trim().to_string(), nonempty(&reasoning))
}

fn note_reason(out: &mut String, text: &str) {
    let Some(text) = nonempty(text) else {
        return;
    };
    if !out.is_empty() {
        out.push('\n');
    }
    out.push_str(&text);
}

fn nonempty(text: &str) -> Option<String> {
    let text = text.trim();
    if text.is_empty() {
        None
    } else {
        Some(text.to_string())
    }
}

fn merge_reason(tagged: Option<String>, stored: Option<String>) -> Option<String> {
    match (tagged.and_then(|text| nonempty(&text)), stored) {
        (None, next) | (next, None) => next,
        (Some(prev), Some(next)) => Some(combine(&prev, &next)),
    }
}

fn combine(prev: &str, next: &str) -> String {
    let prev = collapse_repeat(prev);
    let next = collapse_repeat(next);
    if next.is_empty() || prev == next || prev.contains(&next) {
        return prev;
    }
    if next.contains(&prev) {
        return next;
    }
    collapse_repeat(&format!("{prev}\n{next}"))
}

/// One copy of a block that was joined to itself (`a\na`, or `a` then `a`).
fn collapse_repeat(text: &str) -> String {
    let text = text.trim();
    if text.is_empty() {
        return String::new();
    }
    let paras: Vec<&str> = text
        .split("\n\n")
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .collect();
    if paras.len() > 1 {
        let mut kept: Vec<&str> = Vec::new();
        for para in paras {
            if let Some(index) = kept
                .iter()
                .position(|prev| *prev == para || para.contains(*prev) || prev.contains(para))
            {
                if para.len() > kept[index].len() {
                    kept[index] = para;
                }
                continue;
            }
            kept.push(para);
        }
        if kept.len() == 1 {
            return collapse_repeat(kept[0]);
        }
        return kept.join("\n\n");
    }
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    if lines.len() > 1 && lines.iter().all(|line| *line == lines[0]) {
        return lines[0].to_string();
    }
    text.to_string()
}

/// Drop thinking that is only the reply, including a reply glued on either end.
fn drop_visible_overlap(reasoning: String, visible: &str) -> Option<String> {
    let visible = visible.trim();
    let mut text = reasoning.trim().to_string();
    if text.is_empty() {
        return None;
    }
    if visible.is_empty() {
        return Some(text);
    }
    if text == visible || (text.len() >= 12 && visible.contains(&text)) {
        return None;
    }
    loop {
        let current = text.trim().to_string();
        if current.is_empty() {
            return None;
        }
        if let Some(rest) = strip_affix(&current, visible) {
            text = rest;
            continue;
        }
        break;
    }
    let text = text.trim();
    if text.is_empty() || text == visible || (text.len() >= 12 && visible.contains(text)) {
        None
    } else {
        Some(text.to_string())
    }
}

fn strip_affix(text: &str, visible: &str) -> Option<String> {
    if visible.is_empty() {
        return None;
    }
    if let Some(rest) = text.strip_prefix(visible) {
        return Some(rest.trim().to_string());
    }
    text.strip_suffix(visible)
        .map(|rest| rest.trim().to_string())
}

/// After `session_finished`, re-read the conversation and attach stored thinking.
/// The answer was already streamed; this only fills the collapsed step.
#[cfg(target_arch = "wasm32")]
pub(super) fn adopt_when_finished(
    api_base: String,
    session: dioxus::prelude::Signal<Option<String>>,
    mut messages: dioxus::prelude::Signal<Vec<ChatMsg>>,
) {
    use dioxus::prelude::{ReadableExt, WritableExt};
    let Some(id) = session.read().clone() else {
        return;
    };
    wasm_bindgen_futures::spawn_local(async move {
        if let Ok(loaded) = super::fetch_conversation(&api_base, &id).await {
            messages.with_mut(|live| merge_history(live, &loaded.messages));
        }
    });
}
