//! The [`Provider`] trait — Liberado's narrow waist for all inference.

use std::pin::Pin;

use async_trait::async_trait;
use futures::Stream;
use serde::de::DeserializeOwned;

use crate::error::{ProviderError, ProviderResult};
use crate::types::{CompletionRequest, CompletionResponse, StreamItem};

/// A streamed completion: a sequence of [`StreamItem`]s (incremental tokens, then a final `Done`).
/// Boxed so the [`Provider`] trait stays dyn-compatible.
pub type CompletionStream = Pin<Box<dyn Stream<Item = ProviderResult<StreamItem>> + Send>>;

/// A provider-agnostic inference backend.
///
/// One method does all the work: [`complete`](Provider::complete). Tool-calling and structured
/// output are expressed through the [`CompletionRequest`]/[`CompletionResponse`] types rather
/// than separate methods, which keeps the trait **dyn-compatible** — the daemon can hold a
/// `Box<dyn Provider>` (or `Arc<dyn Provider>`) and swap mock vs. real, or different models per
/// role, purely from config (Decision 13). Concrete implementations (a thin DeepSeek client,
/// or a rig-backed one) live in their own crates; this crate defines only the boundary and a
/// mock.
#[async_trait]
pub trait Provider: Send + Sync {
    /// The underlying model id (for tracing + role validation + status display).
    ///
    /// Returns an owned `String` so implementations can hot-swap the model under a lock
    /// without tying the return value to a short-lived guard.
    fn model(&self) -> String;

    /// Hot-swap the model used for subsequent completions (same base URL / API key).
    ///
    /// Default is a no-op success so test doubles and non-swappable backends stay simple.
    /// Production OpenAI-compatible providers override this. Does not validate the id against
    /// `list_models` — callers may pass any id the upstream accepts.
    fn set_model(&self, model: String) {
        let _ = model;
    }

    /// Run one completion turn.
    async fn complete(&self, request: CompletionRequest) -> ProviderResult<CompletionResponse>;

    /// Run one completion turn, **streaming** the result. The default forwards to
    /// [`complete`](Self::complete) and emits the whole answer as a single token then `Done`, so a
    /// non-streaming provider (a mock, or one without an SSE API) is transparently usable by the
    /// streaming agent loop. Real backends (DeepSeek) override this with token-by-token SSE.
    async fn complete_stream(
        &self,
        request: CompletionRequest,
    ) -> ProviderResult<CompletionStream> {
        let response = self.complete(request).await?;
        let mut items: Vec<ProviderResult<StreamItem>> = Vec::new();
        if let Some(content) = &response.content
            && !content.is_empty()
        {
            items.push(Ok(StreamItem::Token(content.clone())));
        }
        items.push(Ok(StreamItem::Done(response)));
        Ok(Box::pin(futures::stream::iter(items)))
    }

    /// List model ids the backend currently reports (OpenAI-compatible `GET /models`).
    ///
    /// Default is empty: not every backend implements `/models`, and the configured
    /// [`model`](Self::model) remains the authority for completions. OpenAI-compatible
    /// clients override this; the daemon exposes the result via `GET /api/models`.
    async fn list_models(&self) -> ProviderResult<Vec<String>> {
        Ok(Vec::new())
    }
}

/// One retry on an undecodable reply. Not a general retry policy — see [`complete_json`].
const DECODE_RETRIES: u32 = 1;

