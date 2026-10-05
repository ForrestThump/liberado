//! Byte-safe SSE line assembly and per-turn reasoning capture.
//!
//! A multibyte UTF-8 character can be split across HTTP chunks. Decoding each
//! chunk with `from_utf8_lossy` inserts U+FFFD, and that replacement is then
//! parsed as the JSON string and stored. Newlines are a single byte and cannot
//! sit inside a character, but a chunk boundary can still fall mid-character
//! *before* the line's newline arrives. Hold at most the three incomplete
//! trailing bytes and decode only a valid prefix.

use serde_json::Value;

use crate::think_text::{ThinkFilter, split_answer};
use crate::{CompletionResponse, FinishReason, StreamItem, ToolInvocation, Usage};

use super::{ToolAcc, ToolNameMap, accumulate_tool_deltas, map_finish_reason, parse_usage};
use crate::reasoning_text::{ChannelAccum, merge};

/// Incomplete tail of a UTF-8 sequence. A character is at most 4 bytes, so three
/// bytes is the longest prefix that is still not a character.
#[derive(Default)]
pub(crate) struct Utf8Carry {
    pending: Vec<u8>,
}

impl Utf8Carry {
    /// Append `chunk` and return only the valid UTF-8 prefix. At most three
    /// incomplete trailing bytes stay here for the next chunk. A byte sequence
    /// that cannot be a character is dropped and is never replaced with U+FFFD.
    pub(crate) fn push(&mut self, chunk: &[u8]) -> String {
        self.pending.extend_from_slice(chunk);
        let mut out = String::new();
        loop {
            if self.pending.is_empty() {
                break;
            }
            match std::str::from_utf8(&self.pending) {
                Ok(text) => {
                    out.push_str(text);
                    self.pending.clear();
                    break;
                }
                Err(err) => {
                    let valid = err.valid_up_to();
                    if valid > 0 {
                        out.push_str(std::str::from_utf8(&self.pending[..valid]).unwrap_or(""));
                        self.pending.drain(..valid);
                    }
                    if err.error_len().is_none() {
                        if self.pending.len() > 3 {
                            let drop_n = self.pending.len() - 3;
                            self.pending.drain(..drop_n);
                        }
                        break;
                    }
                    let bad = err.error_len().unwrap_or(1).max(1);
                    let drop_n = bad.min(self.pending.len()).max(1).min(self.pending.len());
                    if drop_n == 0 {
                        break;
                    }
                    self.pending.drain(..drop_n);
                }
            }
        }
        out
    }
}

/// One streamed completion: visible tokens, hidden thinking, tool calls.
#[derive(Default)]
pub(crate) struct TurnAccum {
    raw_content: String,
    think: ThinkFilter,
    channel: ChannelAccum,
    tools: Vec<ToolAcc>,
    finish: Option<FinishReason>,
    usage: Option<Usage>,
}

impl TurnAccum {
    pub(crate) fn on_line(&mut self, line: &str, _name_map: &ToolNameMap) -> Option<String> {
        let data = line.trim().strip_prefix("data:")?.trim();
        if data.is_empty() || data == "[DONE]" {
            return None;
        }
        let v: Value = serde_json::from_str(data).ok()?;
        self.absorb(&v)
    }

    fn absorb(&mut self, v: &Value) -> Option<String> {
        if let Some(u) = parse_usage(&v["usage"]) {
            self.usage = Some(u);
        }
        let choice = &v["choices"][0];
        if let Some(fr) = choice["finish_reason"].as_str() {
            self.finish = Some(map_finish_reason(fr));
        }
        self.note_channel(&choice["delta"]);
        self.note_channel(&choice["message"]);
        if let Some(deltas) = choice["delta"]["tool_calls"].as_array() {
            accumulate_tool_deltas(&mut self.tools, deltas);
        }
        self.push_content(choice["delta"]["content"].as_str().unwrap_or(""))
    }

    fn note_channel(&mut self, obj: &Value) {
        if let Some(text) = obj.get("reasoning_content").and_then(Value::as_str) {
            self.channel.note_content(text);
        }
        if let Some(details) = obj.get("reasoning_details") {
            self.channel.note_details(details);
        }
    }

    fn push_content(&mut self, text: &str) -> Option<String> {
        if text.is_empty() {
            return None;
        }
        self.raw_content.push_str(text);
        let visible = self.think.push(text);
        if visible.is_empty() {
            None
        } else {
            Some(visible)
        }
    }

    pub(crate) fn finish(&mut self, name_map: &ToolNameMap) -> Vec<StreamItem> {
        let mut items = Vec::new();
        let tail = self.think.finish();
        if !tail.is_empty() {
            items.push(StreamItem::Token(tail));
        }
        let (visible, tagged) = split_answer(&self.raw_content);
        let reasoning = merge(tagged, self.channel.finish());
        let tool_calls: Vec<ToolInvocation> = std::mem::take(&mut self.tools)
            .into_iter()
            .filter_map(|acc| acc.into_invocation(name_map))
            .collect();
        items.push(StreamItem::Done(CompletionResponse {
            content: (!visible.is_empty()).then_some(visible),
            tool_calls,
            finish_reason: self.finish.unwrap_or(FinishReason::Stop),
            usage: self.usage.take(),
            reasoning,
        }));
        items
    }
}
