//! Separate model thinking from the user-visible answer.
//!
//! MiniMax's OpenAI-compatible API writes reasoning into `content` as
//! `<think>...</think>` unless the request sets `reasoning_split`. The separate
//! `reasoning_content` / `reasoning_details` fields are that hidden channel.
//! Closed blocks leave the visible answer and are returned as thinking. A think
//! block that opens the message and never closes is the whole reply: visible
//! text is empty and the interior is thinking. A later unclosed `<think>` stays
//! in the answer, so a reply that mentions the tag is not eaten.

const OPEN: &[u8] = b"<think>";
const CLOSE: &[u8] = b"</think>";

/// Assistant text with thinking blocks removed. Other roles pass through.
pub fn visible_transcript(role: &str, content: String) -> String {
    if role == "assistant" {
        visible_answer(&content)
    } else {
        content
    }
}

/// `content` with `<think>` blocks removed. Unchanged when the text has none.
pub fn visible_answer(raw: &str) -> String {
    split_answer(raw).0
}

/// Visible answer and the thinking that was inside `<think>` blocks.
///
/// `None` thinking means there was no real think block (an answer-only reply,
/// or a later unclosed tag that is part of the answer).
pub fn split_answer(raw: &str) -> (String, Option<String>) {
    let mut filter = ThinkFilter::default();
    let mut visible = filter.push(raw);
    visible.push_str(&filter.finish());
    (visible, filter.take_reasoning())
}

/// Streaming counterpart of [`visible_answer`]. Chunk boundaries may split a tag.
#[derive(Default)]
pub(crate) struct ThinkFilter {
    pending: String,
    /// Open tag plus interior captured while inside a block, for an unclosed non-leading tag.
    block: String,
    leading_ws: String,
    inside: bool,
    started: bool,
    removed: bool,
    reasoning: String,
}

impl ThinkFilter {
    pub(crate) fn push(&mut self, chunk: &str) -> String {
        self.pending.push_str(chunk);
        let mut out = String::new();
        loop {
            if self.inside {
                if let Some(idx) = find_tag(&self.pending, CLOSE) {
                    self.block.push_str(&self.pending[..idx]);
                    self.pending.drain(..idx + CLOSE.len());
                    self.keep_closed();
                    self.block.clear();
                    self.inside = false;
                    self.removed = true;
                    continue;
                }
                let hold = held_suffix(&self.pending, CLOSE);
                let take = self.pending.len() - hold;
                let interior: String = self.pending.drain(..take).collect();
                self.block.push_str(&interior);
                break;
            } else if let Some(idx) = find_tag(&self.pending, OPEN) {
                let prefix: String = self.pending.drain(..idx).collect();
                self.emit_visible(&prefix, &mut out);
                let open: String = self.pending.drain(..OPEN.len()).collect();
                self.block = open;
                self.inside = true;
                if !self.started {
                    self.leading_ws.clear();
                }
            } else {
                let hold = held_suffix(&self.pending, OPEN);
                let emit_to = self.pending.len() - hold;
                let prefix: String = self.pending.drain(..emit_to).collect();
                self.emit_visible(&prefix, &mut out);
                break;
            }
        }
        out
    }

    pub(crate) fn finish(&mut self) -> String {
        if self.inside {
            if !self.started {
                self.keep_leading_unclosed();
                return String::new();
            }
            let mut rest = std::mem::take(&mut self.block);
            rest.push_str(&self.pending);
            self.pending.clear();
            self.inside = false;
            let mut out = String::new();
            self.emit_visible(&rest, &mut out);
            return out;
        }
        let rest = std::mem::take(&mut self.pending);
        if !self.started {
            if self.removed {
                return String::new();
            }
            let mut out = std::mem::take(&mut self.leading_ws);
            out.push_str(&rest);
            return out;
        }
        let mut out = String::new();
        self.emit_visible(&rest, &mut out);
        out
    }

    fn emit_visible(&mut self, text: &str, out: &mut String) {
        if text.is_empty() {
            return;
        }
        if self.started {
            out.push_str(text);
            return;
        }
        if text.trim().is_empty() {
            self.leading_ws.push_str(text);
            return;
        }
        if self.removed {
            let combined = format!("{}{text}", self.leading_ws);
            out.push_str(combined.trim_start());
        } else {
            out.push_str(&self.leading_ws);
            out.push_str(text);
        }
        self.leading_ws.clear();
        self.started = true;
    }

    fn keep_closed(&mut self) {
        let owned = strip_open_tag(&self.block).to_string();
        self.note(&owned);
    }

    fn keep_leading_unclosed(&mut self) {
        let mut all = std::mem::take(&mut self.block);
        all.push_str(&self.pending);
        self.pending.clear();
        self.leading_ws.clear();
        self.note(strip_open_tag(&all));
    }

    fn note(&mut self, text: &str) {
        let text = text.trim();
        if text.is_empty() {
            return;
        }
        if !self.reasoning.is_empty() {
            self.reasoning.push('\n');
        }
        self.reasoning.push_str(text);
    }

    fn take_reasoning(&mut self) -> Option<String> {
        let text = std::mem::take(&mut self.reasoning);
        let trimmed = text.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    }
}

