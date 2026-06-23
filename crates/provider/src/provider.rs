//! The [`Provider`] trait — Liberado's narrow waist for all inference.

use async_trait::async_trait;
use serde::de::DeserializeOwned;

use crate::error::{ProviderError, ProviderResult};
use crate::types::{CompletionRequest, CompletionResponse};

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
    /// The underlying model id (for tracing + role validation).
    fn model(&self) -> &str;

    /// Run one completion turn.
    async fn complete(&self, request: CompletionRequest) -> ProviderResult<CompletionResponse>;
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
    serde_json::from_str(&content).map_err(|e| ProviderError::Decode(e.to_string()))
}
