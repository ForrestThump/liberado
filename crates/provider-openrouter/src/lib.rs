//! # liberado-provider-openrouter
//!
//! A concrete [`Provider`] backed by [OpenRouter](https://openrouter.ai)'s OpenAI-compatible
//! chat-completions API. OpenRouter fronts many models (across many vendors) behind one endpoint
//! and one API key — the reason to reach for it here is concurrency: running many candidate
//! evaluations at once (`liberado-heuristics-tuner`) without getting rate-limited by a single
//! upstream provider's per-key limits.
//!
//! Like [`liberado_provider_deepseek`], this is a thin translation layer: map the normalized
//! [`CompletionRequest`] to the OpenAI wire shape, POST it, map the response back. The
//! wire-format translation itself (tool-name sanitization, request/response mapping, SSE stream
//! assembly) lives in `liberado_provider::openai_compat`, shared with `liberado-provider-deepseek`
//! since both backends speak the same OpenAI-compatible format — see that module's doc comment
//! for why (`docs/roadmap/hygiene-audit-2026-07-04.md`). This crate only owns the actual HTTP
//! round-trip and its own defaults/env-var names/status-code mapping.
//!
//! **OpenRouter-specific notes**:
//! - Tool-calling and JSON mode support vary **by routed model** — OpenRouter passes the request
//!   through, it doesn't normalize capability gaps. Picking a capable model is the caller's job
//!   (same as Decision 13's role-tiered model floors elsewhere in this system).
//! - `402` (insufficient account credits) is a real OpenRouter response code, mapped alongside the
//!   other client-error statuses to [`ProviderError::InvalidRequest`] (see [`map_status`]).

use async_trait::async_trait;
use liberado_provider::openai_compat::{
    build_tool_name_map, from_openai_response, stream_sse_response, to_openai_request,
};
use liberado_provider::{
    CompletionRequest, CompletionResponse, CompletionStream, Provider, ProviderError,
    ProviderResult,
};
use serde_json::{Value, json};

/// A broadly-available, cheap default — arbitrary, meant to be overridden via `OPENROUTER_MODEL`
/// or [`OpenRouterProvider::new`]. Kept as a concrete model string (not OpenRouter's `"auto"`
/// routing) so a given run's model choice is reproducible, which matters for tuning/evaluation.
pub const DEFAULT_MODEL: &str = "openai/gpt-4o-mini";
/// OpenRouter's API base URL (overridable via [`OpenRouterProvider::with_base_url`]).
pub const DEFAULT_BASE_URL: &str = "https://openrouter.ai/api/v1";

/// An OpenRouter-backed provider.
pub struct OpenRouterProvider {
    client: reqwest::Client,
    api_key: String,
    model: String,
    base_url: String,
}

impl OpenRouterProvider {
    /// Build a provider with an explicit key + model (an OpenRouter model slug, e.g.
    /// `"anthropic/claude-3.5-haiku"` or `"deepseek/deepseek-chat"`).
    pub fn new(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            client: reqwest::Client::new(),
            api_key: api_key.into(),
            model: model.into(),
            base_url: DEFAULT_BASE_URL.to_string(),
        }
    }

    /// Build a provider from the `OPENROUTER_API_KEY` environment variable, using
    /// [`DEFAULT_MODEL`]. `OPENROUTER_MODEL` overrides the model if present.
    pub fn from_env() -> Result<Self, std::env::VarError> {
        let api_key = std::env::var("OPENROUTER_API_KEY")?;
        let model = std::env::var("OPENROUTER_MODEL").unwrap_or_else(|_| DEFAULT_MODEL.to_string());
        Ok(Self::new(api_key, model))
    }

    /// Override the API base URL (e.g. to point at a mock server in tests).
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }

    fn endpoint(&self) -> String {
        format!("{}/chat/completions", self.base_url.trim_end_matches('/'))
    }
}

#[async_trait]
impl Provider for OpenRouterProvider {
    fn model(&self) -> &str {
        &self.model
    }

    async fn complete(&self, request: CompletionRequest) -> ProviderResult<CompletionResponse> {
        let name_map = build_tool_name_map(&request.tools);
        let body = to_openai_request(&self.model, &request, &name_map);

        let response = self
            .client
            .post(self.endpoint())
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .await
            .map_err(|e| ProviderError::Transport(e.to_string()))?;

        let status = response.status();
        if !status.is_success() {
            let detail = response
                .text()
                .await
                .unwrap_or_else(|e| format!("<error body unavailable: {e}>"));
            return Err(map_status(status.as_u16(), detail));
        }

        let value: Value = response
            .json()
            .await
            .map_err(|e| ProviderError::Transport(format!("malformed response body: {e}")))?;
        from_openai_response(&value, &name_map)
    }