/// Run a completion in structured-output mode and deserialize the reply into `T`.
///
/// A free function rather than a trait method so the trait stays dyn-compatible (a generic
/// method would not be). This is the dispatcher's path to a typed `DispatchDecision`: pass the
/// schema, get back the parsed value, and map [`ProviderError::Decode`] into the
/// retry/repair/escalate flow (Decision 13).
pub async fn complete_json<P, T>(
    provider: &P,
    request: CompletionRequest,
    schema: serde_json::Value,
) -> ProviderResult<T>
where
    P: Provider + ?Sized,
    T: DeserializeOwned,
{
    let request = request.with_json_schema(schema);
    let mut attempt = 0;
    // The last raw reply, kept only so the give-up branch can log it in full. Held here rather than
    // threaded through the error because an error message is read by operators and a whole model
    // reply does not belong in one.
    let mut last_reply: Option<String> = None;
    loop {
        match complete_json_once::<P, T>(provider, request.clone(), &mut last_reply).await {
            Ok(value) => return Ok(value),
            // The backend refused the *request*, not the reply — an older route, or one that does
            // not implement `json_schema` response format. Retry once asking only for valid JSON.
            //
            // Without this, adding a schema turns "occasionally malformed" into "always fails" on
            // any backend that does not support it, which is a strictly worse trade: a constrained
            // decoder is an optimisation over prompt-only shaping, and an optimisation must not be
            // load-bearing. Degrading here keeps the floor exactly where it was.
            Err(ProviderError::InvalidRequest(msg)) if request.has_json_schema() => {
                tracing::warn!(
                    error = %msg,
                    "backend rejected the json_schema response format — falling back to json_object \
                     for this call. Structured decoding is unconstrained until this is resolved."
                );
                let relaxed = request.clone().without_json_schema();
                return complete_json_once::<P, T>(provider, relaxed, &mut last_reply).await;
            }
            // A reply that isn't the required shape is the one failure here worth re-asking about:
            // structured-output decoding is close to deterministic, so a malformed or empty reply
            // is usually a transient provider hiccup rather than a prompt the model refuses. One
            // extra call is cheap next to what a failure costs — an unattended cron gets no second
            // chance, and a live evening-debrief burned a whole run on a single bad reply.
            //
            // Deliberately narrow. Transport and rate-limit errors are NOT retried here: those have
            // their own backoff semantics at the caller, and silently re-issuing them from inside a
            // helper would hide load problems and double the spend on a provider already failing.
            Err(e @ (ProviderError::Decode(_) | ProviderError::EmptyResponse))
                if attempt < DECODE_RETRIES =>
            {
                attempt += 1;
                tracing::warn!(
                    attempt,
                    error = %e,
                    "structured output did not decode — retrying once"
                );
            }
            // Out of retries on an undecodable reply. Log the **whole** reply once, at debug, before
            // giving up.
            //
            // The windowed error above is the right thing in a warning — bounded, aimed at the
            // defect. But a window is a hypothesis about where the problem is, and the two crons
            // that failed on 2026-07-28 were diagnosed from a window that was itself misaligned.
            // When the retry has already failed, the reply is worth having in full: it is the only
            // artefact that settles what the model actually emitted, and by then it is not noise
            // because the dispatch is about to fail anyway.
            //
            // `debug`, so an ordinary run does not carry model output into the log, and a diagnosis
            // is one `RUST_LOG` away rather than a rebuild.
            Err(e @ (ProviderError::Decode(_) | ProviderError::EmptyResponse)) => {
                tracing::debug!(
                    error = %e,
                    reply = %last_reply.as_deref().unwrap_or("(none captured)"),
                    "structured output failed after retry — full reply follows"
                );
                return Err(e);
            }
            Err(e) => return Err(e),
        }
    }
}

async fn complete_json_once<P, T>(
    provider: &P,
    request: CompletionRequest,
    last_reply: &mut Option<String>,
) -> ProviderResult<T>
where
    P: Provider + ?Sized,
    T: DeserializeOwned,
{
    let response = provider.complete(request).await?;
    // Captured before `content` is moved out. Both are already on the response and were being
    // discarded, which is why a live truncation could only be *guessed* at from the parse column.
    //
    // `finish_reason` is the decisive one: `Length` means the backend stopped us at a token cap, so
    // the reply is a prefix and no amount of prompt work will fix it — a completely different repair
    // than a model emitting a stray token mid-object. `completion_tokens` next to it shows *where*
    // the ceiling sits (a suspiciously round number is a configured cap, ours or theirs).
    let finish = response.finish_reason;
    let completion_tokens = response.usage.map(|u| u.completion_tokens);
    let content = response.content.ok_or(ProviderError::EmptyResponse)?;
    *last_reply = Some(content.clone());
    serde_json::from_str(&content).map_err(|e| {
        // Carry what the model actually said, aimed at the *failure point*.
        //
        // The first version of this logged a 400-char prefix, which was one setting away from
        // useful: a live weekly-review cron failed with parse errors at columns 420, 1182 and 1306,
        // all past the window. It proved the reply *started* as valid JSON and said nothing about
        // why it stopped being valid.
        //
        // Two things settle it. The **total length** next to the error column distinguishes a
        // truncated reply (error at the very end) from a malformed one (error in the middle). And a
        // **window around the column** shows the offending bytes — a stray token, an unescaped
        // control character, a second object glued on. Bounded either way; the point is the defect,
        // not a mirror of the body.
        ProviderError::Decode(format!(
            "{e} — finish_reason={finish:?}, completion_tokens={}, reply was {} chars; \
             around the failure: {}",
            completion_tokens
                .map(|t| t.to_string())
                .unwrap_or_else(|| "unreported".into()),
            content.chars().count(),
            window_around(&content, e.line(), e.column())
        ))
    })
}

