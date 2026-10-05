//! SSE assembly for an OpenAI-compatible chat-completions response.
//!
//! Split from `openai_compat` so the streaming loop can hide `<think>` blocks
//! without pushing that file over its cyclomatic review boundary.
//!
//! Chunks are decoded as UTF-8 only up to the last complete character. A
//! multibyte character split across HTTP reads used to become U+FFFD via
//! `from_utf8_lossy` and that replacement was then parsed as the JSON string.

use futures::StreamExt;
use std::pin::Pin;

use crate::{CompletionStream, ProviderError, StreamItem};

use super::ToolNameMap;
use super::sse_decode::{TurnAccum, Utf8Carry};

/// Drive an already-successful (status checked by the caller) OpenAI-compatible SSE response body
/// into a [`CompletionStream`].
///
/// Content tokens are the visible answer. A `<think>` block, including one split across chunks,
/// is not yielded as tokens. Its interior, plus `reasoning_content` / `reasoning_details`, is
/// stored on [`crate::CompletionResponse::reasoning`] of the final [`StreamItem::Done`].
pub fn stream_sse_response(response: reqwest::Response, name_map: ToolNameMap) -> CompletionStream {
    let chunks = response.bytes_stream().map(owned_chunk);
    stream_chunk_stream(Box::pin(chunks), name_map)
}

fn owned_chunk(item: Result<impl AsRef<[u8]>, impl ToString>) -> Result<Vec<u8>, String> {
    match item {
        Ok(bytes) => Ok(bytes.as_ref().to_vec()),
        Err(err) => Err(err.to_string()),
    }
}

pub(crate) fn stream_chunk_stream(
    mut chunks: Pin<Box<dyn futures::Stream<Item = Result<Vec<u8>, String>> + Send>>,
    name_map: ToolNameMap,
) -> CompletionStream {
    let stream = async_stream::try_stream! {
        let mut carry = Utf8Carry::default();
        let mut buf = String::new();
        let mut turn = TurnAccum::default();

        while let Some(chunk) = chunks.next().await {
            let chunk = chunk.map_err(ProviderError::Transport)?;
            buf.push_str(&carry.push(&chunk));

            while let Some(nl) = buf.find('\n') {
                let line: String = buf.drain(..=nl).collect();
                if let Some(visible) = turn.on_line(&line, &name_map) {
                    yield StreamItem::Token(visible);
                }
            }
        }

        for item in turn.finish(&name_map) {
            yield item;
        }
    };

    Box::pin(stream)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::StreamItem;
    use futures::StreamExt;

    async fn collect(bytes: Vec<Vec<u8>>) -> (String, crate::CompletionResponse) {
        let chunks = futures::stream::iter(bytes.into_iter().map(Ok::<_, String>));
        let mut stream = stream_chunk_stream(Box::pin(chunks), ToolNameMap::default());
        let mut tokens = String::new();
        let mut done = None;
        while let Some(item) = stream.next().await {
            match item.expect("stream item") {
                StreamItem::Token(text) => {
                    assert!(
                        !text.contains('\u{FFFD}'),
                        "token replaced a character: {text:?}"
                    );
                    tokens.push_str(&text);
                }
                StreamItem::Done(resp) => done = Some(resp),
            }
        }
        (tokens, done.expect("stream finished"))
    }

    #[tokio::test]
    async fn a_multibyte_character_split_across_chunks_is_not_replaced() {
        let dash = "\u{2014}";
        let emoji = "\u{1F604}";
        let content = format!("Hello {dash} {emoji} there");
        let payload = serde_json::json!({
            "choices": [{ "delta": { "content": content } }]
        });
        let frame = format!("data: {payload}\n");
        let dash_at = frame.find(dash).expect("em dash");
        let emoji_at = frame.find(emoji).expect("emoji");
        assert!(
            !frame.is_char_boundary(dash_at + 1),
            "split must land inside the em dash"
        );
        assert!(
            !frame.is_char_boundary(emoji_at + 2),
            "split must land inside the emoji"
        );

        // Two cuts: first chunk ends mid-dash, second ends mid-emoji, third finishes the frame.
        let bytes = frame.as_bytes();
        let chunks = vec![
            bytes[..dash_at + 1].to_vec(),
            bytes[dash_at + 1..emoji_at + 2].to_vec(),
            bytes[emoji_at + 2..].to_vec(),
        ];
        let (tokens, done) = collect(chunks).await;
        let visible = done.content.clone().unwrap_or_default();
        assert!(
            tokens.contains(dash) && tokens.contains(emoji),
            "{tokens:?}"
        );
        assert!(
            visible.contains(dash) && visible.contains(emoji),
            "{visible:?}"
        );
        assert!(!tokens.contains('\u{FFFD}'));
        assert!(!visible.contains('\u{FFFD}'));
        assert_eq!(done.reasoning, None);
    }

    #[tokio::test]
    async fn a_think_block_split_across_chunks_is_reasoning_not_the_answer() {
        let sse = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"<thi\"}}]}\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"nk>\\nsecret\\n</think>\\n\\nHello\"}}]}\n",
            "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"secret\"}}]}\n",
            "data: {\"choices\":[{\"finish_reason\":\"stop\"}]}\n",
            "data: [DONE]\n",
        );
        let (tokens, done) = collect(vec![sse.as_bytes().to_vec()]).await;
        assert_eq!(tokens, "Hello");
        assert_eq!(done.content.as_deref(), Some("Hello"));
        assert_eq!(done.reasoning.as_deref(), Some("secret"));
        assert!(!tokens.contains("secret"));
    }

    #[tokio::test]
    async fn think_only_keeps_an_empty_answer_and_a_thinking_step() {
        let sse = "data: {\"choices\":[{\"delta\":{\"content\":\"<think>only</think>\"}}]}\n";
        let (tokens, done) = collect(vec![sse.as_bytes().to_vec()]).await;
        assert!(tokens.is_empty());
        assert_eq!(done.content, None);
        assert_eq!(done.reasoning.as_deref(), Some("only"));
    }

    #[tokio::test]
    async fn prose_beside_a_tool_call_stays_visible() {
        let sse = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"I'll look that up\"}}]}\n",
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"c1\",\"function\":{\"name\":\"search\",\"arguments\":\"{}\"}}]}}]}\n",
            "data: {\"choices\":[{\"finish_reason\":\"tool_calls\"}]}\n",
        );
        let (tokens, done) = collect(vec![sse.as_bytes().to_vec()]).await;
        assert_eq!(tokens, "I'll look that up");
        assert_eq!(done.content.as_deref(), Some("I'll look that up"));
        assert_eq!(done.reasoning, None);
        assert_eq!(done.tool_calls.len(), 1);
    }
}