    async fn complete_stream(
        &self,
        request: CompletionRequest,
    ) -> ProviderResult<CompletionStream> {
        let name_map = build_tool_name_map(&request.tools);
        let mut body = to_openai_request(&self.model, &request, &name_map);
        body["stream"] = json!(true);

        let response = self
            .client
            .post(self.endpoint())
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .await
            .map_err(|e| ProviderError::Transport(e.to_string()))?;

        let status = response.status();
        if !status.is_success() {
            let detail = response
                .text()
                .await
                .unwrap_or_else(|e| format!("<error body unavailable: {e}>"));
            return Err(map_status(status.as_u16(), detail));
        }

        Ok(stream_sse_response(response, name_map))
    }
}

/// Map an HTTP error status to a typed [`ProviderError`] (Decision 13: callers branch on these).
/// `402` (insufficient OpenRouter account credits) is OpenRouter-specific; folded into the same
/// `InvalidRequest` bucket as the other client-error statuses rather than given its own
/// `ProviderError` variant — no caller in this system branches on "out of credits" differently
/// from any other rejected request today.
fn map_status(status: u16, body: String) -> ProviderError {
    match status {
        429 => ProviderError::RateLimited,
        400 | 401 | 402 | 403 | 404 | 422 => {
            ProviderError::InvalidRequest(format!("HTTP {status}: {body}"))
        }
        _ => ProviderError::Transport(format!("HTTP {status}: {body}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use liberado_provider::Message;

    #[test]
    fn status_mapping() {
        assert!(matches!(
            map_status(429, "x".into()),
            ProviderError::RateLimited
        ));
        assert!(matches!(
            map_status(401, "x".into()),
            ProviderError::InvalidRequest(_)
        ));
        assert!(matches!(
            map_status(402, "x".into()),
            ProviderError::InvalidRequest(_)
        ));
        assert!(matches!(
            map_status(500, "x".into()),
            ProviderError::Transport(_)
        ));
    }

    #[test]
    fn constructor_sets_fields() {
        let provider = OpenRouterProvider::new("sk-or-abc", "my-model");
        assert_eq!(provider.model, "my-model");
        assert_eq!(provider.api_key, "sk-or-abc");
        assert_eq!(provider.base_url, DEFAULT_BASE_URL);
    }

    #[test]
    fn with_base_url_overrides_default() {
        let provider = OpenRouterProvider::new("k", "m").with_base_url("http://localhost:8080");
        assert_eq!(provider.base_url, "http://localhost:8080");
    }

    #[test]
    fn endpoint_strips_trailing_slash() {
        let provider = OpenRouterProvider::new("k", "m").with_base_url("https://api.example.com/");
        assert_eq!(
            provider.endpoint(),
            "https://api.example.com/chat/completions"
        );
    }

    #[test]
    fn endpoint_without_trailing_slash() {
        let provider = OpenRouterProvider::new("k", "m").with_base_url("https://api.example.com");
        assert_eq!(
            provider.endpoint(),
            "https://api.example.com/chat/completions"
        );
    }

    #[test]
    fn from_env_uses_environment_variables() {
        let result = OpenRouterProvider::from_env();
        if std::env::var("OPENROUTER_API_KEY").is_ok() {
            let provider = result.expect("from_env should succeed when OPENROUTER_API_KEY is set");
            assert_eq!(
                provider.api_key,
                std::env::var("OPENROUTER_API_KEY").unwrap()
            );
        } else {
            assert!(
                result.is_err(),
                "from_env should fail when OPENROUTER_API_KEY is unset"
            );
        }
    }

    #[test]
    fn model_getter_returns_configured_model() {
        let provider = OpenRouterProvider::new("k", "custom-model-v2");
        assert_eq!(provider.model(), "custom-model-v2");
    }

    #[tokio::test]
    #[ignore = "requires OPENROUTER_API_KEY + network access"]
    async fn live_smoke() {
        let provider = OpenRouterProvider::from_env().expect("OPENROUTER_API_KEY not set");
        let resp = provider
            .complete(CompletionRequest::new(vec![Message::user(
                "Reply with exactly one word: pong",
            )]))
            .await
            .expect("live call failed");
        assert!(
            resp.content.is_some(),
            "expected text content from OpenRouter"
        );
    }
}