/// A bounded window of `s` around the parser's failure point, marked with `⟪HERE⟫`.
///
/// # `column` is a BYTE offset, not a character one
///
/// This is the whole subtlety, and getting it wrong made the diagnostic lie. `serde_json` reports
/// `line`/`column` where the column counts **bytes** within that line — `from_str` on the 21-char,
/// 27-byte `{"a": "— — —", "b": }` reports column 27. Indexing a `Vec<char>` with that number walked
/// past the failure (and off the end entirely, where the old `.min(len)` silently clamped to the end
/// of the reply).
///
/// So every window printed for a reply containing any non-ASCII — an em dash or curly quote in the
/// model's own `rationale`, which is most of them — pointed too far right, and the further into the
/// reply the failure was, the further off it landed. The two live cron failures on 2026-07-28 were
/// read from exactly such a window.
///
/// The previous version's comment claimed to be "character-based throughout, so a multi-byte
/// boundary can't be split" — true of the *output*, and the reason the bug was invisible: it never
/// panicked, it just aimed wrong.
fn window_around(s: &str, line: usize, column: usize) -> String {
    const RADIUS: usize = 160;

    // Byte offset of the start of `line` (1-based), then the byte column within it.
    let line_start = if line <= 1 {
        0
    } else {
        s.match_indices('\n')
            .nth(line - 2)
            .map(|(i, _)| i + 1)
            .unwrap_or(0)
    };
    let mut byte_idx = line_start
        .saturating_add(column.saturating_sub(1))
        .min(s.len());
    // Snap left onto a boundary: a column can land mid-character when the offending byte *is* a
    // malformed multi-byte sequence.
    while byte_idx > 0 && !s.is_char_boundary(byte_idx) {
        byte_idx -= 1;
    }

    let (before, after) = s.split_at(byte_idx);
    let head: String = {
        let kept: Vec<char> = before.chars().rev().take(RADIUS).collect();
        kept.into_iter().rev().collect()
    };
    let tail: String = after.chars().take(RADIUS).collect();

    let mut out = String::new();
    if before.chars().count() > head.chars().count() {
        out.push('…');
    }
    out.push_str(&head);
    out.push_str("⟪HERE⟫");
    out.push_str(&tail);
    if after.chars().count() > tail.chars().count() {
        out.push('…');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The decode-failure window must aim at the failure, not the start of the reply.
    ///
    /// The first version logged a 400-char prefix, and a live weekly-review cron failed at column
    /// 1182 — proving only that the reply *started* as valid JSON. Total length next to the error
    /// column is what separates a truncated reply from a malformed one.
    #[test]
    fn a_decode_failure_reports_length_and_a_window_at_the_column() {
        // Valid JSON for ~600 chars, then a bare token where a value belongs.
        let bad = format!("{{\"a\":\"{}\",\"b\":oops}}", "x".repeat(600));
        let err = serde_json::from_str::<serde_json::Value>(&bad).unwrap_err();
        let col = err.column();
        assert!(
            col > 400,
            "fixture must fail past the old 400-char window, got {col}"
        );

        let rendered = format!(
            "{err} — reply was {} chars; around the failure: {}",
            bad.chars().count(),
            window_around(&bad, err.line(), col)
        );
        assert!(rendered.contains("⟪HERE⟫"), "{rendered}");
        assert!(
            rendered.contains("oops"),
            "the offending token must be visible: {rendered}"
        );
        assert!(rendered.contains("reply was 617 chars"), "{rendered}");
    }

    /// A truncated reply must be *identifiable as truncated*, not inferred from a parse column.
    /// `finish_reason=Length` is the backend saying it stopped us at a cap — a different repair
    /// entirely from a model emitting a stray token mid-object.
    #[tokio::test]
    async fn a_capped_reply_reports_finish_reason_and_completion_tokens() {
        use crate::{CompletionResponse, FinishReason, MockProvider, Usage};

        // A reply cut off mid-string, exactly as a token cap produces.
        let truncated = format!("{{\"action\":\"{}", "x".repeat(500));
        let capped = CompletionResponse {
            content: Some(truncated),
            tool_calls: Vec::new(),
            finish_reason: FinishReason::Length,
            usage: Some(Usage {
                prompt_tokens: 1200,
                completion_tokens: 1024,
                total_tokens: 2224,
                cached_prompt_tokens: None,
            }),
        };
        let mock = MockProvider::with_script("m", [capped.clone(), capped]);

        let err = complete_json::<MockProvider, serde_json::Value>(
            &mock,
            CompletionRequest::new(vec![]),
            serde_json::json!({}),
        )
        .await
        .unwrap_err();

        let msg = err.to_string();
        assert!(msg.contains("finish_reason=Length"), "{msg}");
        assert!(msg.contains("completion_tokens=1024"), "{msg}");
        assert!(msg.contains("⟪HERE⟫"), "{msg}");
    }

    #[test]
    fn the_window_is_bounded_and_multibyte_safe() {
        let s = "🎉".repeat(2000);
        let w = window_around(&s, 1, 1500);
        assert!(w.chars().count() < 400, "window must stay bounded");
        // Out-of-range and zero columns must not panic, nor must an empty reply.
        let _ = window_around(&s, 1, 0);
        let _ = window_around(&s, 1, 99_999);
        let _ = window_around("", 1, 5);
        let _ = window_around(&s, 99_999, 1);
    }

    /// The bug this window was built to avoid, and then had: `serde_json`'s column counts **bytes**,
    /// so indexing characters with it aims past the failure — further past it the more non-ASCII the
    /// reply contains. A model's own `rationale` prose is full of em dashes, so this was the normal
    /// case, not an edge one.
    #[test]
    fn the_window_points_at_the_real_failure_when_the_reply_has_multibyte_text() {
        // 21 chars, 27 bytes. The offending `}` is the last byte; serde reports column 27.
        let reply = r#"{"a": "— — —", "b": }"#;
        let err = serde_json::from_str::<serde_json::Value>(reply).unwrap_err();
        assert_eq!(
            err.column(),
            27,
            "precondition: serde reports a byte column"
        );
        assert!(
            err.column() > reply.chars().count(),
            "precondition: the byte column is past the end of the char sequence, which is exactly \
             what silently clamped the old window to the end of the reply"
        );

        let w = window_around(reply, err.line(), err.column());
        let (before, after) = w.split_once("⟪HERE⟫").expect("marker present");
        assert!(
            before.ends_with("\"b\": "),
            "the marker must sit just before the offending token, got: {w}"
        );
        assert!(
            after.starts_with('}'),
            "and the offending token must follow it, got: {w}"
        );
    }

    /// A column is relative to its *line*, so a multi-line reply needs the line offset added or the
    /// window lands near the start of the whole body.
    #[test]
    fn the_window_accounts_for_the_line_the_error_is_on() {
        let reply = "{\n  \"a\": 1,\n  \"b\": }\n}";
        let err = serde_json::from_str::<serde_json::Value>(reply).unwrap_err();
        assert!(err.line() > 1, "precondition: the failure is not on line 1");

        let w = window_around(reply, err.line(), err.column());
        let (before, after) = w.split_once("⟪HERE⟫").expect("marker present");
        assert!(before.ends_with("\"b\": "), "got: {w}");
        assert!(after.starts_with('}'), "got: {w}");
    }
}
