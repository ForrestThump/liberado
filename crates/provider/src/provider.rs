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
    let response = provider.complete(request.with_json_schema(schema)).await?;
    let content = response.content.ok_or(ProviderError::EmptyResponse)?;
    serde_json::from_str(&content).map_err(|e| {
        // Carry a prefix of what the model actually said. Without it a decode failure is
        // unanswerable after the fact: the dispatcher logs "classification produced unusable
        // output" and the output itself is gone, so "was the prompt wrong, or did the provider
        // hiccup?" cannot be settled even with the logs in hand. A failed evening-debrief cron
        // could not be diagnosed for exactly this reason. Truncated, because the point is to see
        // the *shape* of the reply (prose instead of JSON, a fenced block, a refusal), not to
        // mirror a large body into the log.
        ProviderError::Decode(format!(
            "{e} — model said: {}",
            truncate_chars(&content, 400)
        ))
    })
}

/// First `max` characters of `s`, with an ellipsis when truncated. Character-based, so a multi-byte
/// boundary can't be split.
fn truncate_chars(s: &str, max: usize) -> String {
    let trimmed = s.trim();
    if trimmed.chars().count() <= max {
        return trimmed.to_string();
    }
    let head: String = trimmed.chars().take(max).collect();
    format!("{head}…")
}
