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
    loop {
        match complete_json_once::<P, T>(provider, request.clone()).await {
            Ok(value) => return Ok(value),
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
            Err(e) => return Err(e),
        }
    }
}

async fn complete_json_once<P, T>(provider: &P, request: CompletionRequest) -> ProviderResult<T>
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
            window_around(&content, e.column())
        ))
    })
}

/// A bounded window of `s` centred on 1-based character `column`, marked with `⟪⟫` at the point the
/// parser gave up. Character-based throughout, so a multi-byte boundary can't be split.
fn window_around(s: &str, column: usize) -> String {
    const RADIUS: usize = 160;
    let chars: Vec<char> = s.chars().collect();
    // serde columns are 1-based; a 0 (or an out-of-range value) falls back to the start.
    let idx = column.saturating_sub(1).min(chars.len());
    let start = idx.saturating_sub(RADIUS);
    let end = (idx + RADIUS).min(chars.len());

    let mut out = String::new();
    if start > 0 {
        out.push('…');
    }
    out.extend(&chars[start..idx]);
    out.push_str("⟪HERE⟫");
    out.extend(&chars[idx..end]);
    if end < chars.len() {
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
            window_around(&bad, col)
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
        let w = window_around(&s, 1500);
        assert!(w.chars().count() < 400, "window must stay bounded");
        // Out-of-range and zero columns must not panic, nor must an empty reply.
        let _ = window_around(&s, 0);
        let _ = window_around(&s, 99_999);
        let _ = window_around("", 5);
    }
}