fn strip_open_tag(text: &str) -> &str {
    let bytes = text.as_bytes();
    if bytes.len() >= OPEN.len() && bytes[..OPEN.len()].eq_ignore_ascii_case(OPEN) {
        &text[OPEN.len()..]
    } else {
        text
    }
}

fn find_tag(hay: &str, tag: &[u8]) -> Option<usize> {
    let bytes = hay.as_bytes();
    let n = tag.len();
    if bytes.len() < n {
        return None;
    }
    let last = bytes.len() - n;
    let mut i = 0;
    while i <= last {
        if hay.is_char_boundary(i) && bytes[i..i + n].eq_ignore_ascii_case(tag) {
            return Some(i);
        }
        i += 1;
    }
    None
}

fn held_suffix(text: &str, tag: &[u8]) -> usize {
    let bytes = text.as_bytes();
    let max = bytes.len().min(tag.len().saturating_sub(1));
    for len in (1..=max).rev() {
        let start = bytes.len() - len;
        if text.is_char_boundary(start) && bytes[start..].eq_ignore_ascii_case(&tag[..len]) {
            return len;
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn through_filter(raw: &str, size: usize) -> String {
        let mut filter = ThinkFilter::default();
        let mut got = String::new();
        let mut rest = raw;
        while !rest.is_empty() {
            let mut end = size.min(rest.len());
            while !rest.is_char_boundary(end) {
                end -= 1;
            }
            if end == 0 {
                end = rest
                    .chars()
                    .next()
                    .map(|c| c.len_utf8())
                    .unwrap_or(rest.len());
            }
            let (chunk, next) = rest.split_at(end);
            got.push_str(&filter.push(chunk));
            rest = next;
        }
        got.push_str(&filter.finish());
        got
    }

    fn assert_hidden(raw: &str, expect: &str) {
        assert_eq!(visible_answer(raw), expect, "whole: {raw:?}");
        for size in [1usize, 2, 3, 5, 7, 8, 64] {
            assert_eq!(
                through_filter(raw, size),
                expect,
                "chunks of {size}: {raw:?}"
            );
        }
    }

    #[test]
    fn a_leading_think_block_is_not_part_of_the_answer() {
        assert_hidden(
            "<think>\nsecret step\n</think>\n\nThe specialist is ready.",
            "The specialist is ready.",
        );
    }

    #[test]
    fn the_tag_match_is_case_insensitive() {
        assert_hidden("<THINK>\nhidden\n</Think>\n\nVisible", "Visible");
    }

    #[test]
    fn several_closed_blocks_all_go() {
        assert_hidden("<think>one</think>\nA\n<think>two</think>\nB", "A\n\nB");
    }

    #[test]
    fn an_unclosed_leading_think_hides_the_whole_reply() {
        assert_hidden("<think>\nstill thinking", "");
        assert_hidden("  <think>only this", "");
    }

    #[test]
    fn a_later_unclosed_tag_stays_in_the_answer() {
        assert_hidden("Use <think> like this", "Use <think> like this");
    }

    #[test]
    fn text_without_a_think_tag_is_unchanged() {
        assert_hidden("\nHi", "\nHi");
        assert_hidden("Hello", "Hello");
        assert_hidden("", "");
    }

    #[test]
    fn a_think_block_between_words_leaves_the_words() {
        assert_hidden("A <think>x</think>", "A ");
        assert_hidden("A <think>x</think> B", "A  B");
        assert_hidden("héllo <think>x</think> there", "héllo  there");
    }

    #[test]
    fn only_the_assistant_transcript_is_filtered() {
        let raw = "<think>\nx\n</think>\n\nHi".to_string();
        assert_eq!(visible_transcript("assistant", raw.clone()), "Hi");
        assert_eq!(visible_transcript("user", raw.clone()), raw);
        assert_eq!(
            visible_transcript("tool", "<think>x</think>".into()),
            "<think>x</think>"
        );
    }

    #[test]
    fn parsed_response_keeps_the_reasoning_channel_out_of_content() {
        use crate::openai_compat::{ToolNameMap, from_openai_response};
        use serde_json::json;

        let v = json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": "<think>\nhidden\n</think>\n\nVisible",
                    "reasoning_content": "hidden",
                    "reasoning_details": [{ "type": "reasoning.text", "text": "hidden" }]
                },
                "finish_reason": "stop"
            }]
        });
        let resp = from_openai_response(&v, &ToolNameMap::default()).unwrap();
        assert_eq!(resp.content.as_deref(), Some("Visible"));
        assert_eq!(resp.reasoning.as_deref(), Some("hidden"));
        assert!(!resp.content.as_deref().unwrap_or("").contains("hidden"));
    }

    #[test]
    fn split_answer_keeps_think_interiors_out_of_the_visible_text() {
        let (visible, reasoning) = split_answer("<think>\nsecret step\n</think>\n\nReady");
        assert_eq!(visible, "Ready");
        assert_eq!(reasoning.as_deref(), Some("secret step"));
        let (only, thought) = split_answer("<think>only this");
        assert_eq!(only, "");
        assert_eq!(thought.as_deref(), Some("only this"));
        let (answer, none) = split_answer("Just the answer");
        assert_eq!(answer, "Just the answer");
        assert_eq!(none, None);
        let (mentioned, not_thinking) = split_answer("Use <think> like this");
        assert_eq!(mentioned, "Use <think> like this");
        assert_eq!(not_thinking, None);
    }
}
