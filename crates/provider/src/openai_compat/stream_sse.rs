//! SSE assembly for an OpenAI-compatible chat-completions response.
//!
//! Split from `openai_compat` so the streaming loop can hide `<think>` blocks
//! without pushing that file over its cyclomatic review boundary.

use futures::StreamExt;
use serde_json::Value;

use crate::{
    CompletionResponse, CompletionStream, FinishReason, ProviderError, StreamItem, ToolInvocation,
    Usage,
};

use super::{ToolAcc, ToolNameMap, accumulate_tool_deltas, map_finish_reason, parse_usage};

/// Drive an already-successful (status checked by the caller) OpenAI-compatible SSE response body
/// into a [`CompletionStream`]: parse each `data:` line as a chunk with a `delta`, emit content
/// deltas as [`StreamItem::Token`], accumulate tool-call deltas and the finish reason, then emit the
/// assembled response as the final [`StreamItem::Done`].
///
/// Extracted from `liberado-provider-deepseek`/`liberado-provider-openrouter`, which had this loop
/// duplicated verbatim (`docs/future-work/archive/hygiene-audit-2026-07-05.md`) — unlike the request/response
/// mapping functions above (already shared before this), the streaming loop is where chunk-boundary
/// bugs actually hide, so it's the part most worth not maintaining twice. Callers own the HTTP POST,
/// status-code check, and building `name_map` (via [`build_tool_name_map`](super::build_tool_name_map))
/// — this only owns turning a 200 response's body into normalized stream items.
///
/// Content tokens are the visible answer. A `<think>` block, including one split across chunks,
/// is not yielded and is not stored on [`StreamItem::Done`].
pub fn stream_sse_response(response: reqwest::Response, name_map: ToolNameMap) -> CompletionStream {
    let stream = async_stream::try_stream! {
        let mut bytes = response.bytes_stream();
        let mut buf = String::new();
        let mut raw_content = String::new();
        let mut think = crate::think_text::ThinkFilter::default();
        let mut tools: Vec<ToolAcc> = Vec::new();
        let mut finish = FinishReason::Stop;
        // Populated by the trailing usage chunk when the request sets `stream_options.include_usage`
        // (that chunk carries `usage` and an empty `choices`); `None` if the backend omits it.
        let mut usage: Option<Usage> = None;

        while let Some(chunk) = bytes.next().await {
            let chunk = chunk.map_err(|e| ProviderError::Transport(e.to_string()))?;
            buf.push_str(&String::from_utf8_lossy(&chunk));

            while let Some(nl) = buf.find('\n') {
                let line: String = buf.drain(..=nl).collect();
                let Some(data) = line.trim().strip_prefix("data:") else { continue };
                let data = data.trim();
                if data.is_empty() || data == "[DONE]" {
                    continue;
                }
                let Ok(v) = serde_json::from_str::<Value>(data) else { continue };
                if let Some(u) = parse_usage(&v["usage"]) {
                    usage = Some(u);
                }
                let choice = &v["choices"][0];
                if let Some(fr) = choice["finish_reason"].as_str() {
                    finish = map_finish_reason(fr);
                }
                let delta = &choice["delta"];
                if let Some(t) = delta["content"].as_str()
                    && !t.is_empty()
                {
                    raw_content.push_str(t);
                    let visible = think.push(t);
                    if !visible.is_empty() {
                        yield StreamItem::Token(visible);
                    }
                }
                if let Some(deltas) = delta["tool_calls"].as_array() {
                    accumulate_tool_deltas(&mut tools, deltas);
                }
            }
        }

        let tool_calls: Vec<ToolInvocation> =
            tools.into_iter().filter_map(|acc| acc.into_invocation(&name_map)).collect();
        let tail = think.finish();
        if !tail.is_empty() {
            yield StreamItem::Token(tail);
        }
        // Persist the pure strip, so a chunk-boundary miss cannot store the thinking.
        let visible = crate::think_text::visible_answer(&raw_content);
        yield StreamItem::Done(CompletionResponse {
            content: (!visible.is_empty()).then_some(visible),
            tool_calls,
            finish_reason: finish,
            usage,
        });
    };

    Box::pin(stream)
}
